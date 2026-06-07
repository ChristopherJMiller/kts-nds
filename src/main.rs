//! Bevy on the Nintendo DS.
//!
//! A tiny demo that runs a real Bevy `App` + ECS schedule on DS hardware. A
//! marker entity with a [`Position`] component is moved by the D-pad each
//! frame; an ECS system renders it onto the libnds text console. This shows the
//! full Bevy ECS data-flow (resources, components, queries, systems, schedules)
//! executing on bare-metal ARM9 — the renderer is libnds rather than wgpu.

#![no_std]
#![no_main]

extern crate alloc;

mod libnds;

use core::ffi::c_int;
use core::ptr;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

// ---------------------------------------------------------------------------
// Bare-metal runtime glue: allocator, panic handler, critical-section impl.
// ---------------------------------------------------------------------------

/// Global allocator backed by newlib's heap (set up by the BlocksDS crt0).
struct NewlibAlloc;

unsafe impl core::alloc::GlobalAlloc for NewlibAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        // newlib guarantees 8-byte alignment; honour larger requests too.
        unsafe { libnds::memalign(layout.align().max(8), layout.size()) as *mut u8 }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: core::alloc::Layout) {
        unsafe { libnds::free(ptr as *mut core::ffi::c_void) }
    }
}

#[global_allocator]
static ALLOCATOR: NewlibAlloc = NewlibAlloc;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Best-effort: show that we panicked, then spin.
    unsafe {
        libnds::consoleClear();
        libnds::iprintf(c"PANIC".as_ptr());
    }
    let _ = info;
    loop {
        unsafe { libnds::swiWaitForVBlank() }
    }
}

/// DS interrupt master enable register (single 32-bit MMIO word).
const REG_IME: *mut u32 = 0x0400_0208 as *mut u32;

/// Single-core critical section: disable interrupts on acquire, restore on
/// release. This is what Bevy's `critical-section` feature builds upon.
struct DsCriticalSection;
critical_section::set_impl!(DsCriticalSection);

unsafe impl critical_section::Impl for DsCriticalSection {
    unsafe fn acquire() -> bool {
        unsafe {
            let was_enabled = ptr::read_volatile(REG_IME) & 1 != 0;
            ptr::write_volatile(REG_IME, 0);
            was_enabled
        }
    }

    unsafe fn release(was_enabled: bool) {
        if was_enabled {
            unsafe { ptr::write_volatile(REG_IME, 1) }
        }
    }
}

// ---------------------------------------------------------------------------
// Game: Bevy ECS world.
// ---------------------------------------------------------------------------

// Text console dimensions (libnds default font is 32x24 tiles).
const SCREEN_COLS: i32 = 32;
const SCREEN_ROWS: i32 = 24;

/// Buttons held this frame, refreshed from libnds before each `app.update()`.
#[derive(Resource, Default)]
struct Buttons(u32);

/// Grid position of the marker entity.
#[derive(Component)]
struct Position {
    x: i32,
    y: i32,
}

/// Marker tag component.
#[derive(Component)]
struct Player;

fn spawn_player(mut commands: Commands) {
    commands.spawn((
        Player,
        Position {
            x: SCREEN_COLS / 2,
            y: SCREEN_ROWS / 2,
        },
    ));
}

/// Move the player one cell per frame in the held direction, clamped to screen.
fn movement(buttons: Res<Buttons>, mut query: Query<&mut Position, With<Player>>) {
    let held = buttons.0;
    for mut pos in &mut query {
        if held & libnds::KEY_LEFT != 0 {
            pos.x -= 1;
        }
        if held & libnds::KEY_RIGHT != 0 {
            pos.x += 1;
        }
        if held & libnds::KEY_UP != 0 {
            pos.y -= 1;
        }
        if held & libnds::KEY_DOWN != 0 {
            pos.y += 1;
        }
        pos.x = pos.x.clamp(0, SCREEN_COLS - 1);
        pos.y = pos.y.clamp(1, SCREEN_ROWS - 1); // row 0 reserved for the title
    }
}

/// Render the world onto the libnds text console.
fn render(query: Query<&Position, With<Player>>) {
    unsafe {
        libnds::consoleClear();
        // Title on the top row.
        libnds::iprintf(c"  Bevy ECS on Nintendo DS".as_ptr());
        for pos in &query {
            // ANSI cursor move to (row, col) is 1-based, then draw the marker.
            libnds::iprintf(
                c"\x1b[%d;%dH@".as_ptr(),
                (pos.y + 1) as c_int,
                (pos.x + 1) as c_int,
            );
        }
        // Hint line at the bottom.
        libnds::iprintf(c"\x1b[24;0HD-pad: move the @".as_ptr());
    }
}

/// libnds calls into `main` from the BlocksDS crt0.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> c_int {
    unsafe { libnds::consoleDemoInit() };

    let mut app = App::new();
    app.insert_resource(Buttons::default())
        .add_systems(Startup, spawn_player)
        .add_systems(Update, (movement, render).chain());

    // Run Startup once and prime the world.
    app.finish();
    app.cleanup();
    app.update();

    loop {
        unsafe {
            libnds::swiWaitForVBlank();
            libnds::scanKeys();
            let held = libnds::keysHeld();
            app.world_mut().resource_mut::<Buttons>().0 = held;
        }
        app.update();
    }
}
