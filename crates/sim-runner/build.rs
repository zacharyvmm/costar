//! build.rs — Links the pre-built Zephyr library when ZEPHYR_BUILD_DIR is set.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=ZEPHYR_BASE");
    println!("cargo:rerun-if-env-changed=ZEPHYR_BUILD_DIR");

    let zephyr_build_dir = std::env::var("ZEPHYR_BUILD_DIR").unwrap_or_default();
    let zephyr_base = std::env::var("ZEPHYR_BASE").unwrap_or_default();

    // If ZEPHYR_BASE is set, the real Zephyr kernel is being compiled
    // from source via cc crate in sim-zephyr-port.  Don't link zephyr.elf.
    if !zephyr_base.is_empty() {
        println!("cargo:warning=ZEPHYR_BASE is set — using cc crate Zephyr build, skipping zephyr.elf link");
        println!("cargo:rustc-cfg=zephyr_cc_kernel");
        return;
    }

    if zephyr_build_dir.is_empty() {
        println!("cargo:warning=ZEPHYR_BUILD_DIR not set — Zephyr real integration disabled");
        return;
    }

    let zephyr_elf = format!("{}/zephyr/zephyr.elf", zephyr_build_dir);
    let metadata = std::fs::metadata(&zephyr_elf);

    match metadata {
        Ok(m) if m.is_file() => {
            // Localize the app's `main` symbol so it doesn't conflict with ours.
            // Zephyr's kernel calls the app's main via z_cstart, not as the ELF entry point.
            let localized = format!("{}/zephyr/zephyr_localized.o", zephyr_build_dir);
            let status = Command::new("objcopy")
                .args(["--localize-symbol=main", &zephyr_elf, &localized])
                .status();

            match status {
                Ok(s) if s.success() => {
                    println!("cargo:rerun-if-changed={}", zephyr_elf);
                    println!("cargo:rustc-link-arg={}", localized);
                    println!("cargo:rustc-cfg=zephyr_linked");
                    println!(
                        "cargo:warning=Linked Zephyr from {} (main localized)",
                        localized
                    );
                }
                _ => {
                    // objcopy failed — try without localization
                    println!(
                        "cargo:warning=objcopy failed, linking {} directly",
                        zephyr_elf
                    );
                    println!("cargo:rerun-if-changed={}", zephyr_elf);
                    println!("cargo:rustc-link-arg={}", zephyr_elf);
                    println!("cargo:rustc-cfg=zephyr_linked");
                    println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
                }
            }
        }
        _ => {
            println!(
                "cargo:warning=zephyr.elf not found at {} — build with 'west build -b native_sim/native/64' first",
                zephyr_elf
            );
        }
    }
}
