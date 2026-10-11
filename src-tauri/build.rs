fn main() {
    // tauri-build emits its own rerun rules. Include Rust sources explicitly so
    // a desktop build cannot reuse the stamp of a prior executable.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    println!("cargo:rustc-env=TOOLPORT_BUILD_STAMP=TOOLPORT_BUILD_STAMP:{at}");
    if let Ok(triple) = std::env::var("TARGET") {
        println!("cargo:rustc-env=CONDUIT_TARGET_TRIPLE={triple}");
    }
    #[cfg(feature = "desktop")]
    tauri_build::build()
}
