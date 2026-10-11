//! The Tauri bundler treats every entry in `src/bin` as a binary to ship, even
//! with `autobins = false`, and the Windows installer build fails when one has
//! no executable. Helper modules belong in `src/gateway`.

#[test]
fn src_bin_holds_only_declared_binaries() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let declared: Vec<&str> = manifest
        .lines()
        .filter_map(|line| line.trim().strip_prefix("path = \"src/bin/"))
        .filter_map(|rest| rest.strip_suffix('"'))
        .collect();
    let mut stray = Vec::new();
    for entry in std::fs::read_dir(root.join("src/bin")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if !declared.contains(&name.as_str()) {
            stray.push(name);
        }
    }
    assert!(
        stray.is_empty(),
        "src/bin entries without a [[bin]] in Cargo.toml: {stray:?}"
    );
}
