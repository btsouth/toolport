//! The gateway path written into client entries.
use super::*;

#[test]
fn gateway_paths_are_written_without_dot_dot_and_old_spellings_stay_current() {
    assert_eq!(
        lexically_normalized(Path::new(
            "/Applications/Toolport.app/Contents/MacOS/../Helpers/ToolportGateway.app/Contents/MacOS/toolport-gateway"
        )),
        PathBuf::from(
            "/Applications/Toolport.app/Contents/Helpers/ToolportGateway.app/Contents/MacOS/toolport-gateway"
        )
    );
    assert_eq!(
        lexically_normalized(Path::new("/a/./b/../c")),
        PathBuf::from("/a/c")
    );
    assert_eq!(
        lexically_normalized(Path::new("/../a")),
        PathBuf::from("/a")
    );
    assert_eq!(
        lexically_normalized(Path::new("../a/b/..")),
        PathBuf::from("../a")
    );

    let root = std::env::temp_dir().join(format!("toolport-tidy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bundle = root.join("Toolport.app/Contents");
    let helper = bundle.join("Helpers/ToolportGateway.app/Contents/MacOS");
    std::fs::create_dir_all(&helper).unwrap();
    std::fs::create_dir_all(bundle.join("MacOS")).unwrap();
    std::fs::write(helper.join("toolport-gateway"), "gw").unwrap();
    let dotted = bundle
        .join("MacOS")
        .join("..")
        .join("Helpers/ToolportGateway.app/Contents/MacOS/toolport-gateway");
    assert_eq!(
        tidy_existing_path(dotted.clone()),
        helper.join("toolport-gateway")
    );
    #[cfg(unix)]
    {
        // Through a symlinked dir, `..` means the link target's parent: keep it.
        let elsewhere = root.join("elsewhere/inner");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(root.join("elsewhere/Helpers")).unwrap();
        std::fs::write(root.join("elsewhere/Helpers/gw"), "x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
        let through = root.join("link").join("..").join("Helpers/gw");
        assert_eq!(tidy_existing_path(through.clone()), through);
    }
    let stored = dotted.to_string_lossy().into_owned();
    let current = helper
        .join("toolport-gateway")
        .to_string_lossy()
        .into_owned();
    assert!(!gateway_command_is_stale(&stored, &current));
    assert!(command_is_gateway_binary(&stored));
    std::fs::remove_dir_all(&root).unwrap();
}
