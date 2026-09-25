//! Explicit, shell-free argument binding for catalog and saved stdio servers.

use crate::registry::{ArgPart, ServerEntry};

#[derive(Debug)]
pub struct ResolvedArgs {
    pub args: Vec<String>,
    sensitive: Vec<String>,
    has_binding: bool,
}

impl ResolvedArgs {
    /// Child stderr and spawn errors can echo argv. Bound invocations return a
    /// generic error because even a bounded tail can contain a secret fragment.
    pub fn redact(&self, message: String) -> String {
        // A child's bounded stderr tail may start in the middle of a value, or
        // quote/escape it. Exact string replacement cannot make that safe.
        // Keep all post-resolution errors opaque for bound invocations; setup
        // and vault failures are reported before this point with input labels.
        if self.has_binding {
            return "downstream server failed after launch; check its command and setup values"
                .into();
        }
        let mut values = self.sensitive.clone();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        values
            .into_iter()
            .filter(|s| !s.is_empty())
            .fold(message, |m, s| m.replace(&s, "<redacted>"))
    }
}

pub fn resolve_args(server: &ServerEntry) -> Result<ResolvedArgs, String> {
    let args = resolve_args_with(server, crate::secrets::get_vault_secret_result)?;
    if let Some(launch) = &server.launch {
        for key in &launch.required_env {
            let entry = server
                .env
                .iter()
                .find(|entry| &entry.key == key)
                .ok_or_else(|| {
                    format!(
                        "{} needs environment setting {key}. Open server setup to add it.",
                        server.name
                    )
                })?;
            let present = match &entry.value {
                Some(value) => !value.trim().is_empty(),
                None if entry.secret => crate::secrets::get_secret_result(&server.id, key)
                    .map_err(|_| format!("could not read '{key}' from the vault"))?
                    .is_some_and(|value| !value.trim().is_empty()),
                None => false,
            };
            if !present {
                return Err(format!(
                    "{} needs {key}. Open server setup to add it.",
                    server.name
                ));
            }
        }
    }
    Ok(args)
}

pub fn resolve_args_with(
    server: &ServerEntry,
    mut vault: impl FnMut(&str, &str) -> Result<Option<String>, String>,
) -> Result<ResolvedArgs, String> {
    let mut args = server.args.clone();
    let mut sensitive = Vec::new();
    let Some(launch) = &server.launch else {
        return Ok(ResolvedArgs {
            args,
            sensitive,
            has_binding: false,
        });
    };
    launch.validate(&args, false)?;
    for binding in &launch.bindings {
        let mut value = String::new();
        for part in &binding.parts {
            match part {
                ArgPart::Literal { value: literal } => value.push_str(literal),
                ArgPart::Input { key } => {
                    let input = launch
                        .inputs
                        .iter()
                        .find(|i| &i.key == key)
                        .ok_or_else(|| {
                            format!("launch argument refers to missing input '{key}'")
                        })?;
                    let resolved = if input.secret {
                        match &input.value {
                            Some(v) => Some(v.clone()), // unsaved Test Connection only
                            None => vault(&server.id, &input.key).map_err(|_| {
                                format!("could not read '{}' from the vault", input.label)
                            })?,
                        }
                    } else {
                        input.value.clone()
                    };
                    let resolved = match resolved.filter(|v| !v.trim().is_empty()) {
                        Some(v) => v,
                        None if input.required => {
                            return Err(format!(
                                "{} needs {}. Open server setup to add it.",
                                server.name, input.label
                            ))
                        }
                        None => String::new(),
                    };
                    sensitive.push(resolved.clone());
                    value.push_str(&resolved);
                }
            }
        }
        sensitive.push(value.clone());
        args[binding.index] = value;
    }
    // Screen the actual invocation. The normal transport also screens after
    // normalization and launcher rewriting; its errors are redacted by callers.
    if let Some(command) = server.command.as_deref() {
        crate::downstream::screen_spawn_command(command, &args)
            .map_err(|_| "launch arguments failed Toolport's spawn safety check".to_string())?;
    }
    Ok(ResolvedArgs {
        args,
        sensitive,
        has_binding: !launch.bindings.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ArgBinding, LaunchConfig, LaunchInput};

    #[test]
    fn composes_one_arg_and_redacts_child_output() {
        let mut server: ServerEntry = serde_json::from_str(r#"{"name":"Twilio","transport":"stdio","command":"npx","args":["-y","@twilio-alpha/mcp","<launch-input>"]}"#).unwrap();
        server.launch = Some(LaunchConfig {
            inputs: vec![
                LaunchInput {
                    key: "SID".into(),
                    label: "Account SID".into(),
                    secret: false,
                    required: true,
                    value: Some("AC123".into()),
                },
                LaunchInput {
                    key: "KEY".into(),
                    label: "API Secret".into(),
                    secret: true,
                    required: true,
                    value: None,
                },
            ],
            bindings: vec![ArgBinding {
                index: 2,
                parts: vec![
                    ArgPart::Input { key: "SID".into() },
                    ArgPart::Literal { value: "/".into() },
                    ArgPart::Input { key: "KEY".into() },
                ],
            }],
            ..Default::default()
        });
        let args = resolve_args_with(&server, |_, _| Ok(Some("s3cr3t".into()))).unwrap();
        assert_eq!(args.args[2], "AC123/s3cr3t");
        assert!(!args.redact("failed AC123/s3cr3t".into()).contains("s3cr3t"));
        assert!(!args.redact("truncated cr3t".into()).contains("cr3t"));
        assert!(resolve_args_with(&server, |_, _| Ok(None))
            .unwrap_err()
            .contains("API Secret"));
        assert!(
            resolve_args_with(&server, |_, _| Err("vault broke s3cr3t".into()))
                .unwrap_err()
                .contains("vault")
        );
    }

    #[test]
    fn windows_command_name_is_screened_after_binding() {
        let mut server: ServerEntry = serde_json::from_str(r#"{"name":"unsafe","transport":"stdio","command":"C:\\Node\\node.exe","args":["<launch-input>","server.js"]}"#).unwrap();
        server.launch = Some(LaunchConfig {
            inputs: vec![LaunchInput {
                key: "FLAG".into(),
                label: "Flag".into(),
                secret: false,
                required: true,
                value: Some("-e".into()),
            }],
            bindings: vec![ArgBinding {
                index: 0,
                parts: vec![ArgPart::Input { key: "FLAG".into() }],
            }],
            ..Default::default()
        });
        let error = resolve_args_with(&server, |_, _| Ok(None)).unwrap_err();
        assert!(error.contains("spawn safety"));
        assert!(!error.contains("-e"));
    }
}
