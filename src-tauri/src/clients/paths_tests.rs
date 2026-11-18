//! Where every client reads its MCP config, on every OS, and how Toolport moves a
//! connection out of a file a client stopped reading.
use super::*;
use serde_json::json;

fn mock_home(platform: Platform) -> PathBuf {
    match platform {
        Platform::Windows => PathBuf::from(r"C:\Users\alice"),
        Platform::MacOs => PathBuf::from("/Users/alice"),
        Platform::Linux => PathBuf::from("/home/alice"),
    }
}

fn no_env(_: &str) -> Option<std::ffi::OsString> {
    None
}

/// The OS's roaming config dir for `home` (`%APPDATA%`, Application Support, ~/.config).
fn roaming(home: &Path, platform: Platform) -> PathBuf {
    match platform {
        Platform::Windows => home.join("AppData").join("Roaming"),
        Platform::MacOs => home.join("Library").join("Application Support"),
        Platform::Linux => home.join(".config"),
    }
}

type Expect = fn(&Path, Platform) -> PathBuf;

/// Every client: its documented default config file on each OS (no env overrides,
/// nothing on disk) and the format Toolport writes into it.
const CLIENT_PATHS: &[(&str, &str, Expect)] = &[
    ("claude-desktop", "JsonMcpServers", |h, p| {
        roaming(h, p)
            .join("Claude")
            .join("claude_desktop_config.json")
    }),
    ("cursor", "JsonCursorMcpServers", |h, _| {
        h.join(".cursor/mcp.json")
    }),
    ("droid", "JsonDroidMcpServers", |h, _| {
        h.join(".factory/mcp.json")
    }),
    ("crush", "JsonMcp", |h, _| {
        h.join(".config/crush/crush.json")
    }),
    ("anythingllm", "JsonMcpServers", |h, p| {
        roaming(h, p).join("anythingllm-desktop/storage/plugins/anythingllm_mcp_servers.json")
    }),
    ("vscode", "JsonServers", |h, p| {
        roaming(h, p).join("Code").join("User").join("mcp.json")
    }),
    ("amp", "JsonAmpMcpServers", |h, _| {
        h.join(".config/amp/settings.json")
    }),
    ("windsurf", "JsonMcpServers", |h, p| match p {
        Platform::Windows => roaming(h, p).join("devin").join("mcp_config.json"),
        _ => h.join(".config/devin/mcp_config.json"),
    }),
    ("devin-cli", "JsonMcpServers", |h, p| match p {
        Platform::Windows => roaming(h, p).join("devin").join("mcp_config.json"),
        _ => h.join(".config/devin/mcp_config.json"),
    }),
    ("opencode", "JsonOpenCodeMcp", |h, _| {
        h.join(".config/opencode/opencode.json")
    }),
    ("kilo-code", "JsonOpenCodeMcp", |h, _| {
        h.join(".config/kilo/kilo.jsonc")
    }),
    ("grok", "TomlMcpServers", |h, _| h.join(".grok/config.toml")),
    ("codex", "TomlMcpServers", |h, _| {
        h.join(".codex/config.toml")
    }),
    ("github-copilot-cli", "JsonCopilotMcpServers", |h, _| {
        h.join(".copilot/mcp-config.json")
    }),
    ("antigravity", "JsonMcpServers", |h, _| {
        h.join(".gemini/config/mcp_config.json")
    }),
    ("claude-code", "JsonMcpServers", |h, _| {
        h.join(".claude.json")
    }),
    ("gemini-cli", "JsonMcpServers", |h, _| {
        h.join(".gemini/settings.json")
    }),
    ("qwen-code", "JsonQwenMcpServers", |h, _| {
        h.join(".qwen/settings.json")
    }),
    ("junie", "JsonMcpServers", |h, _| {
        h.join(".junie/mcp/mcp.json")
    }),
    ("cline", "JsonMcpServers", |h, _| {
        h.join(".cline/data/settings/cline_mcp_settings.json")
    }),
    ("roo-code", "JsonMcpServers", |h, p| {
        roaming(h, p)
            .join("Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json")
    }),
    ("warp", "JsonMcpServers", |h, _| h.join(".warp/.mcp.json")),
    ("amazon-q", "JsonMcpServers", |h, _| {
        h.join(".aws/amazonq/mcp.json")
    }),
    ("kiro", "JsonMcpServers", |h, _| {
        h.join(".kiro/settings/mcp.json")
    }),
    ("auggie", "JsonAuggieMcpServers", |h, _| {
        h.join(".augment/settings.json")
    }),
    ("kimi-code", "JsonKimiMcpServers", |h, _| {
        h.join(".kimi-code/mcp.json")
    }),
    ("deepseek-harness", "YamlDshPatch", |h, _| {
        h.join(".dsh/cordis.patch.yml")
    }),
    ("reasonix", "TomlReasonixPlugins", |h, p| match p {
        Platform::Windows => roaming(h, p).join("reasonix").join("config.toml"),
        _ => h.join(".reasonix/config.toml"),
    }),
    ("codewhale", "JsonCodewhaleServers", |h, _| {
        h.join(".codewhale/mcp.json")
    }),
    ("openclaw", "JsonOpenClawMcp", |h, _| {
        h.join(".openclaw/openclaw.json")
    }),
    ("openhands", "JsonOpenHandsMcpServers", |h, _| {
        h.join(".openhands/mcp.json")
    }),
    ("nanobot", "JsonNanobotMcp", |h, _| {
        h.join(".nanobot/config.json")
    }),
    ("zcode", "JsonZCodeMcp", |h, _| {
        h.join(".zcode/cli/config.json")
    }),
    ("command-code", "JsonMcpServers", |h, _| {
        h.join(".commandcode/mcp.json")
    }),
    ("workbuddy", "JsonMcpServers", |h, _| {
        h.join(".workbuddy/mcp.json")
    }),
    ("workbuddy-ai", "JsonMcpServers", |h, _| {
        h.join(".workbuddy-ai/mcp.json")
    }),
    ("tabnine", "JsonMcpServers", |h, _| {
        h.join(".tabnine/mcp_servers.json")
    }),
    ("tabnine-cli", "JsonMcpServers", |h, _| {
        h.join(".tabnine/agent/settings.json")
    }),
    ("zed", "JsonContextServers", |h, p| match p {
        Platform::Windows => roaming(h, p).join("Zed").join("settings.json"),
        _ => h.join(".config/zed/settings.json"),
    }),
    ("lm-studio", "JsonMcpServers", |h, _| {
        h.join(".lmstudio/mcp.json")
    }),
    ("jan", "JsonMcpServers", |h, p| match p {
        Platform::Linux => h.join(".local/share/Jan/data/mcp_config.json"),
        _ => roaming(h, p)
            .join("Jan")
            .join("data")
            .join("mcp_config.json"),
    }),
    ("boltai", "JsonMcpServers", |h, _| {
        h.join(".boltai/mcp.json")
    }),
    ("pi", "JsonMcpServers", |h, _| h.join(".pi/agent/mcp.json")),
    ("omp", "JsonMcpServers", |h, _| {
        h.join(".omp/agent/mcp.json")
    }),
    ("goose", "YamlExtensions", |h, p| match p {
        Platform::Windows => roaming(h, p).join("Block/goose/config/config.yaml"),
        _ => h.join(".config/goose/config.yaml"),
    }),
    ("hermes", "YamlMcpServers", |h, p| match p {
        Platform::Windows => h
            .join("AppData")
            .join("Local")
            .join("hermes")
            .join("config.yaml"),
        _ => h.join(".hermes/config.yaml"),
    }),
    ("continue", "YamlMcpServersList", |h, _| {
        h.join(".continue/config.yaml")
    }),
    ("witsy", "JsonMcpServers", |h, p| {
        roaming(h, p).join("Witsy").join("settings.json")
    }),
];

/// Compare paths by components, so `a/b` and `a\b` spellings of one mock path agree.
fn same(a: &Path, b: &Path) -> bool {
    let parts = |p: &Path| {
        p.to_string_lossy()
            .replace('\\', "/")
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    parts(a) == parts(b)
}

#[test]
fn every_client_has_a_documented_path_and_format_on_every_os() {
    let ids: Vec<_> = defs().iter().map(|def| def.id).collect();
    let listed: Vec<_> = CLIENT_PATHS.iter().map(|(id, _, _)| *id).collect();
    assert_eq!(
        ids, listed,
        "CLIENT_PATHS must list every client in defs() order"
    );
    for (id, format, expect) in CLIENT_PATHS {
        let def = find_def(id).unwrap();
        assert_eq!(
            serde_json::to_value(def.format).unwrap(),
            json!(format),
            "{id} format"
        );
        for platform in Platform::ALL {
            let home = mock_home(platform);
            let table = resolve_client_config_path(id, &home, platform)
                .unwrap_or_else(|| panic!("{id} has no {platform:?} path"));
            let with_env = resolve_client_config_path_env(id, &home, platform, &no_env).unwrap();
            let expected = expect(&home, platform);
            assert!(
                same(&with_env, &expected),
                "{id} on {platform:?}: {with_env:?}"
            );
            // The env-free table agrees, except where Linux reads XDG through `dirs`.
            if platform != Platform::Linux {
                assert!(
                    same(&table, &expected),
                    "{id} table on {platform:?}: {table:?}"
                );
            }
        }
    }
}

#[test]
fn goose_and_devin_follow_xdg_config_home_on_macos_and_linux() {
    let xdg = std::env::temp_dir().join("toolport-xdg-paths");
    let env = |key: &str| (key == "XDG_CONFIG_HOME").then(|| xdg.clone().into_os_string());
    let relative = |key: &str| (key == "XDG_CONFIG_HOME").then(|| "relative/config".into());
    for platform in [Platform::MacOs, Platform::Linux] {
        let home = mock_home(platform);
        for (id, file) in [
            ("goose", "goose/config.yaml"),
            ("windsurf", "devin/mcp_config.json"),
            ("devin-cli", "devin/mcp_config.json"),
        ] {
            let path = resolve_client_config_path_env(id, &home, platform, &env).unwrap();
            assert!(same(&path, &xdg.join(file)), "{id} on {platform:?}");
            // etcetera and `dirs` ignore a relative XDG_CONFIG_HOME.
            let path = resolve_client_config_path_env(id, &home, platform, &relative).unwrap();
            assert!(
                same(&path, &home.join(".config").join(file)),
                "{id} {platform:?}"
            );
        }
    }
    // Windows keeps Goose and Devin under %APPDATA%.
    let home = mock_home(Platform::Windows);
    let goose = resolve_client_config_path_env("goose", &home, Platform::Windows, &env).unwrap();
    assert!(same(
        &goose,
        &roaming(&home, Platform::Windows).join("Block/goose/config/config.yaml")
    ));
}

#[test]
fn every_moved_location_is_listed_as_legacy_on_the_right_os() {
    for platform in Platform::ALL {
        let home = mock_home(platform);
        let legacy = |id| relocate::resolve_legacy_config_paths(id, &home, platform, &no_env);
        let goose = legacy("goose");
        if platform == Platform::MacOs {
            assert_eq!(
                goose,
                vec![home.join("Library/Application Support/Block/goose/config.yaml")]
            );
        } else {
            assert!(goose.is_empty(), "{platform:?}");
        }
        assert_eq!(
            legacy("windsurf"),
            vec![home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json")]
        );
        let cline_config = match platform {
            Platform::Linux => home.join(".config"),
            _ => roaming(&home, platform),
        };
        assert_eq!(
            legacy("cline"),
            vec![cline_config
                .join("Code")
                .join("User")
                .join("globalStorage")
                .join("saoudrizwan.claude-dev")
                .join("settings")
                .join("cline_mcp_settings.json")]
        );
        let crush = legacy("crush");
        let hermes = legacy("hermes");
        if platform == Platform::Windows {
            assert_eq!(
                crush,
                vec![home
                    .join("AppData")
                    .join("Local")
                    .join("crush")
                    .join("crush.json")]
            );
            assert_eq!(hermes, vec![home.join(".hermes").join("config.yaml")]);
        } else {
            assert!(crush.is_empty() && hermes.is_empty(), "{platform:?}");
        }
        // Which file the Store build reads when both exist is unverified: no move.
        assert!(legacy("claude-desktop").is_empty(), "{platform:?}");
        assert_eq!(
            legacy("antigravity"),
            vec![
                home.join(".gemini/antigravity-ide/mcp_config.json"),
                home.join(".gemini/antigravity/mcp_config.json"),
            ]
        );
        assert_eq!(legacy("amazon-q"), vec![home.join(".aws/amazonq/mcp.json")]);
        assert_eq!(
            legacy("lm-studio"),
            vec![
                home.join(".lmstudio/mcp.json"),
                home.join(".cache/lm-studio/mcp.json")
            ]
        );
        // Env-chosen locations are per shell, so their defaults are never legacy.
        for id in [
            "devin-cli",
            "opencode",
            "kilo-code",
            "pi",
            "omp",
            "openclaw",
            "vscode",
        ] {
            assert!(legacy(id).is_empty(), "{id} on {platform:?}");
        }
    }
    let reasonix = |platform| {
        relocate::resolve_legacy_config_paths("reasonix", &mock_home(platform), platform, &no_env)
    };
    let home = mock_home(Platform::MacOs);
    assert_eq!(
        reasonix(Platform::MacOs),
        vec![
            home.join("Library/Application Support/reasonix/config.toml"),
            home.join(".config/reasonix/config.toml"),
        ]
    );
    let home = mock_home(Platform::Linux);
    assert_eq!(
        reasonix(Platform::Linux),
        vec![home.join(".config/reasonix/config.toml")]
    );
    // On Windows the OS config dir IS Reasonix's home, so nothing is legacy there.
    assert!(reasonix(Platform::Windows).is_empty());
    let set = |key: &str| (key == "REASONIX_HOME").then(|| "/elsewhere".into());
    assert!(relocate::resolve_legacy_config_paths(
        "reasonix",
        &mock_home(Platform::Linux),
        Platform::Linux,
        &set
    )
    .is_empty());
}

#[test]
fn cline_reads_its_overrides_and_the_old_file_until_the_new_one_exists() {
    let root = std::env::temp_dir().join(format!("toolport-cline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let current = home.join(".cline/data/settings/cline_mcp_settings.json");
    let platform = Platform::Linux;
    assert_eq!(resolve_cline_path(&home, platform, &no_env), current);
    // Only the old extension storage exists: an older Cline reads that file, and
    // 4.1 merges it on first start.
    let legacy = cline_legacy_settings_path(&home, platform, &no_env);
    std::fs::create_dir_all(legacy.parent().unwrap().parent().unwrap()).unwrap();
    assert_eq!(resolve_cline_path(&home, platform, &no_env), legacy);
    // A ~/.cline folder alone (the CLI, shared rules) is not evidence (S1).
    std::fs::create_dir_all(home.join(".cline").join("rules")).unwrap();
    assert_eq!(resolve_cline_path(&home, platform, &no_env), legacy);
    // Once the new settings file exists, it is the target.
    std::fs::create_dir_all(current.parent().unwrap()).unwrap();
    std::fs::write(&current, "{}").unwrap();
    assert_eq!(resolve_cline_path(&home, platform, &no_env), current);
    let file = root.join("custom.json");
    let data = root.join("data");
    let dir = root.join("cline-dir");
    let padded = format!(" {} ", file.display());
    let env = |key: &str| match key {
        "CLINE_MCP_SETTINGS_PATH" => Some(padded.clone().into()),
        "CLINE_DATA_DIR" => Some(data.clone().into_os_string()),
        _ => None,
    };
    assert_eq!(resolve_cline_path(&home, platform, &env), file);
    let env = |key: &str| match key {
        "CLINE_DATA_DIR" => Some(data.clone().into_os_string()),
        "CLINE_DIR" => Some(dir.clone().into_os_string()),
        _ => None,
    };
    assert_eq!(
        resolve_cline_path(&home, platform, &env),
        data.join("settings/cline_mcp_settings.json")
    );
    let env = |key: &str| (key == "CLINE_DIR").then(|| dir.clone().into_os_string());
    assert_eq!(
        resolve_cline_path(&home, platform, &env),
        dir.join("data/settings/cline_mcp_settings.json")
    );
    // A relative override is Cline's cwd; the default stays.
    let env = |key: &str| (key == "CLINE_MCP_SETTINGS_PATH").then(|| "rel.json".into());
    assert_eq!(resolve_cline_path(&home, platform, &env), current);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn pi_and_oh_my_pi_follow_their_agent_dir_and_profile_vars() {
    let home = std::env::temp_dir().join("toolport-pi-home");
    let dir = std::env::temp_dir().join("toolport-pi-agent");
    assert_eq!(pi_agent_dir(&home, &no_env), home.join(".pi/agent"));
    let env = |key: &str| (key == "PI_CODING_AGENT_DIR").then(|| dir.clone().into_os_string());
    assert_eq!(pi_agent_dir(&home, &env), dir);
    let tilde = |key: &str| (key == "PI_CODING_AGENT_DIR").then(|| "~/agents/pi".into());
    assert_eq!(pi_agent_dir(&home, &tilde), home.join("agents/pi"));
    let relative = |key: &str| (key == "PI_CODING_AGENT_DIR").then(|| "agents".into());
    assert_eq!(pi_agent_dir(&home, &relative), home.join(".pi/agent"));

    assert_eq!(omp_agent_dir(&home, &no_env), home.join(".omp/agent"));
    assert_eq!(omp_agent_dir(&home, &env), dir);
    let profile = |key: &str| match key {
        "OMP_PROFILE" => Some(" work ".into()),
        "PI_CODING_AGENT_DIR" => Some(dir.clone().into_os_string()),
        _ => None,
    };
    assert_eq!(
        omp_agent_dir(&home, &profile),
        home.join(".omp/profiles/work/agent")
    );
    // OMP_PROFILE, even empty, wins over PI_PROFILE; `default` means none.
    let shadowed = |key: &str| match key {
        "OMP_PROFILE" => Some("".into()),
        "PI_PROFILE" => Some("other".into()),
        _ => None,
    };
    assert_eq!(omp_agent_dir(&home, &shadowed), home.join(".omp/agent"));
    let pi_profile = |key: &str| (key == "PI_PROFILE").then(|| "other".into());
    assert_eq!(
        omp_agent_dir(&home, &pi_profile),
        home.join(".omp/profiles/other/agent")
    );
    let default = |key: &str| (key == "OMP_PROFILE").then(|| "default".into());
    assert_eq!(omp_agent_dir(&home, &default), home.join(".omp/agent"));
    let named = |key: &str| (key == "PI_CONFIG_DIR").then(|| ".omp-alt".into());
    assert_eq!(omp_agent_dir(&home, &named), home.join(".omp-alt/agent"));
}

#[test]
fn opencode_and_kilo_follow_xdg_and_their_config_vars() {
    let root = std::env::temp_dir().join(format!("toolport-oc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let xdg = root.join("xdg");
    assert_eq!(
        resolve_opencode_path(&home, &no_env),
        home.join(".config/opencode/opencode.json")
    );
    let env = |key: &str| (key == "XDG_CONFIG_HOME").then(|| xdg.clone().into_os_string());
    assert_eq!(
        resolve_opencode_path(&home, &env),
        xdg.join("opencode/opencode.json")
    );
    let dir = root.join("oc-dir");
    let file = root.join("explicit.json");
    let env = |key: &str| match key {
        "XDG_CONFIG_HOME" => Some(xdg.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(dir.clone().into_os_string()),
        _ => None,
    };
    assert_eq!(
        resolve_opencode_path(&home, &env),
        dir.join("opencode.json")
    );
    let explicit = |key: &str| (key == "OPENCODE_CONFIG").then(|| file.clone().into_os_string());
    assert_eq!(resolve_opencode_path(&home, &explicit), file);

    let kilo = home.join(".config/kilo");
    assert_eq!(resolve_kilo_path(&home, &no_env), kilo.join("kilo.jsonc"));
    std::fs::create_dir_all(&kilo).unwrap();
    std::fs::write(kilo.join("opencode.json"), "{}").unwrap();
    assert_eq!(
        resolve_kilo_path(&home, &no_env),
        kilo.join("opencode.json")
    );
    std::fs::write(kilo.join("kilo.json"), "{}").unwrap();
    assert_eq!(resolve_kilo_path(&home, &no_env), kilo.join("kilo.json"));
    let env = |key: &str| (key == "XDG_CONFIG_HOME").then(|| xdg.clone().into_os_string());
    assert_eq!(resolve_kilo_path(&home, &env), xdg.join("kilo/kilo.jsonc"));
    let env = |key: &str| (key == "KILO_CONFIG_DIR").then(|| dir.clone().into_os_string());
    assert_eq!(resolve_kilo_path(&home, &env), dir.join("kilo.jsonc"));
    let env = |key: &str| (key == "KILO_CONFIG").then(|| file.clone().into_os_string());
    assert_eq!(resolve_kilo_path(&home, &env), file);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn lm_studio_amazon_q_amp_and_claude_desktop_pick_the_file_their_client_reads() {
    let root = std::env::temp_dir().join(format!("toolport-alt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(resolve_lm_studio_home(&home), home.join(".lmstudio"));
    std::fs::create_dir_all(home.join(".cache/lm-studio")).unwrap();
    assert_eq!(resolve_lm_studio_home(&home), home.join(".cache/lm-studio"));
    let moved = root.join("models-disk");
    std::fs::write(
        home.join(".lmstudio-home-pointer"),
        format!("{}\n", moved.display()),
    )
    .unwrap();
    assert_eq!(resolve_lm_studio_home(&home), moved);

    let q = home.join(".aws/amazonq");
    assert_eq!(resolve_amazon_q_path(&home), q.join("mcp.json"));
    std::fs::create_dir_all(&q).unwrap();
    // The IDE's agent file is agents/default.json; ~/.aws/amazonq/default.json is
    // read by nothing.
    std::fs::create_dir_all(q.join("agents")).unwrap();
    std::fs::write(q.join("agents/default.json"), r#"{"name":"default"}"#).unwrap();
    assert_eq!(resolve_amazon_q_path(&home), q.join("mcp.json"));
    std::fs::write(q.join("default.json"), r#"{"useLegacyMcpJson": false}"#).unwrap();
    assert_eq!(resolve_amazon_q_path(&home), q.join("mcp.json"));
    std::fs::write(
        q.join("agents/default.json"),
        r#"{"useLegacyMcpJson": false}"#,
    )
    .unwrap();
    assert_eq!(
        resolve_amazon_q_path(&home),
        q.join("agents").join("default.json")
    );

    let amp = home.join(".config/amp/settings.json");
    assert_eq!(prefer_existing_jsonc(amp.clone()), amp);
    std::fs::create_dir_all(amp.parent().unwrap()).unwrap();
    std::fs::write(amp.with_extension("jsonc"), "{}").unwrap();
    assert_eq!(
        prefer_existing_jsonc(amp.clone()),
        amp.with_extension("jsonc")
    );
    std::fs::write(&amp, "{}").unwrap();
    assert_eq!(prefer_existing_jsonc(amp.clone()), amp);

    let local = home.join("AppData/Local");
    let real = home.join("AppData/Roaming/Claude/claude_desktop_config.json");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "{}").unwrap();
    assert_eq!(resolve_claude_desktop_path(real.clone(), &local), real);
    let msix = local.join("Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude");
    std::fs::create_dir_all(&msix).unwrap();
    // An installed package without its own copy still reads the real file.
    assert_eq!(resolve_claude_desktop_path(real.clone(), &local), real);
    std::fs::write(msix.join("claude_desktop_config.json"), "{}").unwrap();
    // With both, the real file stays the target (precedence is unverified).
    assert_eq!(resolve_claude_desktop_path(real.clone(), &local), real);
    // A package copy alone is the one read.
    std::fs::remove_file(&real).unwrap();
    assert_eq!(
        resolve_claude_desktop_path(real.clone(), &local),
        msix.join("claude_desktop_config.json")
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// Isolated home, data dir and client env for one end-to-end test.
pub(super) struct Fixture {
    pub(super) home: PathBuf,
    gateway: PathBuf,
    _home: TestHome,
    _env: Vec<EnvRestore>,
    _lock: std::sync::MutexGuard<'static, ()>,
    pub(super) data: crate::registry::DataDirTestEnv,
}

impl Fixture {
    pub(super) fn new(label: &str) -> Self {
        let data = crate::registry::DataDirTestEnv::new(label);
        let lock = env_test_lock();
        let env = isolated_client_env(&data.dir);
        let home = data.dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let test_home = TestHome::set(&home);
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        let gateway = data.dir.join("bin").join("toolport-gateway-9.9.9");
        std::fs::create_dir_all(gateway.parent().unwrap()).unwrap();
        std::fs::write(&gateway, "fixture").unwrap();
        Self {
            home,
            gateway,
            _home: test_home,
            _env: env,
            _lock: lock,
            data,
        }
    }

    pub(super) fn gateway(&self) -> &str {
        self.gateway.to_str().unwrap()
    }
}

pub(super) fn write(path: &Path, value: &serde_json::Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

pub(super) fn read(path: &Path) -> serde_json::Value {
    parse_json_value(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn old_entry(client: &str) -> serde_json::Value {
    json!({
        "command": "/Applications/Toolport.app/Contents/MacOS/toolport-gateway",
        "args": [],
        "env": {"TOOLPORT_CLIENT_ID": client, "TOOLPORT_PROFILE": "work"}
    })
}

/// A JSON client's connection reaches the current file with its scope kept. The old
/// entry leaves the old file only where there is evidence the client no longer reads
/// it; otherwise it stays, pointed at the current gateway. Nothing else changes.
#[test]
fn json_connections_reach_the_current_file_and_leave_old_ones_only_on_evidence() {
    type Case = (&'static str, fn(&Path) -> PathBuf, fn(&Path), bool);
    let cases: &[Case] = &[
        // Devin Desktop still imports the Windsurf file by default.
        (
            "windsurf",
            |h| h.join(".codeium/windsurf/mcp_config.json"),
            |_| {},
            false,
        ),
        // Antigravity 1.x read these; no vendor statement that it stopped.
        (
            "antigravity",
            |h| h.join(".gemini/antigravity-ide/mcp_config.json"),
            |_| {},
            false,
        ),
        (
            "antigravity",
            |h| h.join(".gemini/antigravity/mcp_config.json"),
            |_| {},
            false,
        ),
        // R3-1: Cline's old entry is never removed. Cline 4 recorded its one-time
        // migration of the storage file, but a rollback to Cline 3 reads it again.
        (
            "cline",
            |h| cline_legacy_settings_path(h, Platform::current(), &client_path_env),
            |h| {
                write(
                    &h.join(".cline/data/globalState.json"),
                    &json!({"__vscodeLegacyMcpSettingsMigration": {"done": true}}),
                )
            },
            false,
        ),
        // A rollback to Cline 3 with the Cline 4 folder still present: the newest
        // folder is 4 and the record stays, yet Cline 3 reads the old file.
        (
            "cline",
            |h| cline_legacy_settings_path(h, Platform::current(), &client_path_env),
            |h| {
                write(
                    &h.join(".cline/data/globalState.json"),
                    &json!({"__vscodeLegacyMcpSettingsMigration": {"done": true}}),
                );
                for version in ["3.89.2", "4.1.26"] {
                    std::fs::create_dir_all(h.join(format!(
                        ".vscode/extensions/saoudrizwan.claude-dev-{version}"
                    )))
                    .unwrap()
                }
            },
            false,
        ),
        // R2-3: a downloaded Cline 4 folder guides the write but proves nothing about
        // which file the running extension reads, so the old entry stays.
        (
            "cline",
            |h| cline_legacy_settings_path(h, Platform::current(), &client_path_env),
            |h| {
                std::fs::create_dir_all(h.join(".vscode/extensions/saoudrizwan.claude-dev-4.1.26"))
                    .unwrap()
            },
            false,
        ),
        // Amazon Q CLI and IDEs with legacy reading on still load mcp.json.
        (
            "amazon-q",
            |h| h.join(".aws/amazonq/mcp.json"),
            |h| {
                write(
                    &h.join(".aws/amazonq/agents/default.json"),
                    &json!({"useLegacyMcpJson": false}),
                )
            },
            false,
        ),
        // Without a home pointer LM Studio may still pick the old home.
        (
            "lm-studio",
            |h| h.join(".lmstudio/mcp.json"),
            |h| std::fs::create_dir_all(h.join(".cache/lm-studio")).unwrap(),
            false,
        ),
        // The pointer names the home LM Studio reads.
        (
            "lm-studio",
            |h| h.join(".lmstudio/mcp.json"),
            |h| {
                let cache = h.join(".cache/lm-studio");
                std::fs::create_dir_all(&cache).unwrap();
                std::fs::write(h.join(".lmstudio-home-pointer"), cache.to_str().unwrap()).unwrap();
            },
            // The app may predate the pointer lmstudio-js writes.
            false,
        ),
    ];
    for (client, legacy_of, prepare, moves) in cases {
        let fixture = Fixture::new("relocate-json");
        prepare(&fixture.home);
        let legacy = legacy_of(&fixture.home);
        write(
            &legacy,
            &json!({"mcpServers": {"toolport": old_entry(client), "native": {"command": "native"}}}),
        );
        let target = resolved_definition_path(&find_def(client).unwrap()).unwrap();
        assert_ne!(target, legacy, "{client}");

        let moved = relocate::relocate(client, fixture.gateway(), None).unwrap();
        let managed = moved.managed.expect("ownership of the moved entry");
        assert_eq!(managed.command, fixture.gateway());
        let now = read(&target);
        let entry = &now["mcpServers"]["toolport"];
        assert_eq!(entry["command"], fixture.gateway(), "{client}");
        assert_eq!(entry["env"]["TOOLPORT_CLIENT_ID"], *client, "{client}");
        assert_eq!(entry["env"]["TOOLPORT_PROFILE"], "work", "{client}");
        let old = read(&legacy);
        if *moves {
            assert_eq!(moved.cleaned, vec![legacy.clone()], "{client}");
            assert!(
                old["mcpServers"].get("toolport").is_none(),
                "{client}: {old}"
            );
        } else {
            assert_eq!(moved.kept, vec![legacy.clone()], "{client}");
            assert_eq!(
                old["mcpServers"]["toolport"]["command"],
                fixture.gateway(),
                "{client}"
            );
            assert_eq!(
                old["mcpServers"]["toolport"]["env"]["TOOLPORT_PROFILE"],
                "work"
            );
        }
        assert_eq!(old["mcpServers"]["native"], json!({"command": "native"}));

        // The pass is idempotent, and the client shows as one connected row.
        let again = relocate::relocate(client, fixture.gateway(), None).unwrap();
        assert!(
            again.cleaned.is_empty() && again.managed.is_none(),
            "{client}"
        );
        let row = read_client(&find_def(client).unwrap());
        assert!(row.gateway_installed, "{client}");
        drop(fixture);
    }
}

/// The current file already holds a different connection (Devin CLI's, unscoped)
/// while the old Windsurf file holds a `work`-scoped one: removing the old entry
/// would change what the client is connected to, so it stays, even when the client
/// is known not to read the old file.
#[test]
fn a_different_connection_in_the_current_file_never_removes_the_old_one() {
    for verdict in [
        relocate::OldFile::MaybeRead,
        relocate::OldFile::Unread("test"),
    ] {
        let fixture = Fixture::new("relocate-existing");
        let legacy = fixture
            .home
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json");
        write(
            &legacy,
            &json!({"mcpServers": {"toolport": old_entry("windsurf")}}),
        );
        let target = client_config_path("devin-cli").unwrap();
        let current = json!({"mcpServers": {"toolport": {
            "command": fixture.gateway(), "args": [],
            "env": {"TOOLPORT_CLIENT_ID": "devin-cli", "TOOLPORT_ATTRIBUTION_ID": "devin-cli"}}}});
        write(&target, &current);
        relocate::STATUS_OVERRIDE.with(|cell| cell.set(Some(verdict)));
        let moved = relocate::relocate("windsurf", fixture.gateway(), None);
        relocate::STATUS_OVERRIDE.with(|cell| cell.set(None));
        let moved = moved.unwrap();
        assert!(moved.managed.is_none());
        assert!(moved.cleaned.is_empty(), "{verdict:?}");
        assert_eq!(moved.kept, vec![legacy.clone()]);
        // Devin CLI's connection in the shared file is not rewritten by Devin Desktop.
        assert_eq!(read(&target), current);
        let old = &read(&legacy)["mcpServers"]["toolport"];
        assert_eq!(old["env"]["TOOLPORT_PROFILE"], "work", "{verdict:?}");
        assert_eq!(
            old["command"],
            fixture.gateway(),
            "kept entries follow the gateway"
        );
    }
}

/// S2: Windsurf still reads `~/.codeium/windsurf/mcp_config.json` through
/// `chat.mcp.discovery.enabled`, and older builds read only that file. The startup
/// pass writes the Devin file and keeps the old entry working.
#[test]
fn startup_keeps_an_old_entry_the_client_may_still_read() {
    let fixture = Fixture::new("relocate-keep");
    let legacy = fixture
        .home
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    write(
        &legacy,
        &json!({"mcpServers": {"toolport": old_entry("windsurf"), "native": {"command": "native"}}}),
    );
    let outcome = relocate::relocate_all(fixture.gateway(), &HashMap::new());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let target = client_config_path("windsurf").unwrap();
    assert_eq!(
        read(&target)["mcpServers"]["toolport"]["env"]["TOOLPORT_PROFILE"],
        "work"
    );
    let old = read(&legacy);
    assert_eq!(old["mcpServers"]["toolport"]["command"], fixture.gateway());
    assert_eq!(
        old["mcpServers"]["toolport"]["env"]["TOOLPORT_PROFILE"],
        "work"
    );
    assert_eq!(old["mcpServers"]["native"], json!({"command": "native"}));
    // One client row, connected.
    let rows: Vec<_> = detect_clients()
        .into_iter()
        .filter(|row| row.id == "windsurf")
        .collect();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].gateway_installed);
}

/// Greptile P1: a relocation whose backed-up write the locked keyring refuses
/// waits for the keyring (so the start-up waiter retries it), it is not failed.
#[test]
fn a_relocation_the_locked_keyring_refuses_waits_for_it() {
    let fixture = Fixture::new("relocate-locked");
    let legacy = fixture
        .home
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    write(
        &legacy,
        &json!({"mcpServers": {"toolport": old_entry("windsurf"), "native": {"command": "native", "env": {"API_KEY": "sk-relocate-locked-fixture"}}}}),
    );
    // A Toolport server stores that value, so the backup needs the vault.
    let registry = serde_json::from_value(json!({"version":3,"profiles":[],"servers":[{"id":"native","name":"Native","transport":"stdio","env":[{"key":"API_KEY","secret":true}]}]})).unwrap();
    crate::registry::save(&registry).unwrap();
    let before = std::fs::read(&legacy).unwrap();
    let mut outcome = None;
    super::backup_vault::fake::locked_store(super::backup_vault::Unlock::StillLocked, || {
        outcome = Some(super::backup_vault::noninteractive(|| {
            relocate::relocate_all(fixture.gateway(), &HashMap::new())
        }));
    });
    let outcome = outcome.unwrap();
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(outcome.deferred, vec!["windsurf".to_string()]);
    assert_eq!(std::fs::read(&legacy).unwrap(), before, "nothing written");
}

/// S1: a `~/.cline` folder (from the CLI or shared rules) is not evidence that the
/// VS Code extension moved. Until Cline's new settings file exists, the extension's
/// own storage file stays the one Toolport writes, and its entry is never removed.
#[test]
fn a_dot_cline_folder_alone_never_disconnects_the_vscode_extension() {
    let fixture = Fixture::new("relocate-cline");
    let legacy = cline_legacy_settings_path(&fixture.home, Platform::current(), &client_path_env);
    write(
        &legacy,
        &json!({"mcpServers": {"toolport": old_entry("cline"), "native": {"command": "native"}}}),
    );
    std::fs::create_dir_all(fixture.home.join(".cline").join("rules")).unwrap();
    assert_eq!(
        resolved_definition_path(&find_def("cline").unwrap()).unwrap(),
        legacy
    );
    let outcome = relocate::relocate_all(fixture.gateway(), &HashMap::new());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let old = read(&legacy);
    assert!(old["mcpServers"].get("toolport").is_some(), "{old}");
    assert_eq!(old["mcpServers"]["native"], json!({"command": "native"}));
}

/// Connect writes every old file the client may still read, and Disconnect puts
/// both files back byte for byte.
#[test]
fn connect_writes_both_files_and_disconnect_restores_both_byte_for_byte() {
    let fixture = Fixture::new("relocate-both");
    let legacy = fixture
        .home
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let legacy_bytes =
        "{\n  \"mcpServers\": {\"native\": {\"command\": \"native\"}},\n  \"theme\": 1\n}\n";
    std::fs::write(&legacy, legacy_bytes).unwrap();
    let target = client_config_path("windsurf").unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let target_bytes = "{\"mcpServers\": {\"mine\": {\"command\": \"mine\"}}}\n";
    std::fs::write(&target, target_bytes).unwrap();

    install_gateway("windsurf", None).unwrap();
    assert!(read(&target)["mcpServers"].get("toolport").is_some());
    assert!(read(&legacy)["mcpServers"].get("toolport").is_some());

    uninstall_gateway("windsurf").unwrap();
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), legacy_bytes);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), target_bytes);
}

#[test]
fn a_hand_edited_or_shared_http_legacy_entry_and_an_unreadable_target_block_the_move() {
    let fixture = Fixture::new("relocate-guards");
    let legacy = fixture.home.join(".codeium/windsurf/mcp_config.json");
    let custom = json!({"mcpServers": {"toolport": {"command": "npx", "args": ["my-bridge"]}}});
    write(&legacy, &custom);
    let moved = relocate::relocate("windsurf", fixture.gateway(), None).unwrap();
    assert!(moved.cleaned.is_empty() && moved.managed.is_none());
    assert_eq!(read(&legacy), custom, "a customized entry stays the user's");

    write(
        &legacy,
        &json!({"mcpServers": {"toolport": old_entry("windsurf")}}),
    );
    let target = client_config_path("devin-cli").unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "{ not json").unwrap();
    assert!(relocate::relocate("windsurf", fixture.gateway(), None).is_err());
    assert!(
        read(&legacy)["mcpServers"].get("toolport").is_some(),
        "the old entry is never removed without a replacement"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "{ not json");

    std::fs::remove_file(&target).unwrap();
    let shared = json!({"mcpServers": {"toolport": {"command": "npx",
        "args": ["-y", "mcp-remote", "http://127.0.0.1:8765/mcp"]}}});
    write(&legacy, &shared);
    let managed = ManagedEntry::from_gateway_entry(
        &serde_json::from_value(json!({"id": "toolport", "name": "toolport",
            "transport": "stdio", "command": "npx",
            "args": ["-y", "mcp-remote", "http://127.0.0.1:8765/mcp"]}))
        .unwrap(),
    );
    assert!(relocate::relocate("windsurf", fixture.gateway(), Some(&managed)).is_err());
    assert_eq!(read(&legacy), shared);

    // R2-1: with a Toolport entry already in the current file, the Shared HTTP
    // entry is still never rebuilt into a local gateway: endpoint, args and auth
    // stay byte for byte until the user reconnects.
    let shared = json!({"mcpServers": {"toolport": {"command": "npx",
        "args": ["-y", "mcp-remote", "http://127.0.0.1:8765/mcp", "--header", "Authorization:Bearer abc"]}}});
    write(&legacy, &shared);
    let legacy_bytes = std::fs::read_to_string(&legacy).unwrap();
    let managed = ManagedEntry::from_gateway_entry(
        &serde_json::from_value(json!({"id": "toolport", "name": "toolport",
            "transport": "stdio", "command": "npx",
            "args": ["-y", "mcp-remote", "http://127.0.0.1:8765/mcp", "--header", "Authorization:Bearer abc"]}))
        .unwrap(),
    );
    write(
        &target,
        &json!({"mcpServers": {"toolport": {"command": fixture.gateway(), "args": [],
            "env": {"TOOLPORT_CLIENT_ID": "devin-cli"}}}}),
    );
    let target_bytes = std::fs::read_to_string(&target).unwrap();
    for verdict in [
        relocate::OldFile::MaybeRead,
        relocate::OldFile::Unread("test"),
    ] {
        relocate::STATUS_OVERRIDE.with(|cell| cell.set(Some(verdict)));
        let moved = relocate::relocate("windsurf", fixture.gateway(), Some(&managed));
        relocate::STATUS_OVERRIDE.with(|cell| cell.set(None));
        let moved = moved.unwrap();
        assert!(
            moved.cleaned.is_empty() && moved.managed.is_none(),
            "{verdict:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            legacy_bytes,
            "{verdict:?}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), target_bytes);
    }
}

#[test]
fn moving_a_connection_restores_the_old_file_from_its_snapshot_and_releases_it() {
    let fixture = Fixture::new("relocate-snapshot");
    // Spelled the way Toolport builds it: snapshot records are keyed by the path
    // string, so `a/b` and `a\b` are different records on Windows.
    let legacy = fixture
        .home
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    let original = json!({"mcpServers": {"native": {"command": "native"}}, "theme": "dark"});
    write(&legacy, &original);
    let before = std::fs::read_to_string(&legacy).unwrap();
    // An older build connected Windsurf at the old location, recording a snapshot.
    let entry: ServerEntry = serde_json::from_value(json!({"id": "toolport", "name": "toolport",
        "transport": "stdio", "command": fixture.gateway(),
        "env": [{"key": "TOOLPORT_CLIENT_ID", "value": "windsurf"}]}))
    .unwrap();
    mutation::run("windsurf", &legacy, Format::JsonMcpServers, || {
        edit_format(Format::JsonMcpServers, &legacy, Some(&entry), false)
    })
    .unwrap();
    assert!(read(&legacy)["mcpServers"].get("toolport").is_some());
    assert!(restore::recorded_paths()
        .iter()
        .any(|(_, path, _)| path == &legacy));

    // Where the client is known not to read the old file, the move restores it.
    relocate::STATUS_OVERRIDE.with(|cell| cell.set(Some(relocate::OldFile::Unread("test"))));
    let moved = relocate::relocate("windsurf", fixture.gateway(), None);
    relocate::STATUS_OVERRIDE.with(|cell| cell.set(None));
    moved.unwrap();
    assert_eq!(
        std::fs::read_to_string(&legacy).unwrap(),
        before,
        "exact original bytes"
    );
    assert!(restore::released("windsurf", &legacy).unwrap());
    assert!(!restore::recorded_paths()
        .iter()
        .any(|(_, path, _)| path == &legacy));
    let target = client_config_path("devin-cli").unwrap();
    assert!(read(&target)["mcpServers"].get("toolport").is_some());
}

#[test]
fn connect_keeps_an_old_file_working_and_disconnect_leaves_no_toolport_entry_in_it() {
    let fixture = Fixture::new("relocate-connect");
    let legacy = fixture.home.join(".gemini/antigravity/mcp_config.json");
    write(
        &legacy,
        &json!({"mcpServers": {"toolport": old_entry("antigravity"),
        "native": {"command": "native"}}}),
    );
    let outcome = install_gateway("antigravity", None).unwrap();
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    // Antigravity 1.x read this file: it gets the same, current entry.
    let target = client_config_path("antigravity").unwrap();
    let written = read(&target)["mcpServers"]["toolport"].clone();
    assert!(written.is_object());
    let old = read(&legacy);
    assert_eq!(old["mcpServers"]["toolport"], written);
    assert_eq!(old["mcpServers"]["native"], json!({"command": "native"}));

    uninstall_gateway("antigravity").unwrap();
    assert!(read(&legacy)["mcpServers"].get("toolport").is_none());
    assert_eq!(
        read(&legacy)["mcpServers"]["native"],
        json!({"command": "native"})
    );
    assert!(!read_client(&find_def("antigravity").unwrap()).gateway_installed);
}

#[test]
fn the_launch_pass_moves_legacy_connections_and_reports_their_ownership() {
    let fixture = Fixture::new("relocate-launch");
    let legacy = fixture.home.join(".codeium/windsurf/mcp_config.json");
    write(
        &legacy,
        &json!({"mcpServers": {"toolport": old_entry("windsurf")}}),
    );
    let outcome = relocate::relocate_all(fixture.gateway(), &HashMap::new());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert!(outcome
        .repointed
        .iter()
        .any(|(id, entry)| id == "windsurf" && entry.command == fixture.gateway()));
    // Devin Desktop still imports the old file, so its entry stays, kept current.
    assert_eq!(
        read(&legacy)["mcpServers"]["toolport"]["command"],
        fixture.gateway()
    );
}

#[test]
fn opencode_2_server_maps_are_read_and_written_in_their_own_shape() {
    let v2 = r#"{
  "mcp": {
    "timeout": {"startup": 30},
    "servers": {
      "native": {"type": "local", "command": ["native", "--flag"], "environment": {"K": "v"}}
    },
    "legacy": {"type": "local", "command": ["old"], "enabled": true}
  }
}"#;
    let servers = parse_opencode_json(v2).unwrap();
    let names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["legacy", "native"], "never `servers` or `timeout`");

    let path = tests::temp_path("opencode-v2.json");
    std::fs::write(&path, v2).unwrap();
    let mut entry = tests::sample_gateway(None, "opencode");
    entry.command = Some("/opt/toolport-gateway".into());
    edit_opencode_gateway(&path, Some(&entry)).unwrap();
    let root = read(&path);
    let ours = &root["mcp"]["servers"]["toolport"];
    assert_eq!(ours["type"], "local");
    assert_eq!(ours["command"], json!(["/opt/toolport-gateway"]));
    assert!(
        ours.get("enabled").is_none(),
        "2.x uses `disabled`, absent = on"
    );
    assert!(root["mcp"].get("toolport").is_none());
    assert_eq!(root["mcp"]["timeout"], json!({"startup": 30}));
    assert_eq!(root["mcp"]["legacy"]["command"], json!(["old"]));
    assert!(
        parse_opencode_json(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .iter()
            .any(|s| s.name == "toolport")
    );
    // Repairs find the entry under the 2.x map.
    let value = mutation::value(
        Format::JsonOpenCodeMcp,
        Some(&std::fs::read_to_string(&path).unwrap()),
    )
    .unwrap();
    assert!(moved::server_container(Format::JsonOpenCodeMcp, &value)
        .get("toolport")
        .is_some());
    edit_opencode_gateway(&path, None).unwrap();
    let root = read(&path);
    assert!(root["mcp"]["servers"].get("toolport").is_none());
    assert!(root["mcp"]["servers"].get("native").is_some());

    // A 1.x file keeps the 1.x shape (2.x still converts it at load).
    std::fs::write(
        &path,
        r#"{"mcp": {"native": {"type": "local", "command": ["n"]}}}"#,
    )
    .unwrap();
    edit_opencode_gateway(&path, Some(&entry)).unwrap();
    let root = read(&path);
    assert_eq!(root["mcp"]["toolport"]["enabled"], true);
    assert!(root["mcp"].get("servers").is_none());
    // A 1.x server that is literally named `servers` is still a server.
    let named = r#"{"mcp": {"servers": {"type": "local", "command": ["s"]}}}"#;
    assert_eq!(parse_opencode_json(named).unwrap()[0].name, "servers");
    std::fs::remove_file(&path).ok();
}

#[test]
fn continue_files_toolport_creates_carry_the_required_header() {
    let path = tests::temp_path("continue-new.yaml");
    std::fs::remove_file(&path).ok();
    edit_continue_yaml_gateway(&path, Some(&tests::sample_gateway(None, "continue"))).unwrap();
    let root: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(root["name"], "Local Config");
    assert_eq!(root["version"], "1.0.0");
    assert_eq!(root["schema"], "v1");
    assert_eq!(root["mcpServers"][0]["name"], "toolport");
    // An existing file is the user's and gets no header.
    std::fs::write(&path, "models: []\n").unwrap();
    edit_continue_yaml_gateway(&path, Some(&tests::sample_gateway(None, "continue"))).unwrap();
    let root: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(root.get("name").is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn openclaw_profiles_select_their_own_state_dir() {
    let home = std::env::temp_dir().join(format!("toolport-oc-profile-{}", std::process::id()));
    let env = |key: &str| (key == "OPENCLAW_PROFILE").then(|| "work".to_string());
    let dirs = openclaw::dirs(&home, &env);
    assert_eq!(dirs.state, home.join(".openclaw-work"));
    assert_eq!(
        dirs.config,
        home.join(".openclaw-work").join("openclaw.json")
    );
    assert!(!dirs.default_state);
    for ignored in ["default", "-bad", "has space"] {
        let env = |key: &str| (key == "OPENCLAW_PROFILE").then(|| ignored.to_string());
        assert_eq!(
            openclaw::dirs(&home, &env).state,
            home.join(".openclaw"),
            "{ignored}"
        );
    }
    let state = home.join("state");
    let env = |key: &str| match key {
        "OPENCLAW_PROFILE" => Some("work".to_string()),
        "OPENCLAW_STATE_DIR" => Some(state.display().to_string()),
        _ => None,
    };
    assert_eq!(openclaw::dirs(&home, &env).state, state);
}

#[test]
fn crush_and_antigravity_ignore_their_old_locations() {
    let fixture = Fixture::new("old-locations");
    let local = fixture.data.dir.join("local");
    write(&local.join("crush/crush.json"), &json!({"mcp": {}}));
    assert_eq!(
        crush_path().unwrap(),
        resolve_crush_path(&fixture.home, None, client_path_env("XDG_CONFIG_HOME"))
    );
    write(
        &fixture.home.join(".gemini/antigravity-ide/mcp_config.json"),
        &json!({"mcpServers": {"x": {"command": "x"}}}),
    );
    assert_eq!(
        antigravity_path().unwrap(),
        fixture.home.join(".gemini/config/mcp_config.json")
    );
}

/// An upgrader's Goose connection in `~/Library/Application Support/Block/goose`
/// (where older Toolport wrote it) moves to the file Goose reads, and the user's
/// own extensions stay in the old file untouched.
#[cfg(target_os = "macos")]
#[test]
fn an_upgraders_goose_connection_moves_from_application_support_on_macos() {
    let fixture = Fixture::new("relocate-goose");
    let legacy = fixture
        .home
        .join("Library")
        .join("Application Support")
        .join("Block")
        .join("goose")
        .join("config.yaml");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy,
        "extensions:\n  native:\n    type: stdio\n    name: native\n    cmd: native\n    args: []\n    enabled: true\n",
    )
    .unwrap();
    let entry: ServerEntry = serde_json::from_value(json!({"id": "toolport", "name": "toolport",
        "transport": "stdio", "command": fixture.gateway(),
        "env": [{"key": "TOOLPORT_CLIENT_ID", "value": "goose"}]}))
    .unwrap();
    edit_format(Format::YamlExtensions, &legacy, Some(&entry), false).unwrap();
    let names = |path: &Path| -> Vec<String> {
        parse_client_content(
            Format::YamlExtensions,
            &std::fs::read_to_string(path).unwrap(),
        )
        .unwrap()
        .into_iter()
        .map(|server| server.name)
        .collect()
    };
    assert!(names(&legacy).contains(&GATEWAY_ENTRY_NAME.to_string()));

    let outcome = relocate::relocate_all(fixture.gateway(), &HashMap::new());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let target = client_config_path("goose").unwrap();
    // The XDG file Goose reads (the fixture isolates XDG_CONFIG_HOME), not the old one.
    let xdg = std::path::PathBuf::from(client_path_env("XDG_CONFIG_HOME").unwrap());
    assert_eq!(target, xdg.join("goose").join("config.yaml"));
    let moved = parse_client_content(
        Format::YamlExtensions,
        &std::fs::read_to_string(&target).unwrap(),
    )
    .unwrap();
    let gateway = moved
        .iter()
        .find(|server| server.name == GATEWAY_ENTRY_NAME)
        .expect("Toolport's Goose connection is in the file Goose reads");
    assert_eq!(gateway.command.as_deref(), Some(fixture.gateway()));
    assert_eq!(names(&legacy), vec!["native".to_string()]);
}

#[test]
fn each_workbuddy_build_is_detected_only_from_its_own_folder() {
    let fixture = Fixture::new("workbuddy-detect");
    let present = |id: &str| read_client(&find_def(id).unwrap()).app_present;
    let china = fixture.home.join(".workbuddy");
    let international = fixture.home.join(".workbuddy-ai");
    assert!(!present("workbuddy") && !present("workbuddy-ai"));
    assert!(
        !china.exists() && !international.exists(),
        "detection creates nothing"
    );

    std::fs::create_dir_all(&china).unwrap();
    assert!(present("workbuddy") && !present("workbuddy-ai"));
    std::fs::create_dir_all(&international).unwrap();
    assert!(present("workbuddy") && present("workbuddy-ai"));
    std::fs::remove_dir_all(&china).unwrap();
    assert!(!present("workbuddy") && present("workbuddy-ai"));

    // A stray file with the folder's name is not an install.
    std::fs::write(&china, "not a folder").unwrap();
    assert!(!present("workbuddy"));
    assert!(client_metadata()
        .iter()
        .any(|row| row.id == "workbuddy" && !row.app_present));
    for row in client_metadata() {
        if row.id == "workbuddy-ai" {
            assert!(row.app_present);
        }
    }
}

/// Each build connects, imports and disconnects only its own file. The other build's
/// file holds an older Toolport entry and keeps it, byte for byte, and each row shows
/// its own file's state.
#[test]
fn each_workbuddy_build_connects_only_its_own_file() {
    for (id, own, other) in [
        ("workbuddy", ".workbuddy", ".workbuddy-ai"),
        ("workbuddy-ai", ".workbuddy-ai", ".workbuddy"),
    ] {
        let fixture = Fixture::new("workbuddy-own");
        let file = fixture.home.join(own).join("mcp.json");
        let other_file = fixture.home.join(other).join("mcp.json");
        for path in [&file, &other_file] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        let original = concat!(
            "{\n",
            "    \"mcpServers\": {\n",
            "        \"files\": {\"command\": \"npx\", \"args\": [\"-y\", \"files-mcp\"], \"env\": {\"ROOT\": \"/tmp\"}},\n",
            "        \"map\": {\"url\": \"https://mcp.map.example/sse?key=k\"},\n",
            "        \"mail\": {\"type\": \"streamableHttp\", \"url\": \"https://mail.example/mcp\",\n",
            "                 \"headers\": {\"Authorization\": \"Bearer t\"}, \"timeout\": 30000},\n",
            "        \"feed\": {\"type\": \"sse\", \"url\": \"https://feed.example/sse\"}\n",
            "    }\n",
            "}\n"
        );
        std::fs::write(&file, original).unwrap();
        let stale = format!(
            "{{\"mcpServers\": {{\"toolport\": {{\"command\": \"/opt/old/toolport-gateway\", \"env\": {{\"TOOLPORT_CLIENT_ID\": \"{id}\"}}}}}}}}\n"
        );
        std::fs::write(&other_file, &stale).unwrap();
        let other_id = if id == "workbuddy" {
            "workbuddy-ai"
        } else {
            "workbuddy"
        };

        let def = find_def(id).unwrap();
        let row = read_client(&def);
        assert!(row.app_present && row.config_exists, "{id}");
        assert_eq!(Path::new(&row.config_path), file, "{id}");
        assert!(!row.gateway_installed, "{id} reads only its own file");
        assert!(read_client(&find_def(other_id).unwrap()).gateway_installed);
        let server = |name: &str| {
            row.servers
                .iter()
                .find(|server| server.name == name)
                .unwrap_or_else(|| panic!("{name} in {:?}", row.servers))
                .clone()
        };
        let files = server("files");
        assert_eq!(files.transport, "stdio");
        assert_eq!(files.command.as_deref(), Some("npx"));
        assert_eq!(files.args, ["-y", "files-mcp"]);
        assert_eq!(files.env_keys, ["ROOT"]);
        for (name, transport, url) in [
            ("map", "http", "https://mcp.map.example/sse?key=k"),
            ("mail", "http", "https://mail.example/mcp"),
            ("feed", "sse", "https://feed.example/sse"),
        ] {
            assert_eq!(server(name).transport, transport, "{id} {name}");
            assert_eq!(server(name).url.as_deref(), Some(url), "{id} {name}");
        }

        install_gateway(id, None).unwrap();
        let connected = read(&file);
        let entry = &connected["mcpServers"]["toolport"];
        assert!(entry["command"].as_str().is_some(), "{id}: {connected}");
        assert_eq!(entry["env"]["TOOLPORT_CLIENT_ID"], id);
        let before: serde_json::Value = serde_json::from_str(original).unwrap();
        for name in ["files", "map", "mail", "feed"] {
            assert_eq!(connected["mcpServers"][name], before["mcpServers"][name]);
        }
        assert!(read_client(&def).gateway_installed);
        assert_eq!(std::fs::read_to_string(&other_file).unwrap(), stale, "{id}");

        uninstall_gateway(id).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original, "{id}");
        assert_eq!(std::fs::read_to_string(&other_file).unwrap(), stale, "{id}");
        drop(fixture);
    }
}

/// The data folder can go away after the Clients page found it, or the config can be
/// a link into a folder that does not exist: Connect then refuses and creates nothing.
#[test]
fn workbuddy_connect_never_creates_a_folder() {
    for (id, own, other, message) in [
        (
            "workbuddy",
            ".workbuddy",
            ".workbuddy-ai",
            "WorkBuddy's folder is gone",
        ),
        (
            "workbuddy-ai",
            ".workbuddy-ai",
            ".workbuddy",
            "WorkBuddy AI's folder is gone",
        ),
    ] {
        let fixture = Fixture::new("workbuddy-gone");
        let folder = fixture.home.join(own);
        std::fs::create_dir_all(&folder).unwrap();
        assert!(read_client(&find_def(id).unwrap()).app_present);

        let removed = folder.clone();
        mutation::BEFORE_COMMIT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |_| {
                std::fs::remove_dir_all(&removed).unwrap();
            }));
        });
        let error = install_gateway(id, None).unwrap_err();
        assert!(error.contains(message), "{error}");
        assert!(!folder.exists(), "{id}: the folder was created again");

        // Gone before Connect started, too.
        let error = install_gateway(id, None).unwrap_err();
        assert!(error.contains(message), "{error}");
        assert!(
            !folder.exists() && !fixture.home.join(other).exists(),
            "{id}"
        );

        drop(fixture);
    }
}

/// A config that is a dangling link into a folder that does not exist: Connect follows
/// the link, then refuses instead of creating the link target's folder.
#[cfg(unix)]
#[test]
fn workbuddy_connect_never_creates_a_link_targets_folder() {
    for (id, own, other, message) in [
        (
            "workbuddy",
            ".workbuddy",
            ".workbuddy-ai",
            "WorkBuddy's folder is gone",
        ),
        (
            "workbuddy-ai",
            ".workbuddy-ai",
            ".workbuddy",
            "WorkBuddy AI's folder is gone",
        ),
    ] {
        let fixture = Fixture::new("workbuddy-link");
        let folder = fixture.home.join(own);
        std::fs::create_dir_all(&folder).unwrap();
        let target = fixture.home.join(other).join("mcp.json");
        std::os::unix::fs::symlink(&target, folder.join("mcp.json")).unwrap();
        let error = install_gateway(id, None).unwrap_err();
        assert!(error.contains(message), "{error}");
        assert!(
            !fixture.home.join(other).exists(),
            "{id}: the link target's folder"
        );
        drop(fixture);
    }
}

/// WorkBuddy was uninstalled after Connect: Disconnect has nothing to put back, so it
/// succeeds, clears Toolport's record and writes and creates nothing.
#[test]
fn workbuddy_disconnect_succeeds_once_its_folder_is_gone() {
    for (id, own) in [
        ("workbuddy", ".workbuddy"),
        ("workbuddy-ai", ".workbuddy-ai"),
    ] {
        for original in [
            None,
            Some("{\"mcpServers\": {}}\n"),
            Some(""),
            Some(" \n\t"),
        ] {
            let fixture = Fixture::new("workbuddy-uninstalled");
            let folder = fixture.home.join(own);
            std::fs::create_dir_all(&folder).unwrap();
            if let Some(text) = original {
                std::fs::write(folder.join("mcp.json"), text).unwrap();
            }
            crate::registry_controller::connect_client_stdio(id, None, false).unwrap();
            assert!(crate::registry::load()
                .unwrap()
                .client_managed_entries
                .contains_key(id));

            std::fs::remove_dir_all(&folder).unwrap();
            crate::registry_controller::disconnect_client(id)
                .unwrap_or_else(|error| panic!("{id} {original:?}: {error}"));
            assert!(!crate::registry::load()
                .unwrap()
                .client_managed_entries
                .contains_key(id));
            assert!(!folder.exists(), "{id} {original:?}");
            drop(fixture);
        }
    }
}

/// A stray file with a build's folder name: the full scan and the quick inventory
/// agree that there is no install and no config, with no error.
#[test]
fn a_file_named_like_a_workbuddy_folder_is_no_install_in_either_scan() {
    let fixture = Fixture::new("workbuddy-file");
    for (id, own) in [
        ("workbuddy", ".workbuddy"),
        ("workbuddy-ai", ".workbuddy-ai"),
    ] {
        std::fs::write(fixture.home.join(own), "not a folder").unwrap();
        let full = read_client(&find_def(id).unwrap());
        assert!(!full.app_present && !full.config_exists, "{id} full scan");
        assert_eq!(full.error, None, "{id}");
        let quick = client_metadata()
            .into_iter()
            .find(|row| row.id == id)
            .unwrap();
        assert_eq!(
            (quick.app_present, quick.config_exists),
            (full.app_present, full.config_exists),
            "{id}: both scans agree"
        );
    }
}

/// Servers moved into the gateway are owed back. With the folder gone, Disconnect
/// refuses and keeps the move record and recovery copies; once the folder is back,
/// a retry puts the original file back.
#[test]
fn workbuddy_disconnect_keeps_moved_servers_until_the_folder_is_back() {
    for (id, own, message) in [
        ("workbuddy", ".workbuddy", "WorkBuddy's folder is gone"),
        (
            "workbuddy-ai",
            ".workbuddy-ai",
            "WorkBuddy AI's folder is gone",
        ),
    ] {
        let fixture = Fixture::new("workbuddy-moved");
        let folder = fixture.home.join(own);
        let file = folder.join("mcp.json");
        std::fs::create_dir_all(&folder).unwrap();
        let original = "{\n  \"mcpServers\": {\"files\": {\"command\": \"npx\", \"args\": [\"files-mcp\"]}}\n}\n";
        std::fs::write(&file, original).unwrap();
        let revision = setup_revision(id).unwrap();
        migrate_reviewed(id, None, &["files".to_string()], &revision, || Ok(())).unwrap();
        let moved_in = read(&file);
        assert!(moved_in["mcpServers"].get("files").is_none(), "{moved_in}");
        assert!(moved::matches_path(id, &file).unwrap());
        let copies = restore::recovery_copies(id, &file).unwrap();

        let away = fixture.home.join(format!("{own}-trash"));
        std::fs::rename(&folder, &away).unwrap();
        let error = uninstall_gateway(id).unwrap_err();
        assert!(error.contains(message), "{id}: {error}");
        assert!(!folder.exists(), "{id}");
        assert!(
            moved::matches_path(id, &file).unwrap(),
            "{id}: move record kept"
        );
        assert_eq!(restore::recovery_copies(id, &file).unwrap(), copies, "{id}");

        std::fs::rename(&away, &folder).unwrap();
        let outcome = uninstall_gateway(id).unwrap();
        finish_uninstall(id, &outcome).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original, "{id}");
        drop(fixture);
    }
}

/// The registry update fails after Disconnect, with the folder removed meanwhile:
/// the rollback reports it instead of creating the folder again.
#[test]
fn workbuddy_disconnect_rollback_never_creates_its_folder() {
    for (id, own) in [
        ("workbuddy", ".workbuddy"),
        ("workbuddy-ai", ".workbuddy-ai"),
    ] {
        let fixture = Fixture::new("workbuddy-rollback");
        let folder = fixture.home.join(own);
        std::fs::create_dir_all(&folder).unwrap();
        crate::registry_controller::connect_client_stdio(id, None, false).unwrap();
        let removed = folder.clone();
        let result = crate::registry_controller::disconnect_client_stdio_with(id, false, |_| {
            std::fs::remove_dir_all(&removed).unwrap();
            Err("registry write failed".into())
        });
        assert!(result.is_err(), "{id}");
        assert!(!folder.exists(), "{id}: rollback created the folder");
        drop(fixture);
    }
}

/// A blank original file comes back with its exact bytes on Disconnect.
#[test]
fn workbuddy_disconnect_restores_a_blank_original_exactly() {
    for (id, own) in [
        ("workbuddy", ".workbuddy"),
        ("workbuddy-ai", ".workbuddy-ai"),
    ] {
        for original in ["", " \n\t"] {
            let fixture = Fixture::new("workbuddy-blank");
            let file = fixture.home.join(own).join("mcp.json");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, original).unwrap();
            install_gateway(id, None).unwrap();
            assert!(read(&file)["mcpServers"].get("toolport").is_some(), "{id}");
            let outcome = uninstall_gateway(id).unwrap();
            finish_uninstall(id, &outcome).unwrap();
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                original,
                "{id} {original:?}"
            );
            drop(fixture);
        }
    }
}

/// Detection checks the folder through the native path, so a home folder whose name
/// is not valid UTF-8 still finds both builds in both scans. macOS file systems refuse
/// such names (EILSEQ), so the case cannot arise there.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn workbuddy_detection_checks_the_native_folder_path() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new("workbuddy-native");
    let home = fixture.home.join(std::ffi::OsStr::from_bytes(b"h\xffome"));
    let _home = TestHome::set(&home);
    for (id, own) in [
        ("workbuddy", ".workbuddy"),
        ("workbuddy-ai", ".workbuddy-ai"),
    ] {
        std::fs::create_dir_all(home.join(own)).unwrap();
        std::fs::write(home.join(own).join("mcp.json"), "{}").unwrap();
        let full = read_client(&find_def(id).unwrap());
        assert!(full.app_present && full.config_exists, "{id} full scan");
        let quick = client_metadata()
            .into_iter()
            .find(|row| row.id == id)
            .unwrap();
        assert!(
            quick.app_present && quick.config_exists,
            "{id} quick inventory"
        );
    }
}

/// The folder disappears after the temp file is written, just before the first
/// config is published: Connect reports the folder-gone message and creates nothing.
#[test]
fn workbuddy_connect_reports_a_folder_removed_while_publishing() {
    for (id, own, message) in [
        ("workbuddy", ".workbuddy", "WorkBuddy's folder is gone"),
        (
            "workbuddy-ai",
            ".workbuddy-ai",
            "WorkBuddy AI's folder is gone",
        ),
    ] {
        let fixture = Fixture::new("workbuddy-publish");
        let folder = fixture.home.join(own);
        std::fs::create_dir_all(&folder).unwrap();
        let removed = folder.clone();
        crate::registry::client_file::HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                "commit",
                Box::new(move |_| std::fs::remove_dir_all(&removed).unwrap()),
            ))
        });
        let error = install_gateway(id, None).unwrap_err();
        assert!(error.contains(message), "{id}: {error}");
        assert!(!folder.exists(), "{id}");
        drop(fixture);
    }
}
