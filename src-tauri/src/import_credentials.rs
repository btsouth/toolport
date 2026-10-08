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
    static PLACEHOLDER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^(\$\{[A-Za-z_][A-Za-z0-9_]*\}|<(?i:your[-_ ]|replace[-_ ])[A-Za-z0-9_ -]+>)$",
        )
        .unwrap()
    });
    !value.is_empty()
        && !PLACEHOLDER.is_match(value)
        && !matches!(
            value.to_ascii_uppercase().as_str(),
            "YOUR_API_KEY" | "YOUR_TOKEN" | "REPLACE_ME"
        )
}

pub(crate) fn secret_env(key: &str, value: Option<&str>) -> bool {
    let key = key.to_ascii_uppercase();
    if key == "PATH" {
        return false;
    }
    if [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASS",
        "AUTH",
        "CREDENTIAL",
        "BEARER",
        "COOKIE",
        "PRIVATE",
        "PAT",
    ]
    .iter()
    .any(|needle| {
        key.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == *needle)
    }) {
        return true;
    }
    let Some(value) = value else {
        return false;
    };
    if ["sk-", "ghp_", "github_pat_", "xox", "AKIA", "-----BEGIN"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
    {
        return true;
    }
    static PASSWORD: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)(password|pwd)\s*=").unwrap());
    if url::Url::parse(value).is_ok_and(|url| {
        !url.username().is_empty() || url.password().is_some() || url.query().is_some()
    }) || PASSWORD.is_match(value)
        || value.len() >= 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return true;
    }
    if value.len() < 20 || value.contains(char::is_whitespace) {
        return false;
    }
    let mut counts = std::collections::HashMap::new();
    for byte in value.bytes() {
        *counts.entry(byte).or_insert(0usize) += 1;
    }
    let entropy = counts
        .values()
        .map(|count| {
            let p = *count as f64 / value.len() as f64;
            -p * p.log2()
        })
        .sum::<f64>();
    entropy >= 4.0
}

/// Vault changes are prepared before registry/config locks and undone on every error.
#[derive(Default)]
pub(crate) struct VaultWrites {
    receipts: Vec<(String, String, Option<String>, String)>,
}
impl VaultWrites {
    fn write(&mut self, id: &str, key: &str, value: &str) -> Result<(), String> {
        let previous = secrets::get_vault_secret_result(id, key).map_err(|_| VAULT_FAILURE)?;
        self.receipts
            .push((id.into(), key.into(), previous, value.into()));
        secrets::set_secret(id, key, value).map_err(|_| VAULT_FAILURE.into())
    }
    pub(crate) fn keep(&mut self) {
        self.receipts.clear();
    }
    fn rollback(&mut self) -> Result<(), String> {
        let mut failed = false;
        for (id, key, previous, written) in self.receipts.drain(..).rev() {
            match secrets::get_vault_secret_result(&id, &key) {
                Ok(current) if current.as_deref() == Some(&written) => {
                    let result = match previous {
                        Some(value) => secrets::set_secret(&id, &key, &value),
                        None => secrets::delete_secret(&id, &key),
                    };
                    failed |= result.is_err();
                }
                Ok(current) if current == previous => {}
                _ => failed = true,
            }
        }
        if failed {
            Err("Could not undo every keychain change. Unlock Credentials and review before retrying.".into())
        } else {
            Ok(())
        }
    }
}
pub(crate) fn transaction<T>(
    action: impl FnOnce(&mut VaultWrites) -> Result<T, String>,
) -> Result<T, String> {
    let mut writes = VaultWrites::default();
    match action(&mut writes) {
        Ok(result) => Ok(result),
        Err(error) => match writes.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!("{error} {rollback}")),
        },
    }
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
    pub(crate) fn review_credentials(&self) -> Vec<(String, bool, bool)> {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), true, value.as_deref().is_some_and(provided)))
            .chain(self.entry.env.iter().filter(|env| !env.secret).map(|env| {
                (
                    env.key.clone(),
                    false,
                    env.value.as_deref().is_some_and(provided),
                )
            }))
            .collect()
    }

    /// Missing values entered in review stay in memory until the transaction succeeds.
    pub(crate) fn supply(
        &mut self,
        inputs: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        for (key, value) in inputs {
            if !provided(value) {
                return Err("Enter a credential value before retrying.".into());
            }
            if let Some((_, saved)) = self.values.iter_mut().find(|(name, _)| name == key) {
                *saved = Some(value.clone());
            } else if let Some(env) = self
                .entry
                .env
                .iter_mut()
                .find(|env| &env.key == key && !env.secret && env.value.is_none())
            {
                env.value = Some(value.clone());
            } else {
                return Err("Credential is not part of this reviewed server. Review again.".into());
            }
        }
        Ok(())
    }

    pub(crate) fn prepare(
        entry: ServerEntry,
        definition: Option<&serde_json::Value>,
    ) -> Result<Self, String> {
        Self::prepare_with_choices(entry, definition, None)
    }

    pub(crate) fn prepare_with_choices(
        mut entry: ServerEntry,
        definition: Option<&serde_json::Value>,
        choices: Option<&std::collections::BTreeMap<String, bool>>,
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
        let mut plain = Vec::new();
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
            let secret = choices
                .and_then(|c| c.get(&env.key))
                .copied()
                .unwrap_or_else(|| {
                    if definition.is_none() {
                        env.secret
                    } else {
                        secret_env(&env.key, value.as_deref())
                    }
                });
            if secret || env.key.eq_ignore_ascii_case("authorization") {
                values.push((env.key.clone(), value));
            } else {
                let mut env = env.clone();
                env.secret = false;
                env.value = value.filter(|v| provided(v));
                plain.push(env);
            }
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
                    // Older builds preserve unknown metadata but never use it for auth.
                    entry.unknown_fields.insert(
                        "importedUrlKey".into(),
                        serde_json::json!(secrets::IMPORTED_URL_KEY),
                    );
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
            .filter(|(key, _)| {
                key != secrets::IMPORTED_URL_KEY && !launch.inputs.iter().any(|i| &i.key == key)
            })
            .map(|(key, _)| registry::EnvVar {
                key: key.clone(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            })
            .collect();
        entry.env.extend(plain);
        if !had_launch {
            launch.required_env = entry.env.iter().map(|e| e.key.clone()).collect();
        }
        if had_launch || !launch.inputs.is_empty() || !launch.required_env.is_empty() {
            entry.launch = Some(launch);
        }
        Ok(Self { entry, values })
    }

    pub(crate) fn updates(&self, existing: &ServerEntry) -> Vec<String> {
        let mut fields = Vec::new();
        for (label, left, right) in [
            (
                "Environment",
                serde_json::to_value(&existing.env).unwrap(),
                serde_json::to_value(&self.entry.env).unwrap(),
            ),
            (
                "Launch settings",
                serde_json::to_value(&existing.launch).unwrap(),
                serde_json::to_value(&self.entry.launch).unwrap(),
            ),
            (
                "Arguments",
                serde_json::to_value(&existing.args).unwrap(),
                serde_json::to_value(&self.entry.args).unwrap(),
            ),
            (
                "URL",
                serde_json::to_value(&existing.url).unwrap(),
                serde_json::to_value(&self.entry.url).unwrap(),
            ),
        ] {
            if left != right {
                fields.push(label.into());
            }
        }
        if self.values.iter().any(|(key, value)| {
            value.as_deref().is_some_and(provided)
                && secrets::get_vault_secret_result(&existing.id, key)
                    .ok()
                    .flatten()
                    != *value
        }) {
            fields.push("Credentials".into());
        }
        fields
    }

    pub(crate) fn matches(&self, existing: &ServerEntry) -> Result<bool, String> {
        if existing.command != self.entry.command || existing.transport != self.entry.transport {
            return Ok(false);
        }
        let args = |server: &ServerEntry, imported: bool| {
            crate::launch_inputs::resolve_args_with(server, |id, key| {
                if imported {
                    if let Some((_, value)) = self.values.iter().find(|(name, _)| name == key) {
                        return Ok(value.clone());
                    }
                }
                secrets::get_vault_secret_result(id, key)
            })
            .map(|args| args.args)
        };
        if args(existing, false)? != args(&self.entry, true)? {
            return Ok(false);
        }
        let existing_url = if has_imported_url(existing) {
            secrets::get_vault_secret_result(&existing.id, secrets::IMPORTED_URL_KEY)
                .map_err(|_| VAULT_FAILURE)?
                .or(existing.url.clone())
        } else {
            existing.url.clone()
        };
        let imported_url = self
            .values
            .iter()
            .find(|(key, _)| key == secrets::IMPORTED_URL_KEY)
            .and_then(|(_, value)| value.clone())
            .or(self.entry.url.clone());
        Ok(existing_url == imported_url)
    }

    pub(crate) fn transfer_into(
        &self,
        id: &str,
        writes: &mut VaultWrites,
        existing: bool,
    ) -> Result<Vec<String>, String> {
        self.transfer_with(
            id,
            existing,
            secrets::get_vault_secret_result,
            |id, key, value| writes.write(id, key, value),
        )
    }

    fn transfer_with(
        &self,
        id: &str,
        existing: bool,
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
                if required && (!existing || current.is_none_or(|v| !provided(&v))) {
                    missing.push(key.clone());
                }
                continue;
            };
            if existing && current.as_deref() == Some(value) {
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
        for env in self.entry.env.iter().filter(|env| !env.secret) {
            if env.value.as_deref().is_none_or(|value| !provided(value))
                && self
                    .entry
                    .launch
                    .as_ref()
                    .is_some_and(|launch| launch.required_env.contains(&env.key))
            {
                missing.push(env.key.clone());
            }
        }
        Ok(missing)
    }
}

pub(crate) fn has_secrets(server: &ServerEntry) -> bool {
    has_imported_url(server)
        || server.env.iter().any(|env| env.secret)
        || server
            .launch
            .as_ref()
            .is_some_and(|launch| launch.inputs.iter().any(|input| input.secret))
}

pub(crate) fn has_imported_url(server: &ServerEntry) -> bool {
    server
        .unknown_fields
        .get("importedUrlKey")
        .and_then(|value| value.as_str())
        == Some(secrets::IMPORTED_URL_KEY)
        || server
            .env
            .iter()
            .any(|env| env.key == secrets::IMPORTED_URL_KEY)
}

pub(crate) fn ready(server: &ServerEntry) -> Result<bool, String> {
    if has_imported_url(server)
        && secrets::get_vault_secret_result(&server.id, secrets::IMPORTED_URL_KEY)
            .map_err(|_| VAULT_FAILURE.to_string())?
            .is_none_or(|value| !provided(&value))
    {
        return Ok(false);
    }
    for env in &server.env {
        if !env.secret
            && env.value.as_deref().is_none_or(|value| !provided(value))
            && server
                .launch
                .as_ref()
                .is_some_and(|launch| launch.required_env.contains(&env.key))
        {
            return Ok(false);
        }
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
    fn reviewed_secret_heuristics_cover_connection_strings_and_word_names() {
        for (key, value) in [
            ("DATABASE_URL", "postgres://u:p@host/db"),
            ("CACHE", "redis://:pw@h:6379"),
            (
                "CONNECTION",
                "Server=h; Password = private value; Database=d",
            ),
            ("CONNECTION", "Server=h;Pwd=secret"),
            ("ENDPOINT", "https://host/mcp?v=1"),
            ("VALUE", "0123456789abcdef0123456789abcdef"),
            ("API_KEY", "small"),
            ("PAT", "small"),
        ] {
            assert!(secret_env(key, Some(value)), "{key} was left plain");
        }
        for key in ["PATH", "MONKEY", "KEYBOARD", "COMPASS"] {
            assert!(
                !secret_env(key, Some("/usr/bin")),
                "{key} was falsely vaulted"
            );
        }
    }

    #[test]
    fn reviewed_url_reference_is_ignored_by_legacy_bearer_selection() {
        let import = Import::prepare(entry(true), None).unwrap();
        // v1.24.0 remote.rs:955-964 only examines secret env entries with no value.
        assert!(!import
            .entry
            .env
            .iter()
            .any(|e| e.secret && e.value.is_none() && e.key == secrets::IMPORTED_URL_KEY));
    }

    #[test]
    fn exact_placeholders_do_not_reject_real_secret_prefixes() {
        for value in ["$actual-secret", "{real-secret}", "<actual-secret>"] {
            assert!(provided(value), "{value}");
        }
        for value in ["${TOKEN}", "<your-key>", ""] {
            assert!(!provided(value), "{value}");
        }
    }

    #[test]
    fn ordinary_environment_values_stay_plain() {
        let import = Import::prepare(
            entry(false),
            Some(&json!({"env":{"PAT":"synthetic-pat-secret","PORT":3000}})),
        )
        .unwrap();
        let port = import
            .entry
            .env
            .iter()
            .find(|env| env.key == "PORT")
            .unwrap();
        assert!(!port.secret);
        assert_eq!(port.value.as_deref(), Some("3000"));
    }

    #[test]
    fn ordinary_missing_environment_values_stay_off_until_supplied() {
        let mut import = Import::prepare(
            entry(false),
            Some(&json!({"env":{"PAT":"synthetic-pat","PORT":"${PORT}"}})),
        )
        .unwrap();
        let vault = std::cell::RefCell::new(std::collections::HashMap::new());
        let missing = import
            .transfer_with(
                "imported",
                true,
                |_, key| Ok(vault.borrow().get(key).cloned()),
                |_, key, value| {
                    vault
                        .borrow_mut()
                        .insert(key.to_string(), value.to_string());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(missing, ["PORT"]);
        import
            .supply(&std::collections::BTreeMap::from([(
                "PORT".into(),
                "3000".into(),
            )]))
            .unwrap();
        assert_eq!(
            import
                .entry
                .env
                .iter()
                .find(|env| env.key == "PORT")
                .unwrap()
                .value
                .as_deref(),
            Some("3000")
        );
        assert!(!vault.borrow().contains_key("PORT"));
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
                    true,
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
                true,
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
                    true,
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
                true,
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
                    true,
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
