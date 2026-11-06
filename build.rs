//! Build-time help: ONNX Runtime (for stems) is C++ and links libstdc++.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if !linux {
        return;
    }
    // ONNX Runtime is C++, so linking it needs libstdc++. Most systems have
    // the library (everything written in C++ uses it) but only under its
    // versioned name; the plain name the linker asks for arrives with a C++
    // compiler. Where it is missing, point the linker at the one that is there.
    let found = Command::new("cc")
        .arg("-print-file-name=libstdc++.so")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().contains('/'))
        .unwrap_or(false);
    if found {
        return;
    }
    let Some(out) = std::env::var_os("OUT_DIR").map(PathBuf::from) else { return };
    for dir in ["/usr/lib64", "/lib64", "/usr/lib", "/usr/lib/aarch64-linux-gnu", "/usr/lib/x86_64-linux-gnu"] {
        let lib = Path::new(dir).join("libstdc++.so.6");
        if lib.exists() {
            let link = out.join("libstdc++.so");
            let _ = std::fs::remove_file(&link);
            if std::os::unix::fs::symlink(&lib, &link).is_ok() {
                println!("cargo:rustc-link-search=native={}", out.display());
            }
            return;
        }
    }
}
