//! Passes build facts to `src/meta.rs` so run metadata describes the binary
//! that actually ran, not whatever toolchain is on the PATH at run time.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=BENCH_RUSTC_VERSION={version}");
    for (out, var) in [
        ("BENCH_TARGET", "TARGET"),
        ("BENCH_PROFILE", "PROFILE"),
        ("BENCH_OPT_LEVEL", "OPT_LEVEL"),
        ("BENCH_DEBUG", "DEBUG"),
    ] {
        println!("cargo:rustc-env={out}={}", std::env::var(var).unwrap_or_default());
    }
    // The ijson dependency line, verbatim, so its feature set is recorded.
    let manifest = std::fs::read_to_string("Cargo.toml").unwrap_or_default();
    let ijson = manifest.lines().find(|l| l.trim_start().starts_with("ijson")).unwrap_or("").trim();
    println!("cargo:rustc-env=BENCH_IJSON_DEP={ijson}");

    // Identify the ijson source this binary was BUILT from: the revision, and
    // a hash of the uncommitted diff (candidates are usually uncommitted).
    // Read at build time, because at run time the checkout may have moved on.
    let git = |args: &[&str]| -> String {
        Command::new("git")
            .args(["-C", "../ijson"])
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    println!("cargo:rustc-env=BENCH_IJSON_REV={}", git(&["rev-parse", "HEAD"]));
    let diff = git(&["diff", "HEAD"]);
    let diff_hash = if diff.is_empty() { "clean".to_string() } else { format!("{:016x}", fnv1a(diff.as_bytes())) };
    println!("cargo:rustc-env=BENCH_IJSON_DIFF={diff_hash}");
    let lock = std::fs::read("Cargo.lock").unwrap_or_default();
    println!("cargo:rustc-env=BENCH_LOCK_HASH={:016x}", fnv1a(&lock));
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=../ijson/src");
    println!("cargo:rerun-if-changed=../ijson/Cargo.toml");
}

/// FNV-1a, 64-bit. Enough to tell two diffs apart; not a security hash.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}
