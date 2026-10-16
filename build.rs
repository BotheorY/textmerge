// SPDX-License-Identifier: MIT
use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let object = output.join("resources.o");
    let status = Command::new("windres")
        .args(["-I", "assets", "-i", "assets/textmerge.rc", "-o"])
        .arg(&object)
        .status()
        .expect("windres must be available in the Rust development shell");
    assert!(status.success(), "Resource compilation failed");
    println!("cargo:rustc-link-arg={}", object.display());
    println!("cargo:rustc-link-arg=-static-libgcc");
}
