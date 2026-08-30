//! Read-only detection for Omarchy capabilities.
//!
//! Omarchy integration is optional. A missing command, package record, palette,
//! or selector must never stop Toolport from running as an ordinary Linux app.

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const SELECTORS: &[&str] = &[
    "agy", "claude", "codex", "copilot", "crush", "gemini", "grok", "omp", "opencode", "ori", "pi",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSelection {
    pub selector: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledAgent {
    pub selector: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentConnectionState {
    Connected,
    Available,
    Unsupported,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReview {
    pub selector: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub selected: bool,
    pub state: AgentConnectionState,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub palette_path: PathBuf,
    pub palette_available: bool,
    pub cli_available: bool,
    pub default_agent_reader_available: bool,
    pub menu_cli_available: bool,
    pub plugin_cli_available: bool,
    pub assets_available: bool,
    pub selector_capabilities: Vec<String>,
    pub installed_agents: Vec<InstalledAgent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_agent: Option<AgentSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_error: Option<String>,
}

impl Capabilities {
    pub fn environment_detected(&self) -> bool {
        self.palette_available
            || self.cli_available
            || self.default_agent_reader_available
            || self.assets_available
            || self.package.is_some()
            || self.selected_agent.is_some()
            || self.selector_error.is_some()
    }
}

struct DetectionPaths {
    home: Option<PathBuf>,
    state_home: PathBuf,
    omarchy_path: Option<PathBuf>,
    search_path: Option<std::ffi::OsString>,
    pacman_local: PathBuf,
    mirrorlist: PathBuf,
    pacman_conf: PathBuf,
}

pub fn detect() -> Capabilities {
    let home = dirs::home_dir();
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));

    detect_with(DetectionPaths {
        home,
        state_home,
        omarchy_path: std::env::var_os("OMARCHY_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        search_path: std::env::var_os("PATH"),
        pacman_local: PathBuf::from("/var/lib/pacman/local"),
        mirrorlist: PathBuf::from("/etc/pacman.d/mirrorlist"),
        pacman_conf: PathBuf::from("/etc/pacman.conf"),
    })
}

pub fn client_id_for_selector(selector: &str) -> Option<&'static str> {
    match selector {
        "agy" => Some("antigravity"),
        "claude" => Some("claude-code"),
        "codex" => Some("codex"),
        "copilot" => Some("github-copilot-cli"),
        "crush" => Some("crush"),
        "gemini" => Some("gemini-cli"),
        "grok" => Some("grok"),
        "omp" => Some("omp"),
        "opencode" => Some("opencode"),
        "ori" => None,
        "pi" => Some("pi"),
        _ => None,
    }
}

fn detect_with(paths: DetectionPaths) -> Capabilities {
    let palette_path = paths.state_home.join("omarchy/current/theme/colors.toml");
    let package = installed_package(&paths.pacman_local);
    let channel = detect_channel(&paths.mirrorlist, &paths.pacman_conf, package.as_ref());
    let default_agent_reader = find_command(paths.search_path.as_deref(), "omarchy-default-agent");
    let selector_capabilities = default_agent_reader
        .as_deref()
        .map(read_selector_capabilities)
        .unwrap_or_default();
    let (selected_agent, selector_error) = paths
        .home
        .as_ref()
        .map(|home| read_selection(&home.join(".config/omarchy/defaults/agent")))
        .unwrap_or((None, None));

    let installed_agents = SELECTORS
        .iter()
        .filter_map(|selector| {
            command_available(paths.search_path.as_deref(), selector).then(|| InstalledAgent {
                selector: (*selector).to_string(),
                client_id: client_id_for_selector(selector).map(str::to_string),
            })
        })
        .collect();

    Capabilities {
        palette_available: palette_path.is_file(),
        palette_path,
        cli_available: command_available(paths.search_path.as_deref(), "omarchy"),
        default_agent_reader_available: default_agent_reader.is_some(),
        menu_cli_available: command_available(paths.search_path.as_deref(), "omarchy-menu"),
        plugin_cli_available: command_available(paths.search_path.as_deref(), "omarchy-plugin-add"),
        assets_available: paths
            .omarchy_path
            .as_ref()
            .is_some_and(|path| path.is_dir()),
        selector_capabilities,
        installed_agents,
        package,
        channel,
        selected_agent,
        selector_error,
    }
}

/// Build the confirmation model for Omarchy agents without changing client files.
///
/// Command detection and the existing Toolport client probes are intentionally
/// combined. Omarchy's lazy command shims can prove an agent is installed before
/// it has created a config directory, while an existing config remains useful
/// evidence when a GUI launch has a narrower PATH than the user's shell.
pub fn review_installed_agents(
    capabilities: &Capabilities,
    detected_clients: &[crate::clients::DetectedClient],
) -> Vec<AgentReview> {
    let selected = capabilities
        .selected_agent
        .as_ref()
        .map(|agent| agent.selector.as_str());
    let installed = capabilities
        .installed_agents
        .iter()
        .map(|agent| agent.selector.as_str())
        .collect::<std::collections::HashSet<_>>();

    SELECTORS
        .iter()
        .filter_map(|selector| {
            let selector_available = !capabilities.default_agent_reader_available
                || capabilities
                    .selector_capabilities
                    .iter()
                    .any(|available| available == selector);
            let client_id = client_id_for_selector(selector);
            let client = client_id.and_then(|client_id| {
                detected_clients
                    .iter()
                    .find(|client| client.id == client_id)
            });
            let is_selected = selected == Some(*selector);
            let is_installed = installed.contains(selector)
                || client.is_some_and(|client| client.app_present || client.config_exists);
            if (!is_installed || !selector_available) && !is_selected {
                return None;
            }

            let (state, detail) = if !selector_available {
                (
                    AgentConnectionState::Blocked,
                    "The installed Omarchy selector does not expose this agent.".to_string(),
                )
            } else if *selector == "ori" {
                (
                    AgentConnectionState::Unsupported,
                    "Ori has no reviewed user-global MCP configuration yet.".to_string(),
                )
            } else if !is_installed {
                (
                    AgentConnectionState::Blocked,
                    "Selected in Omarchy, but the agent installation was not detected.".to_string(),
                )
            } else if let Some(client) = client {
                if client.error.is_some() {
                    (
                        AgentConnectionState::Blocked,
                        "Its MCP configuration could not be read safely.".to_string(),
                    )
                } else if client.uses_connectors {
                    (
                        AgentConnectionState::Blocked,
                        "This client requires an account-managed connector.".to_string(),
                    )
                } else {
                    match client.entry_state {
                        crate::clients::GatewayEntryState::Managed => (
                            AgentConnectionState::Connected,
                            "Connected to the Toolport gateway.".to_string(),
                        ),
                        crate::clients::GatewayEntryState::Customized => (
                            AgentConnectionState::Blocked,
                            "Its Toolport entry was customized and needs individual review."
                                .to_string(),
                        ),
                        crate::clients::GatewayEntryState::Absent => (
                            AgentConnectionState::Available,
                            "Ready to connect after confirmation.".to_string(),
                        ),
                    }
                }
            } else {
                (
                    AgentConnectionState::Blocked,
                    "Toolport could not resolve this agent's client adapter.".to_string(),
                )
            };

            Some(AgentReview {
                selector: (*selector).to_string(),
                name: agent_name(selector).to_string(),
                client_id: client_id.map(str::to_string),
                selected: is_selected,
                state,
                detail,
            })
        })
        .collect()
}

fn agent_name(selector: &str) -> &'static str {
    match selector {
        "agy" => "Antigravity",
        "claude" => "Claude Code",
        "codex" => "Codex",
        "copilot" => "GitHub Copilot",
        "crush" => "Crush",
        "gemini" => "Gemini CLI",
        "grok" => "Grok",
        "omp" => "Oh My Pi",
        "opencode" => "OpenCode",
        "ori" => "Ori",
        "pi" => "Pi",
        _ => "Unknown agent",
    }
}

fn command_available(search_path: Option<&OsStr>, command: &str) -> bool {
    find_command(search_path, command).is_some()
}

fn find_command(search_path: Option<&OsStr>, command: &str) -> Option<PathBuf> {
    search_path.and_then(|value| {
        std::env::split_paths(value)
            .map(|dir| dir.join(command))
            .find(|path| executable_file(path))
    })
}

fn read_selector_capabilities(path: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Some(value) = contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# omarchy:args=[")
            .and_then(|value| value.strip_suffix(']'))
    }) else {
        return Vec::new();
    };
    value
        .split('|')
        .map(str::trim)
        .filter(|selector| SELECTORS.contains(selector))
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    path.is_file()
}

fn read_selection(path: &Path) -> (Option<AgentSelection>, Option<String>) {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (None, None),
        Err(error) => {
            return (
                None,
                Some(format!(
                    "Could not read the Omarchy agent selector: {error}"
                )),
            )
        }
    };
    let selector = contents.trim();
    if selector.is_empty() {
        return (None, None);
    }
    if contents.lines().count() != 1 || !SELECTORS.contains(&selector) {
        return (
            None,
            Some(format!("Unknown Omarchy agent selector: {selector}")),
        );
    }
    (
        Some(AgentSelection {
            selector: selector.to_string(),
            client_id: client_id_for_selector(selector).map(str::to_string),
        }),
        None,
    )
}

fn installed_package(local_db: &Path) -> Option<PackageInfo> {
    let entries = std::fs::read_dir(local_db).ok()?;
    let mut stable = None;
    for entry in entries.flatten() {
        let desc = entry.path().join("desc");
        let Ok(contents) = std::fs::read_to_string(desc) else {
            continue;
        };
        let Some(name) = desc_value(&contents, "NAME") else {
            continue;
        };
        if name != "omarchy" && name != "omarchy-dev" {
            continue;
        }
        let Some(version) = desc_value(&contents, "VERSION") else {
            continue;
        };
        let package = PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
        };
        if name == "omarchy-dev" {
            return Some(package);
        }
        stable = Some(package);
    }
    stable
}

fn desc_value<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("%{key}%");
    let mut lines = contents.lines();
    while let Some(line) = lines.next() {
        if line == marker {
            return lines.next().filter(|value| !value.is_empty());
        }
    }
    None
}

fn detect_channel(
    mirrorlist_path: &Path,
    pacman_conf_path: &Path,
    package: Option<&PackageInfo>,
) -> Option<String> {
    let mirrorlist = std::fs::read_to_string(mirrorlist_path).unwrap_or_default();
    let pacman_conf = std::fs::read_to_string(pacman_conf_path).unwrap_or_default();
    let mirror = channel_in(&mirrorlist, true);
    let packages = channel_in(&pacman_conf, false);

    match (mirror, packages) {
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), Some(right)) => Some(format!("{left} / {right}")),
        (Some(channel), None) | (None, Some(channel)) => Some(channel.to_string()),
        (None, None) if package.is_some_and(|value| value.name == "omarchy-dev") => {
            Some("edge".to_string())
        }
        _ => None,
    }
}

fn channel_in(contents: &str, mirror: bool) -> Option<&'static str> {
    let urls = if mirror {
        [
            ("https://stable-mirror.omarchy.org/", "stable"),
            ("https://rc-mirror.omarchy.org/", "rc"),
            ("https://mirror.omarchy.org/", "edge"),
        ]
    } else {
        [
            ("https://pkgs.omarchy.org/stable/", "stable"),
            ("https://pkgs.omarchy.org/rc/", "rc"),
            ("https://pkgs.omarchy.org/edge/", "edge"),
        ]
    };
    urls.into_iter()
        .find_map(|(needle, channel)| contents.contains(needle).then_some(channel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "toolport-omarchy-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn fixture(root: &Path) -> DetectionPaths {
        DetectionPaths {
            home: Some(root.join("home")),
            state_home: root.join("state"),
            omarchy_path: None,
            search_path: Some(root.join("bin").into_os_string()),
            pacman_local: root.join("pacman"),
            mirrorlist: root.join("mirrorlist"),
            pacman_conf: root.join("pacman.conf"),
        }
    }

    fn detected_client(
        id: &str,
        state: crate::clients::GatewayEntryState,
    ) -> crate::clients::DetectedClient {
        crate::clients::DetectedClient {
            id: id.to_string(),
            name: id.to_string(),
            uses_connectors: false,
            config_path: format!("/tmp/{id}.json"),
            config_exists: true,
            app_present: true,
            servers: Vec::new(),
            plugin_servers: Vec::new(),
            gateway_installed: state != crate::clients::GatewayEntryState::Absent,
            entry_state: state,
            error: None,
        }
    }

    #[test]
    fn ordinary_linux_has_no_omarchy_capabilities() {
        let root = temp_dir("absent");
        let detected = detect_with(fixture(&root));

        assert!(!detected.palette_available);
        assert!(!detected.cli_available);
        assert!(!detected.assets_available);
        assert_eq!(detected.package, None);
        assert_eq!(detected.channel, None);
        assert_eq!(detected.selected_agent, None);
        assert_eq!(detected.selector_error, None);
        assert!(!detected.environment_detected());
    }

    #[cfg(unix)]
    #[test]
    fn detects_independent_capabilities_and_xdg_state_palette() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("capabilities");
        let paths = fixture(&root);
        fs::create_dir_all(paths.state_home.join("omarchy/current/theme")).unwrap();
        fs::write(
            paths.state_home.join("omarchy/current/theme/colors.toml"),
            "accent = \"#123456\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        for command in [
            "omarchy",
            "omarchy-default-agent",
            "omarchy-menu",
            "codex",
            "ori",
        ] {
            let file = root.join("bin").join(command);
            fs::write(&file, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(
            root.join("bin/omarchy-default-agent"),
            "#!/bin/sh\n# omarchy:args=[pi|ori|codex]\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("assets")).unwrap();

        let mut paths = paths;
        paths.omarchy_path = Some(root.join("assets"));
        let detected = detect_with(paths);

        assert!(detected.palette_available);
        assert!(detected.cli_available);
        assert!(detected.default_agent_reader_available);
        assert!(detected.menu_cli_available);
        assert!(!detected.plugin_cli_available);
        assert!(detected.assets_available);
        assert!(detected.environment_detected());
        assert_eq!(detected.selector_capabilities, ["pi", "ori", "codex"]);
        assert_eq!(
            detected.installed_agents,
            vec![
                InstalledAgent {
                    selector: "codex".into(),
                    client_id: Some("codex".into()),
                },
                InstalledAgent {
                    selector: "ori".into(),
                    client_id: None,
                },
            ]
        );
    }

    #[test]
    fn review_is_read_only_and_classifies_installed_agents() {
        let root = temp_dir("review");
        let mut capabilities = detect_with(fixture(&root));
        capabilities.selected_agent = Some(AgentSelection {
            selector: "codex".into(),
            client_id: Some("codex".into()),
        });
        capabilities.installed_agents = vec![
            InstalledAgent {
                selector: "codex".into(),
                client_id: Some("codex".into()),
            },
            InstalledAgent {
                selector: "ori".into(),
                client_id: None,
            },
            InstalledAgent {
                selector: "pi".into(),
                client_id: Some("pi".into()),
            },
        ];
        let clients = vec![
            detected_client("codex", crate::clients::GatewayEntryState::Managed),
            detected_client("pi", crate::clients::GatewayEntryState::Absent),
        ];

        let review = review_installed_agents(&capabilities, &clients);
        assert_eq!(review.len(), 3);
        assert_eq!(review[0].selector, "codex");
        assert!(review[0].selected);
        assert_eq!(review[0].state, AgentConnectionState::Connected);
        assert_eq!(review[1].selector, "ori");
        assert_eq!(review[1].state, AgentConnectionState::Unsupported);
        assert_eq!(review[2].selector, "pi");
        assert_eq!(review[2].state, AgentConnectionState::Available);
    }

    #[test]
    fn review_blocks_customized_and_missing_selected_agents() {
        let root = temp_dir("blocked-review");
        let mut capabilities = detect_with(fixture(&root));
        capabilities.selected_agent = Some(AgentSelection {
            selector: "claude".into(),
            client_id: Some("claude-code".into()),
        });
        capabilities.installed_agents = vec![InstalledAgent {
            selector: "codex".into(),
            client_id: Some("codex".into()),
        }];
        let clients = vec![detected_client(
            "codex",
            crate::clients::GatewayEntryState::Customized,
        )];

        let review = review_installed_agents(&capabilities, &clients);
        assert_eq!(review.len(), 2);
        assert_eq!(review[0].selector, "claude");
        assert_eq!(review[0].state, AgentConnectionState::Blocked);
        assert_eq!(review[1].selector, "codex");
        assert_eq!(review[1].state, AgentConnectionState::Blocked);
    }

    #[test]
    fn review_fails_closed_when_the_installed_selector_exposes_no_known_agents() {
        let root = temp_dir("missing-selector-capabilities");
        let mut capabilities = detect_with(fixture(&root));
        capabilities.default_agent_reader_available = true;
        capabilities.installed_agents = vec![InstalledAgent {
            selector: "codex".into(),
            client_id: Some("codex".into()),
        }];
        let clients = vec![detected_client(
            "codex",
            crate::clients::GatewayEntryState::Absent,
        )];

        let review = review_installed_agents(&capabilities, &clients);
        assert!(review.is_empty());

        capabilities.selected_agent = Some(AgentSelection {
            selector: "codex".into(),
            client_id: Some("codex".into()),
        });
        let selected_review = review_installed_agents(&capabilities, &clients);
        assert_eq!(selected_review.len(), 1);
        assert_eq!(selected_review[0].state, AgentConnectionState::Blocked);
        assert_eq!(
            selected_review[0].detail,
            "The installed Omarchy selector does not expose this agent."
        );
    }

    #[test]
    fn selector_is_home_anchored_and_maps_each_supported_agent() {
        let root = temp_dir("selector");
        let paths = fixture(&root);
        let selector = root.join("home/.config/omarchy/defaults/agent");
        fs::create_dir_all(selector.parent().unwrap()).unwrap();
        fs::write(&selector, "agy\n").unwrap();

        let detected = detect_with(paths);
        assert_eq!(
            detected.selected_agent,
            Some(AgentSelection {
                selector: "agy".into(),
                client_id: Some("antigravity".into()),
            })
        );
        assert_eq!(detected.selector_error, None);
    }

    #[test]
    fn ori_is_known_but_explicitly_unsupported() {
        let root = temp_dir("ori");
        let paths = fixture(&root);
        let selector = root.join("home/.config/omarchy/defaults/agent");
        fs::create_dir_all(selector.parent().unwrap()).unwrap();
        fs::write(&selector, "ori\n").unwrap();

        let detected = detect_with(paths);
        assert_eq!(detected.selected_agent.unwrap().client_id, None);
        assert_eq!(detected.selector_error, None);
    }

    #[test]
    fn malformed_or_unknown_selector_is_not_treated_as_an_agent() {
        let root = temp_dir("bad-selector");
        let paths = fixture(&root);
        let selector = root.join("home/.config/omarchy/defaults/agent");
        fs::create_dir_all(selector.parent().unwrap()).unwrap();
        fs::write(&selector, "codex\nclaude\n").unwrap();

        let detected = detect_with(paths);
        assert_eq!(detected.selected_agent, None);
        assert!(detected.selector_error.unwrap().contains("Unknown"));
    }

    #[test]
    fn package_database_beats_the_stale_version_file_contract() {
        let root = temp_dir("package");
        let paths = fixture(&root);
        let package = root.join("pacman/omarchy-dev-4.0.0.r1-1");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("desc"),
            "%NAME%\nomarchy-dev\n\n%VERSION%\n4.0.0.r1-1\n",
        )
        .unwrap();

        let detected = detect_with(paths);
        assert_eq!(
            detected.package,
            Some(PackageInfo {
                name: "omarchy-dev".into(),
                version: "4.0.0.r1-1".into(),
            })
        );
        assert_eq!(detected.channel.as_deref(), Some("edge"));
    }

    #[test]
    fn reports_mixed_package_channels_without_hiding_the_mismatch() {
        let root = temp_dir("mixed-channel");
        let paths = fixture(&root);
        fs::write(
            &paths.mirrorlist,
            "Server = https://stable-mirror.omarchy.org/$repo/os/$arch\n",
        )
        .unwrap();
        fs::write(
            &paths.pacman_conf,
            "Server = https://pkgs.omarchy.org/edge/$arch\n",
        )
        .unwrap();

        let detected = detect_with(paths);
        assert_eq!(detected.channel.as_deref(), Some("stable / edge"));
    }
}
