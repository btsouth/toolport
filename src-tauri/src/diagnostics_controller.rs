//! Shell-neutral diagnostics and observability export operations.

use crate::registry::{self, Registry};

const DIAG_LOG_LINES: usize = 200;

pub fn gather() -> String {
    use std::fmt::Write as _;
    let mut output = String::new();
    let _ = writeln!(output, "Toolport diagnostics");
    let _ = writeln!(output, "version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        output,
        "os: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    match registry::load() {
        Ok(registry) => output.push_str(&registry_summary(&registry)),
        Err(error) => {
            let _ = writeln!(output, "\nregistry: failed to load: {error}");
        }
    }
    let _ = writeln!(output, "\ngateway log (last {DIAG_LOG_LINES} lines):");
    output.push_str(&gateway_log_tail(DIAG_LOG_LINES));
    output
}

pub(crate) fn registry_summary(registry: &Registry) -> String {
    use std::fmt::Write as _;
    let mut output = String::new();
    let active = registry.active_profile_id();
    let _ = writeln!(output, "\nsettings:");
    let _ = writeln!(output, "  lazy discovery: {}", registry.lazy_discovery);
    let global_mode = registry
        .discovery_mode
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| {
            if registry.lazy_discovery {
                "lazy".into()
            } else {
                "full".into()
            }
        });
    let _ = writeln!(output, "  discovery mode: {global_mode} (global)");
    if !registry.client_discovery.is_empty() {
        let mut overrides = registry
            .client_discovery
            .iter()
            .map(|(id, mode)| format!("{id}={mode}"))
            .collect::<Vec<_>>();
        overrides.sort();
        let _ = writeln!(output, "  per-client discovery: {}", overrides.join(", "));
    }
    let _ = writeln!(output, "  deny destructive: {}", registry.deny_destructive);
    let _ = writeln!(
        output,
        "  HTTP endpoint: {}{}",
        if registry.http_bridge_enabled {
            "on"
        } else {
            "off"
        },
        registry
            .http_bridge_port
            .map(|port| format!(" (port {port})"))
            .unwrap_or_default()
    );
    let _ = writeln!(output, "  active profile: {active}");
    let _ = writeln!(output, "\nservers ({}):", registry.servers.len());
    for server in &registry.servers {
        let enabled = if registry.is_enabled(&active, &server.id) {
            "on"
        } else {
            "off"
        };
        let target = match (&server.command, &server.url) {
            (Some(command), _) => safe_command_target(command, &server.args),
            (None, Some(url)) => registry::redact_url_userinfo(url),
            _ => String::new(),
        };
        let _ = writeln!(
            output,
            "  [{enabled}] {} ({}) {target}",
            server.id, server.transport
        );
        if !server.env.is_empty() {
            let keys = server
                .env
                .iter()
                .map(|entry| {
                    if entry.secret {
                        format!("{} (secret)", entry.key)
                    } else {
                        entry.key.clone()
                    }
                })
                .collect::<Vec<_>>();
            let _ = writeln!(output, "        env: {}", keys.join(", "));
        }
    }
    let _ = writeln!(output, "\nprofiles ({}):", registry.profiles.len());
    for profile in &registry.profiles {
        let _ = writeln!(
            output,
            "  {}: [{}]",
            profile.name,
            profile.enabled_server_ids.join(", ")
        );
    }
    output
}

pub(crate) fn safe_command_target(command: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(redact_argument(command));
    let mask = registry::secret_arg_mask(args);
    for (argument, secret) in args.iter().zip(mask) {
        parts.push(if secret {
            "<redacted>".to_string()
        } else {
            registry::redact_url_userinfo(argument)
        });
    }
    parts.join(" ").trim().to_string()
}

fn redact_argument(argument: &str) -> String {
    if registry::arg_looks_secret(argument) {
        "<redacted>".to_string()
    } else {
        registry::redact_url_userinfo(argument)
    }
}

fn gateway_log_tail(lines: usize) -> String {
    let Some(path) = registry::gateway_log_path() else {
        return "(log path unavailable)\n".to_string();
    };
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => last_lines(&text, lines),
        _ => "(no gateway log yet, connect a client to populate it)\n".to_string(),
    }
}

pub(crate) fn last_lines(text: &str, lines: usize) -> String {
    let all = text.lines().collect::<Vec<_>>();
    let start = all.len().saturating_sub(lines);
    let mut tail = all[start..].join("\n");
    if !tail.is_empty() {
        tail.push('\n');
    }
    tail
}

pub fn export_audit(path: &std::path::Path, format: &str) -> Result<(), String> {
    let entries = crate::audit::read_recent(usize::MAX)
        .map_err(|error| format!("Couldn't read the activity log: {error}"))?;
    let body = if format == "csv" {
        crate::audit::to_csv(&entries)
    } else {
        serde_json::to_string_pretty(&entries).map_err(|error| error.to_string())?
    };
    std::fs::write(path, body).map_err(|error| format!("Couldn't write the file: {error}"))
}

pub fn open_data_dir() -> Result<(), String> {
    let directory = registry::conduit_dir().ok_or("could not resolve the data directory")?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create the data directory: {error}"))?;
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    let mut command = std::process::Command::new(program);
    crate::hostenv::strip_bundled_env(&mut command);
    command
        .arg(directory)
        .spawn()
        .map_err(|error| format!("could not open the data directory: {error}"))?;
    Ok(())
}
