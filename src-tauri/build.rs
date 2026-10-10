fn main() {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    println!("cargo:rustc-env=TOOLPORT_BUILD_STAMP=TOOLPORT_BUILD_STAMP:{at}");
    if let Ok(triple) = std::env::var("TARGET") {
        println!("cargo:rustc-env=CONDUIT_TARGET_TRIPLE={triple}");
    }
    #[cfg(feature = "desktop")]
    tauri_build::build()
}
