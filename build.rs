//! Injects Nintendo DS link arguments using the BlocksDS install located via
//! the `$BLOCKSDS` environment variable (set by the Nix dev shell). Keeping
//! these out of the target JSON means the spec file path stays correct on Nix,
//! where BlocksDS lives in the store rather than at `/opt/wonderful`.

use std::env;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();

    // Only emit DS-specific link flags for the bare-metal ARM9 target.
    if !target.contains("nintendo-ds") {
        return;
    }

    let blocksds = match env::var("BLOCKSDS") {
        Ok(v) => v,
        Err(_) => {
            println!(
                "cargo:warning=BLOCKSDS is not set. Run `nix develop` first so the \
                 DS toolchain and libnds can be found."
            );
            return;
        }
    };

    // Select the correct multilib (crt0/libc/libgcc) for the ARM946E-S.
    println!("cargo:rustc-link-arg=-mthumb");
    println!("cargo:rustc-link-arg=-mcpu=arm946e-s+nofp");

    // BlocksDS ARM9 crt0 + linker script.
    println!("cargo:rustc-link-arg=-specs={blocksds}/sys/crts/ds_arm9.specs");

    // libnds (ARM9 build) and newlib C library. Grouped to resolve the
    // circular references between them.
    println!("cargo:rustc-link-search=native={blocksds}/libs/libnds/lib");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg=-lnds9");
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");

    println!("cargo:rerun-if-env-changed=BLOCKSDS");
}
