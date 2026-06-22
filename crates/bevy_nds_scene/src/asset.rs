//! Runtime parser for the `.scene` blob produced host-side by `scene2bin`.
//!
//! Pure parsing: no FFI, no hardware, no Bevy — so it host-tests cleanly. The
//! on-disk layout (little-endian) is the inverse of `scene2bin::encode`; **keep
//! the two in sync** (the convention shared by `png2sprite::encode` ↔
//! `bevy_nds_sprite::asset::parse`, and `bevy_nds_3d_obj` ↔ `parse_dl_asset`).
//!
//! ```text
//! | offset | type        | field                                          |
//! |--------|-------------|------------------------------------------------|
//! | 0      | u32         | magic "BSC1"                                   |
//! | 4      | u16         | format version (currently 1)                   |
//! | 6      | u16         | camera mode (0 Follow/1 TopDown/2 Rail/3 Capt) |
//! | 8      | f32 × 4     | camera params (mode-specific)                  |
//! | 24     | u32         | instance count N                               |
//! |        | Instance ×N |                                                |
//! |        | u32         | exit count M                                   |
//! |        | Exit ×M     |                                                |
//!
//! Instance:
//!   str   mesh       (u16 len + UTF-8 bytes; len 0 ⇒ no mesh)
//!   str   role       (u16 len + UTF-8 bytes)
//!   f32×3 pos / f32×3 rot / f32×3 scale
//!   u8    has_material
//!   u8×3  diffuse / u8×3 ambient
//!   u32   flags
//!   u16   path_len   then f32×2 (x,z) × path_len  (ground-plane waypoints)
//!
//! Exit:
//!   str   target     (u16 len + UTF-8 bytes)  — name of the neighbour space
//!   f32×3 at         (spawn/transition point)
//!   u32   gate       (objective/gate id; 0 = always open)
//! ```
//!
//! A space carries more than it currently *uses* on purpose (issue #27's
//! holistic guard): `flags`/`gate`/`path`/the camera variants leave room for
//! objective types, enemy vuln-state, and gating without a format bump.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// ASCII `"BSC1"` — magic prefix of a baked `.scene` file. Matches
/// `scene2bin::ASSET_MAGIC`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"BSC1");
/// Current `.scene` format version.
pub const VERSION: u16 = 1;

/// Per-space authored camera (issue #23 / #27). No free player-driven camera;
/// the framing is chosen per space and the game's director reads this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CameraMode {
    /// Soft 3/4 follow for open arenas: camera `height` above and `dist` behind
    /// the avatar, fixed downward `pitch` (radians).
    Follow { height: f32, dist: f32, pitch: f32 },
    /// Straight-down tactical framing at `height`.
    TopDown { height: f32 },
    /// Side-on rail for 2.5D corridors (params as Follow; reserved — #27).
    Rail2_5D { height: f32, dist: f32, pitch: f32 },
    /// Capture-framing (reserved — #27).
    CaptureFraming,
}

impl CameraMode {
    fn from_wire(mode: u16, p: [f32; 4]) -> Option<Self> {
        Some(match mode {
            0 => CameraMode::Follow { height: p[0], dist: p[1], pitch: p[2] },
            1 => CameraMode::TopDown { height: p[0] },
            2 => CameraMode::Rail2_5D { height: p[0], dist: p[1], pitch: p[2] },
            3 => CameraMode::CaptureFraming,
            _ => return None,
        })
    }
}

/// One authored object in a space: a mesh reference + placement + an opaque
/// `role` tag the game maps onto its own components.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneInstanceData {
    /// Bare mesh name (e.g. `"teapot"` ⇒ `nitro:/teapot.dl`); `None` for a
    /// transform-only marker (a spawn point, a logical node).
    pub mesh: Option<String>,
    /// Game-defined semantic tag (`"avatar"`, `"enemy"`, `"landmark"`, …).
    pub role: String,
    pub pos: [f32; 3],
    pub rot: [f32; 3],
    pub scale: [f32; 3],
    /// `(diffuse, ambient)` for the lit material; `None` falls back to the
    /// `DsMaterial` default.
    pub material: Option<([u8; 3], [u8; 3])>,
    /// Opaque per-instance flags (objective bits, vuln-state, …; game-defined).
    pub flags: u32,
    /// Ground-plane (XZ) waypoints — an enemy patrol path, a rail, etc.
    pub path: Vec<[f32; 2]>,
}

/// A connection from this space to a neighbour in the level graph (#27).
#[derive(Clone, Debug, PartialEq)]
pub struct SceneExitData {
    /// Name of the neighbouring space (`scene2bin` validates it resolves).
    pub target: String,
    pub at: [f32; 3],
    /// Gate/objective id that must be satisfied to use the exit (0 = open).
    pub gate: u32,
}

/// A fully parsed space.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneData {
    pub camera: CameraMode,
    pub instances: Vec<SceneInstanceData>,
    pub exits: Vec<SceneExitData>,
}

/// Parse a `.scene` blob. Returns `None` on bad magic, an unknown version, a
/// truncated buffer, or an unknown camera mode — i.e. the loader degrades to
/// "no scene" rather than spawning garbage. Split out from any FFI so it
/// host-tests directly.
pub fn parse(bytes: &[u8]) -> Option<SceneData> {
    let mut r = Reader::new(bytes);
    if r.u32()? != MAGIC || r.u16()? != VERSION {
        return None;
    }
    let mode = r.u16()?;
    let cam_params = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    let camera = CameraMode::from_wire(mode, cam_params)?;

    let n = r.u32()? as usize;
    let mut instances = Vec::with_capacity(n.min(MAX_PREALLOC));
    for _ in 0..n {
        let mesh = r.string()?;
        let mesh = if mesh.is_empty() { None } else { Some(mesh) };
        let role = r.string()?;
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let rot = [r.f32()?, r.f32()?, r.f32()?];
        let scale = [r.f32()?, r.f32()?, r.f32()?];
        let has_material = r.u8()? != 0;
        let diffuse = [r.u8()?, r.u8()?, r.u8()?];
        let ambient = [r.u8()?, r.u8()?, r.u8()?];
        let material = has_material.then_some((diffuse, ambient));
        let flags = r.u32()?;
        let path_len = r.u16()? as usize;
        let mut path = Vec::with_capacity(path_len.min(MAX_PREALLOC));
        for _ in 0..path_len {
            path.push([r.f32()?, r.f32()?]);
        }
        instances.push(SceneInstanceData {
            mesh, role, pos, rot, scale, material, flags, path,
        });
    }

    let m = r.u32()? as usize;
    let mut exits = Vec::with_capacity(m.min(MAX_PREALLOC));
    for _ in 0..m {
        let target = r.string()?;
        let at = [r.f32()?, r.f32()?, r.f32()?];
        let gate = r.u32()?;
        exits.push(SceneExitData { target, at, gate });
    }

    Some(SceneData { camera, instances, exits })
}

/// Cap on count-driven `with_capacity` so a corrupt length can't request a
/// multi-GB allocation before the per-element reads fail.
const MAX_PREALLOC: usize = 256;

/// A bounds-checked little-endian cursor. Every read returns `None` past the
/// end, so `parse` propagates truncation with `?` instead of panicking.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.u32()?))
    }

    /// A `u16`-length-prefixed UTF-8 string. Invalid UTF-8 ⇒ `None`.
    fn string(&mut self) -> Option<String> {
        let len = self.u16()? as usize;
        let b = self.take(len)?;
        core::str::from_utf8(b).ok().map(String::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of `scene2bin::encode` for round-trip tests (the host baker is the
    /// authoritative writer; this just exercises `parse`).
    #[derive(Default)]
    struct Writer(Vec<u8>);
    impl Writer {
        fn u8(&mut self, v: u8) { self.0.push(v); }
        fn u16(&mut self, v: u16) { self.0.extend_from_slice(&v.to_le_bytes()); }
        fn u32(&mut self, v: u32) { self.0.extend_from_slice(&v.to_le_bytes()); }
        fn f32(&mut self, v: f32) { self.u32(v.to_bits()); }
        fn string(&mut self, s: &str) {
            self.u16(s.len() as u16);
            self.0.extend_from_slice(s.as_bytes());
        }
    }

    fn sample() -> SceneData {
        SceneData {
            camera: CameraMode::Follow { height: 1.7, dist: 2.0, pitch: -0.7 },
            instances: alloc::vec![
                SceneInstanceData {
                    mesh: Some(String::from("teapot")),
                    role: String::from("avatar"),
                    pos: [0.0, 0.0, 0.0],
                    rot: [-1.5708, 0.0, 0.0],
                    scale: [0.11, 0.11, 0.11],
                    material: Some(([110, 180, 235], [26, 40, 58])),
                    flags: 0,
                    path: alloc::vec![],
                },
                SceneInstanceData {
                    mesh: Some(String::from("cube")),
                    role: String::from("enemy"),
                    pos: [1.2, 0.0, 0.6],
                    rot: [0.0, 0.4, 0.0],
                    scale: [0.16, 0.16, 0.16],
                    material: None,
                    flags: 0x01,
                    path: alloc::vec![[1.2, 0.6], [1.2, -0.6]],
                },
            ],
            exits: alloc::vec![SceneExitData {
                target: String::from("corridor_b"),
                at: [2.0, 0.0, 0.0],
                gate: 0,
            }],
        }
    }

    fn encode(s: &SceneData) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(MAGIC);
        w.u16(VERSION);
        match s.camera {
            CameraMode::Follow { height, dist, pitch } => {
                w.u16(0);
                w.f32(height); w.f32(dist); w.f32(pitch); w.f32(0.0);
            }
            CameraMode::TopDown { height } => {
                w.u16(1);
                w.f32(height); w.f32(0.0); w.f32(0.0); w.f32(0.0);
            }
            CameraMode::Rail2_5D { height, dist, pitch } => {
                w.u16(2);
                w.f32(height); w.f32(dist); w.f32(pitch); w.f32(0.0);
            }
            CameraMode::CaptureFraming => {
                w.u16(3);
                w.f32(0.0); w.f32(0.0); w.f32(0.0); w.f32(0.0);
            }
        }
        w.u32(s.instances.len() as u32);
        for inst in &s.instances {
            w.string(inst.mesh.as_deref().unwrap_or(""));
            w.string(&inst.role);
            for v in inst.pos { w.f32(v); }
            for v in inst.rot { w.f32(v); }
            for v in inst.scale { w.f32(v); }
            let (has, d, a) = match inst.material {
                Some((d, a)) => (1, d, a),
                None => (0, [0; 3], [0; 3]),
            };
            w.u8(has);
            for v in d { w.u8(v); }
            for v in a { w.u8(v); }
            w.u32(inst.flags);
            w.u16(inst.path.len() as u16);
            for p in &inst.path { w.f32(p[0]); w.f32(p[1]); }
        }
        w.u32(s.exits.len() as u32);
        for e in &s.exits {
            w.string(&e.target);
            for v in e.at { w.f32(v); }
            w.u32(e.gate);
        }
        w.0
    }

    #[test]
    fn round_trips() {
        let scene = sample();
        let blob = encode(&scene);
        assert_eq!(parse(&blob), Some(scene));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut blob = encode(&sample());
        blob[0] ^= 0xFF;
        assert!(parse(&blob).is_none());
    }

    #[test]
    fn rejects_unknown_version() {
        let mut blob = encode(&sample());
        blob[4] = 0xFF; // bump version low byte
        assert!(parse(&blob).is_none());
    }

    #[test]
    fn rejects_truncation() {
        let blob = encode(&sample());
        for cut in 0..blob.len() {
            assert!(parse(&blob[..cut]).is_none(), "len {cut} should not parse");
        }
    }

    #[test]
    fn empty_mesh_becomes_none() {
        let scene = SceneData {
            camera: CameraMode::TopDown { height: 3.2 },
            instances: alloc::vec![SceneInstanceData {
                mesh: None,
                role: String::from("spawn"),
                pos: [0.0; 3],
                rot: [0.0; 3],
                scale: [1.0; 3],
                material: None,
                flags: 0,
                path: alloc::vec![],
            }],
            exits: alloc::vec![],
        };
        let parsed = parse(&encode(&scene)).unwrap();
        assert_eq!(parsed.instances[0].mesh, None);
        assert_eq!(parsed.camera, CameraMode::TopDown { height: 3.2 });
    }
}
