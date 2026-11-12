//! Links clang's runtime library on macOS.
//!
//! whisper.cpp's Metal code asks which macOS it is running on
//! (`@available(macOS 15.0, ...)`), and that question compiles to a call
//! into clang's runtime library. clang links the library by itself; rustc
//! does not. Without it an Eddie built for a macOS older than 15, as
//! `packaging/macos/bundle.sh` builds it, does not link: "Undefined
//! symbols: ___isPlatformVersionAtLeast".

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let folder = Command::new("clang")
        .arg("--print-runtime-dir")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|folder| Path::new(folder).join("libclang_rt.osx.a").exists());
    match folder {
        Some(folder) => {
            println!("cargo:rustc-link-search=native={folder}");
            println!("cargo:rustc-link-lib=clang_rt.osx");
        }
        None => println!(
            "cargo:warning=clang's runtime library (libclang_rt.osx.a) was not found; \
             linking fails when MACOSX_DEPLOYMENT_TARGET is older than 15"
        ),
    }
}
