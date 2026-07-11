//! Stage the built-in app library for embedding. `include_dir!` embeds a
//! directory as-is, but `default-workspace/apps/` on a dev machine can carry
//! local junk (`_build/` from a test compile, `data/` runtime state) that must
//! never ship inside the binary — so copy it into OUT_DIR with those filtered
//! out and embed the staged copy.

use std::fs;
use std::path::Path;

/// Never embedded: runtime state and build output. Vendored `deps/` ARE
/// embedded — a library app must install runnable with zero network.
const EXCLUDE: &[&str] = &["_build", "data", "node_modules", ".git"];

fn copy_filtered(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create staged catalog dir");
    for entry in fs::read_dir(src).expect("read default-workspace/apps") {
        let entry = entry.expect("read dir entry");
        let name = entry.file_name();
        if EXCLUDE.iter().any(|skip| name == *skip) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if entry.file_type().expect("file type").is_dir() {
            copy_filtered(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy catalog file");
        }
    }
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let src = Path::new(&manifest_dir).join("default-workspace/apps");
    let dst = Path::new(&out_dir).join("catalog");
    let _ = fs::remove_dir_all(&dst);
    copy_filtered(&src, &dst);
    println!("cargo:rerun-if-changed=default-workspace/apps");
}
