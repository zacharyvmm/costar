//! build.rs — Links the pre-built Zephyr library when ZEPHYR_BUILD_DIR is set.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=ZEPHYR_BASE");
    println!("cargo:rerun-if-env-changed=ZEPHYR_BUILD_DIR");

    let zephyr_build_dir = std::env::var("ZEPHYR_BUILD_DIR").unwrap_or_default();
    let zephyr_base = std::env::var("ZEPHYR_BASE").unwrap_or_default();

    // If ZEPHYR_BASE is set on supported hosts, the real Zephyr kernel is
    // being compiled from source via cc crate in sim-zephyr-port.  Don't link
    // zephyr.elf.  Only enable when the zephyr_real feature is active.
    if !zephyr_base.is_empty() {
        let has_feature = std::env::var("CARGO_FEATURE_ZEPHYR_REAL").is_ok();
        if !has_feature {
            println!(
                "cargo:warning=ZEPHYR_BASE is set but zephyr_real feature is not enabled; skipping"
            );
            return;
        }

        println!(
            "cargo:warning=ZEPHYR_BASE is set — using cc crate Zephyr build, skipping zephyr.elf link"
        );
        println!("cargo:rustc-cfg=zephyr_cc_kernel");

        // ── Ztest linker section aliases ─────────────────────────────
        // When ZEPHYR_APP=ztest, the ztest framework needs ELF section markers
        // that would normally come from Zephyr's custom linker script.  Our
        // cc-crate build provides them via ztest_sections.ld (INSERT AFTER .data).
        // The flag must be emitted from sim-runner's build.rs (not sim-zephyr-port's)
        // because cargo:rustc-link-arg from a library dependency is not reliably
        // forwarded to the final linker step in all cargo versions.
        let zephyr_app = std::env::var("ZEPHYR_APP").unwrap_or_default();
        if zephyr_app == "ztest" && cfg!(target_os = "linux") {
            // sim-zephyr-port's crate dir — the ld fragment lives there.
            // CARGO_MANIFEST_DIR points to sim-runner/, so we walk to the sibling.
            let ld_script = std::path::PathBuf::from(
                std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"),
            )
            .join("../sim-zephyr-port/c/ztest_sections.ld");
            println!("cargo:rustc-link-arg=-Wl,-T,{}", ld_script.display());
            println!(
                "cargo:warning=ztest mode: using linker section fragment {}",
                ld_script.display()
            );
        }

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
