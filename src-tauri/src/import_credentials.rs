//! Reviewed imports retain values in the vault and definitions in the registry.
use crate::{
    registry::{self, ServerEntry},
    secrets,
};
use serde::{Serialize, Serializer};

const VAULT_FAILURE: &str =
    "Keychain unavailable. Unlock it and retry importing. Client config unchanged.";

pub(crate) fn provided(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && !value.starts_with(['$', '<', '{'])
        && !value.to_ascii_uppercase().starts_with("YOUR_")
        && !value.to_ascii_uppercase().starts_with("REPLACE_")
}

pub(crate) fn shown_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return "<endpoint URL>".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    if registry::arg_looks_secret(url.path()) {
        url.set_path("/");
    }
    url.to_string()
}

pub(crate) fn shown_args(args: &[String]) -> Vec<String> {
    args.iter()
        .zip(registry::secret_arg_mask(args))
        .map(|(arg, secret)| {
            if secret {
                "<launch-input>".into()
            } else {
                arg.clone()
            }
        })
        .collect()
}

pub(crate) fn serialize_args<S: Serializer>(
    value: &[String],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    shown_args(value).serialize(serializer)
}
pub(crate) fn serialize_url<S: Serializer>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.as_deref().map(shown_url).serialize(serializer)
}
pub(crate) fn serialize_command<S: Serializer>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value
        .as_ref()
        .map(|v| {
            if registry::arg_looks_secret(v) {
                "<command>"
            } else {
                v
            }
        })
        .serialize(serializer)
}

pub(crate) struct Import {
    pub entry: ServerEntry,
    values: Vec<(String, Option<String>)>,
}

impl Import {
    pub(crate) fn prepare(
        mut entry: ServerEntry,
        definition: Option<&serde_json::Value>,
    ) -> Result<Self, String> {
        if entry
            .command
            .as_deref()
            .is_some_and(registry::arg_looks_secret)
        {
            return Err("The command contains a credential. Move it to an environment setting and review again. Client config unchanged.".into());
        }
        if let Some(definition) = definition {
            for field in ["env", "environment", "envs", "headers", "http_headers"] {
                if let Some(settings) = definition.get(field).and_then(|v| v.as_object()) {
                    for key in settings.keys() {
                        if key.is_empty() || key.contains(['=', '\0']) {
                            return Err("An imported credential has an invalid name. Fix its native config and review again.".into());
                        }
                        if !entry.env.iter().any(|e| &e.key == key) {
                            entry.env.push(registry::EnvVar {
                                key: key.clone(),
                                value: None,
                                secret: true,
                                unknown_fields: Default::default(),
                            });
                        }
                    }
                }
            }
            for field in ["bearerTokenEnvVar", "bearer_token_env_var"] {
                if let Some(key) = definition.get(field).and_then(|v| v.as_str()) {
                    if !entry.env.iter().any(|e| e.key == key) {
                        entry.env.push(registry::EnvVar {
                            key: key.into(),
                            value: None,
                            secret: true,
                            unknown_fields: Default::default(),
                        });
                    }
                }
            }
            if definition
                .get("env_http_headers")
                .and_then(|v| v.as_object())
                .is_some_and(|v| !v.is_empty())
            {
                return Err("This server uses HTTP headers from environment variables. Keep its native entry until those headers are supported.".into());
            }
        }
        let mut values = Vec::new();
        for env in &entry.env {
            let value = definition
                .and_then(|d| {
                    ["env", "environment", "envs", "headers", "http_headers"]
                        .into_iter()
                        .find_map(|field| d.get(field)?.get(&env.key))
                })
                .and_then(|value| match value {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    serde_json::Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                });
            values.push((env.key.clone(), value));
        }
        if entry.transport != "stdio" {
            // Arbitrary header schemes have no equivalent in Toolport's bearer
            // transport. Refuse the cutover instead of silently dropping them.
            if let Some(definition) = definition {
                for field in ["headers", "http_headers"] {
                    if let Some(headers) = definition.get(field).and_then(|v| v.as_object()) {
                        if headers
                            .keys()
                            .any(|key| !key.eq_ignore_ascii_case("authorization"))
                        {
                            return Err("This server uses custom HTTP headers. Keep its native entry until those headers are supported.".into());
                        }
                    }
                }
            }
            for (key, value) in &mut values {
                if key.eq_ignore_ascii_case("authorization") {
                    if let Some(raw) = value {
                        let Some((scheme, token)) = raw.split_once(' ') else {
                            return Err("This server needs a bearer token. Review its native authentication settings. Client config unchanged.".into());
                        };
                        if !scheme.eq_ignore_ascii_case("bearer") {
                            return Err("This server uses unsupported HTTP authentication. Keep its native entry.".into());
                        }
                        *raw = token.to_string();
                    }
                    *key = secrets::HTTP_AUTH_KEY.into();
                }
            }
            if let Some(url) = entry.url.as_ref() {
                let secret = url::Url::parse(url).is_ok_and(|parsed| {
                    parsed.query().is_some()
                        || !parsed.username().is_empty()
                        || parsed.password().is_some()
                        || registry::arg_looks_secret(parsed.path())
                });
                if secret {
                    values.push((secrets::IMPORTED_URL_KEY.into(), Some(url.clone())));
                    entry.url = Some(shown_url(url));
                }
            }
        }
        if let Some(launch) = &mut entry.launch {
            for input in &mut launch.inputs {
                if input.secret {
                    values.push((input.key.clone(), input.value.take()));
                }
            }
        }
        let mask = registry::secret_arg_mask(&entry.args);
        let had_launch = entry.launch.is_some();
        let mut launch = entry.launch.take().unwrap_or_default();
        for (index, secret) in mask.into_iter().enumerate() {
            if !secret || launch.bindings.iter().any(|binding| binding.index == index) {
                continue;
            }
            let key = format!("IMPORTED_ARG_{index}");
            values.push((
                key.clone(),
                Some(std::mem::replace(
                    &mut entry.args[index],
                    "<launch-input>".into(),
                )),
            ));
            launch.inputs.push(registry::LaunchInput {
                key: key.clone(),
                label: format!("Imported argument {}", index + 1),
                secret: true,
                required: true,
                value: None,
                unknown_fields: Default::default(),
            });
            launch.bindings.push(registry::ArgBinding {
                index,
                parts: vec![registry::ArgPart::Input {
                    key,
                    unknown_fields: Default::default(),
                }],
                unknown_fields: Default::default(),
            });
        }
        entry.env = values
            .iter()
            .filter(|(key, _)| !launch.inputs.iter().any(|i| &i.key == key))
            .map(|(key, _)| registry::EnvVar {
                key: key.clone(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            })
            .collect();
        if !had_launch {
            launch.required_env = entry.env.iter().map(|e| e.key.clone()).collect();
        }
        if had_launch || !launch.inputs.is_empty() || !launch.required_env.is_empty() {
            entry.launch = Some(launch);
        }
        Ok(Self { entry, values })
    }

    pub(crate) fn transfer(&self, id: &str) -> Result<Vec<String>, String> {
        self.transfer_with(id, secrets::get_vault_secret_result, secrets::set_secret)
    }

    fn transfer_with(
        &self,
        id: &str,
        mut read: impl FnMut(&str, &str) -> Result<Option<String>, String>,
        mut write: impl FnMut(&str, &str, &str) -> Result<(), String>,
    ) -> Result<Vec<String>, String> {
        let mut missing = Vec::new();
        for (key, imported) in &self.values {
            let current = read(id, key).map_err(|_| VAULT_FAILURE.to_string())?;
            let Some(value) = imported.as_deref().filter(|v| provided(v)) else {
                let required = self.entry.launch.as_ref().is_none_or(|launch| {
                    launch
                        .inputs
                        .iter()
                        .find(|input| &input.key == key)
                        .map(|input| input.required)
                        .unwrap_or_else(|| launch.required_env.contains(key))
                });
                if required && current.is_none_or(|v| !provided(&v)) {
                    missing.push(key.clone());
                }
                continue;
            };
            if let Some(current) = current {
                if current != value {
                    return Err("Imported credentials differ from the saved values. Resolve them in Credentials and review again. Client config unchanged.".into());
                }
                continue;
            }
            write(id, key, value).map_err(|_| VAULT_FAILURE.to_string())?;
            if read(id, key)
                .map_err(|_| VAULT_FAILURE.to_string())?
                .as_deref()
                != Some(value)
            {
                return Err(VAULT_FAILURE.into());
            }
        }
        Ok(missing)
    }
}

pub(crate) fn ready(server: &ServerEntry) -> Result<bool, String> {
    for env in &server.env {
        if env.secret
            && env.value.is_none()
            && server
                .launch
                .as_ref()
                .is_none_or(|launch| launch.required_env.contains(&env.key))
        {
            let value = secrets::get_vault_secret_result(&server.id, &env.key)
                .map_err(|_| VAULT_FAILURE.to_string())?;
            if value.is_none_or(|value| !provided(&value)) {
                return Ok(false);
            }
        }
    }
    if let Some(launch) = &server.launch {
        for input in launch.inputs.iter().filter(|i| i.secret && i.required) {
            if secrets::get_vault_secret_result(&server.id, &input.key)
                .map_err(|_| VAULT_FAILURE.to_string())?
                .is_none_or(|value| !provided(&value))
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(remote: bool) -> ServerEntry {
        serde_json::from_value(json!({"id":"imported","name":"Synthetic","transport":if remote {"http"} else {"stdio"},"command":if remote {None} else {Some("mock")},"args":[],"url":if remote {Some("https://example.invalid/mcp?token=synthetic-url-secret")} else {None},"env":[{"key":if remote {"Authorization"} else {"PAT"},"secret":true}]})).unwrap()
    }

    #[test]
    fn reviewed_values_are_references_and_transfer_once() {
        let mut raw = entry(false);
        raw.args = vec!["--token".into(), "synthetic-argument-secret".into()];
        let import =
            Import::prepare(raw, Some(&json!({"env":{"PAT":"synthetic-pat-secret"}}))).unwrap();
        let serialized = serde_json::to_string(&import.entry).unwrap();
        for secret in ["synthetic-pat-secret", "synthetic-argument-secret"] {
            assert!(!serialized.contains(secret));
        }
        assert_eq!(import.entry.args[1], "<launch-input>");
        let vault = std::cell::RefCell::new(std::collections::HashMap::new());
        let writes = std::cell::Cell::new(0);
        for _ in 0..2 {
            assert!(import
                .transfer_with(
                    "imported",
                    |_, key| Ok(vault.borrow().get(key).cloned()),
                    |_, key, value| {
                        writes.set(writes.get() + 1);
                        vault
                            .borrow_mut()
                            .insert(key.to_string(), value.to_string());
                        Ok(())
                    }
                )
                .unwrap()
                .is_empty());
        }
        assert_eq!(writes.get(), 2);
    }

    #[test]
    fn existing_launch_bindings_and_optional_env_remain_usable() {
        let mut server = entry(false);
        server.args = vec!["--token".into(), "<launch-input>".into()];
        server.env[0].key = "OPTIONAL".into();
        server.launch = Some(serde_json::from_value(json!({
            "inputs":[{"key":"TOKEN","label":"Token","secret":true,"required":true,"value":"synthetic-launch-secret"}],
            "bindings":[{"index":1,"parts":[{"kind":"input","key":"TOKEN"}]}],"requiredEnv":[]
        })).unwrap());
        let import = Import::prepare(server, None).unwrap();
        let vault = std::cell::RefCell::new(std::collections::HashMap::new());
        assert!(import
            .transfer_with(
                "imported",
                |_, key| Ok(vault.borrow().get(key).cloned()),
                |_, key, value| {
                    vault
                        .borrow_mut()
                        .insert(key.to_string(), value.to_string());
                    Ok(())
                }
            )
            .unwrap()
            .is_empty());
        let resolved = crate::launch_inputs::resolve_args_with(&import.entry, |_, key| {
            Ok(vault.borrow().get(key).cloned())
        })
        .unwrap();
        assert_eq!(resolved.args, ["--token", "synthetic-launch-secret"]);
        assert_eq!(import.entry.launch.as_ref().unwrap().bindings.len(), 1);
        assert!(!serde_json::to_string(&import.entry)
            .unwrap()
            .contains("synthetic-launch-secret"));
    }

    #[test]
    fn remote_pat_and_url_are_vaulted_without_double_bearer() {
        let import = Import::prepare(
            entry(true),
            Some(&json!({"headers":{"Authorization":"Bearer synthetic-pat-secret"}})),
        )
        .unwrap();
        assert_eq!(
            import.entry.url.as_deref(),
            Some("https://example.invalid/mcp")
        );
        let serialized = serde_json::to_string(&import.entry).unwrap();
        assert!(!serialized.contains("synthetic"));
        assert!(import.values.contains(&(
            secrets::HTTP_AUTH_KEY.into(),
            Some("synthetic-pat-secret".into())
        )));
        assert!(import
            .values
            .iter()
            .any(|(key, value)| key == secrets::IMPORTED_URL_KEY
                && value.as_ref().unwrap().contains("synthetic-url-secret")));
    }

    #[test]
    fn missing_input_and_vault_failure_are_distinct_and_opaque() {
        let missing =
            Import::prepare(entry(false), Some(&json!({"env":{"PAT":"${PAT}"}}))).unwrap();
        assert_eq!(
            missing
                .transfer_with(
                    "imported",
                    |_, _| Ok(None),
                    |_, _, _| panic!("placeholder is not a credential")
                )
                .unwrap(),
            ["PAT"]
        );
        let import = Import::prepare(
            entry(false),
            Some(&json!({"env":{"PAT":"synthetic-pat-secret"}})),
        )
        .unwrap();
        let error = import
            .transfer_with(
                "imported",
                |_, _| Ok(None),
                |_, _, _| Err("provider echoed synthetic-pat-secret".into()),
            )
            .unwrap_err();
        assert_eq!(error, VAULT_FAILURE);
        assert!(!error.contains("synthetic-pat-secret"));
        assert_eq!(
            import
                .transfer_with(
                    "imported",
                    |_, _| Err("private provider error".into()),
                    |_, _, _| panic!("read failed")
                )
                .unwrap_err(),
            VAULT_FAILURE
        );
    }

    #[test]
    fn detected_serialization_masks_url_and_arguments() {
        let server = crate::clients::McpServer {
            name: "Synthetic".into(),
            transport: "http".into(),
            command: None,
            args: vec!["--token=synthetic-argument-secret".into()],
            url: Some(
                "https://user:synthetic-password@example.invalid/mcp?token=synthetic-url-secret"
                    .into(),
            ),
            env_keys: vec!["PAT".into()],
        };
        let ui = serde_json::to_string(&server).unwrap();
        assert!(!ui.contains("synthetic"));
        assert!(ui.contains("<launch-input>"));
    }

    #[test]
    fn unsupported_headers_are_refused_before_transfer() {
        assert!(Import::prepare(
            entry(true),
            Some(&json!({"headers":{"X-API-Key":"synthetic-pat-secret"}}))
        )
        .err()
        .unwrap()
        .contains("custom HTTP headers"));
    }
}
