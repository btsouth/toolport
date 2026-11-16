//! One Toolport entry used by several clients that read the same config file.
//! Membership is derived from receipts, registry records and the entry itself.
use super::*;

#[cfg(test)]
thread_local! {
    /// Adds Cline to the Devin pair, for the three-member tests.
    pub(crate) static THREE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Clients whose definitions may share one Toolport entry in one file.
pub(crate) fn shares_entry_with(id: &str) -> &'static [&'static str] {
    #[cfg(test)]
    if THREE.with(std::cell::Cell::get) {
        return match id {
            "windsurf" => &["devin-cli", "cline"],
            "devin-cli" => &["windsurf", "cline"],
            "cline" => &["windsurf", "devin-cli"],
            _ => &[],
        };
    }
    match id {
        "windsurf" => &["devin-cli"],
        "devin-cli" => &["windsurf"],
        _ => &[],
    }
}

/// Outermost lock for every write that touches `path`.
pub(crate) fn lock(path: &Path) -> Result<crate::registry_controller::AuthMutationLock, String> {
    crate::registry_controller::acquire_auth_owner_lock(&format!(
        "client-config-path:{}",
        crate::registry::sha256_hex(&backup_storage::path_identity(path))
    ))
}

/// The path lock for a client's resolved config file, when it has one.
pub(crate) fn lock_client(
    id: &str,
) -> Result<Option<crate::registry_controller::AuthMutationLock>, String> {
    let managed = crate::registry_controller::registry_for_disconnect()
        .map(|registry| registry.client_managed_entries)
        .unwrap_or_default();
    own_path(id, &managed).map(|path| lock(&path)).transpose()
}

/// The file `id` works on: its record's saved path when that counts, else the
/// path it resolves to here.
fn own_path(id: &str, managed: &HashMap<String, ManagedEntry>) -> Option<PathBuf> {
    let resolved = find_def(id).and_then(|def| resolved_definition_path(&def).ok());
    managed
        .get(id)
        .and_then(|record| record.config_path.as_deref())
        .map(PathBuf::from)
        .filter(|saved| managed.get(id).is_some_and(|r| record_on(id, r, saved)))
        .or(resolved)
}

pub(crate) fn name(id: &str) -> String {
    find_def(id).map_or_else(|| id.to_string(), |def| def.name.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Receipt {
    Live,
    Broken,
    Released,
    /// A Discard tombstone: no capture time.
    Discarded,
}

fn receipt(id: &str, path: &Path) -> Option<Receipt> {
    let file = restore::record_path(id, path).ok()?;
    let raw = match std::fs::read_to_string(&file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let raw = std::fs::read_to_string(restore::released_path(id, path).ok()?).ok()?;
            let record: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
            return Some(if record["capturedAt"] == 0 {
                Receipt::Discarded
            } else {
                Receipt::Released
            });
        }
        Err(_) => return Some(Receipt::Broken),
    };
    Some(match backup_storage::raw_metadata(&raw) {
        Ok((record, _)) if record["configPath"] != path.to_string_lossy().as_ref() => {
            Receipt::Broken
        }
        Ok((record, _))
            if record["disconnected"] == true && record["disconnectBefore"].is_null() =>
        {
            Receipt::Released
        }
        Ok(_) => Receipt::Live,
        Err(_) => Receipt::Broken,
    })
}

fn holders(path: &Path, live: bool) -> Vec<String> {
    machine_defs(false)
        .iter()
        .map(|def| def.client_id().to_string())
        .filter(|id| {
            let state = receipt(id, path);
            if live {
                matches!(state, Some(Receipt::Live | Receipt::Broken))
            } else {
                state == Some(Receipt::Discarded)
            }
        })
        .collect()
}

/// The file a registry record belongs to. A saved path counts only when this
/// client resolves to it here or a live receipt names it.
fn record_on(id: &str, record: &ManagedEntry, path: &Path) -> bool {
    let resolved = find_def(id).and_then(|def| resolved_definition_path(&def).ok());
    let saved = record
        .config_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|saved| {
            resolved
                .as_deref()
                .is_some_and(|resolved| locations::same_path(resolved, saved))
                || !holders(saved, true).is_empty()
        });
    saved
        .or(resolved)
        .is_some_and(|own| locations::same_path(&own, path))
}

pub(crate) struct Group {
    pub(crate) path: PathBuf,
    format: Format,
    /// The client whose live `original-` receipt is for this path.
    pub(crate) owner: Option<String>,
    broken: bool,
    /// Clients with a registry record for this path, plus the owner.
    pub(crate) members: Vec<String>,
    /// The client the entry names.
    pub(crate) carrier: Option<String>,
    /// Members with a record while the entry matches the carrier's record.
    pub(crate) connected: Vec<String>,
    pub(crate) shared_http: bool,
    pub(crate) profile: Option<String>,
    entry: Option<McpServer>,
}

/// The group for `id`'s file, or `None` when main's rules apply: the client
/// declares no share, the file has two live receipts, or another client that
/// does not share an entry with it is involved.
pub(crate) fn derive(
    id: &str,
    managed: &HashMap<String, ManagedEntry>,
) -> Result<Option<Group>, String> {
    derive_at(id, managed, None)
}

fn derive_at(
    id: &str,
    managed: &HashMap<String, ManagedEntry>,
    at: Option<&Path>,
) -> Result<Option<Group>, String> {
    let partners = shares_entry_with(id);
    let def = match find_def(id).filter(|def| def.location.is_none()) {
        Some(def) if !partners.is_empty() => def,
        _ => return Ok(None),
    };
    let allowed = |other: &str| other == id || partners.contains(&other);
    let Some(path) = at.map(Path::to_path_buf).or_else(|| own_path(id, managed)) else {
        return Ok(None);
    };
    let live = holders(&path, true);
    if live.len() > 1 || live.iter().any(|other| !allowed(other)) {
        return Ok(None);
    }
    let mut members = Vec::new();
    for (other, record) in managed {
        if record_on(other, record, &path) {
            if !allowed(other) {
                return Ok(None);
            }
            members.push(other.clone());
        }
    }
    let owner = live.first().cloned();
    if let Some(owner) = owner.as_ref().filter(|owner| !members.contains(owner)) {
        members.push(owner.clone());
    }
    members.sort();
    let text = if mutation::exists(&path) {
        Some(read_config_file(&path)?)
    } else {
        None
    };
    let servers = match text.as_deref() {
        Some(text) => parse_client_content(def.format, text)?,
        None => Vec::new(),
    };
    let entry = owned_gateway_entry(&servers).cloned();
    let env = |key, legacy| {
        let (entry, text) = entry.as_ref().zip(text.as_deref())?;
        entry_env_value(def.format, &entry.name, text, key, legacy)
            .ok()
            .flatten()
    };
    let carrier = env(crate::brand::CLIENT_ID, crate::brand::CLIENT_ID_LEGACY);
    if carrier.as_deref().is_some_and(|carrier| !allowed(carrier)) {
        return Ok(None);
    }
    let record = carrier.as_deref().and_then(|carrier| managed.get(carrier));
    let matches = entry
        .as_ref()
        .zip(record)
        .is_some_and(|(entry, record)| managed_matches_detected(entry, record));
    Ok(Some(Group {
        connected: members
            .iter()
            .filter(|member| matches && managed.contains_key(*member))
            .cloned()
            .collect(),
        shared_http: record.is_some_and(|record| record.transport == "sharedHttp")
            || entry.as_ref().is_some_and(|entry| entry.url.is_some()),
        profile: env(crate::brand::PROFILE, crate::brand::PROFILE_LEGACY),
        broken: owner
            .as_ref()
            .is_some_and(|owner| receipt(owner, &path) == Some(Receipt::Broken)),
        path,
        format: def.format,
        owner,
        members,
        carrier,
        entry,
    }))
}

/// Section 7: a client with nothing on `id`'s file may not connect to a file
/// another client already uses unless their definitions share an entry.
pub(crate) fn refuse_new_share(
    id: &str,
    managed: &HashMap<String, ManagedEntry>,
) -> Result<(), String> {
    let Some(def) = find_def(id) else {
        return Ok(());
    };
    let Ok(path) = resolved_definition_path(&def) else {
        return Ok(());
    };
    if managed
        .get(id)
        .is_some_and(|record| record_on(id, record, &path))
        || matches!(receipt(id, &path), Some(Receipt::Live | Receipt::Broken))
    {
        return Ok(());
    }
    let entry_owner = || {
        let text = read_config_file(&path).ok()?;
        let servers = parse_client_content(def.format, &text).ok()?;
        let entry = owned_gateway_entry(&servers)?;
        let carrier = location_launch_id(def.format, &entry.name, &text).ok()??;
        let resolved = find_def(&carrier).and_then(|def| resolved_definition_path(&def).ok())?;
        locations::same_path(&resolved, &path).then_some(carrier)
    };
    let other = holders(&path, true)
        .into_iter()
        .find(|other| other != id)
        .or_else(|| {
            let mut others: Vec<_> = managed
                .iter()
                .filter(|(other, record)| *other != id && record_on(other, record, &path))
                .map(|(other, _)| other.clone())
                .collect();
            others.sort();
            others.into_iter().next()
        })
        .or_else(|| entry_owner().filter(|other| other != id));
    match other {
        Some(other)
            if def.location.is_some() || !shares_entry_with(id).contains(&other.as_str()) =>
        {
            let other = name(&other);
            Err(crate::i18n::trf(
                "{other} already uses {file}. Toolport can connect only one of them to that file for now. Disconnect {other} first, or point one of them at another file.",
                &[("other", &other), ("file", &path.display().to_string())],
            ))
        }
        _ => Ok(()),
    }
}

impl Group {
    pub(crate) fn file(&self) -> String {
        self.path.display().to_string()
    }

    /// Refuse when the owner's receipt cannot be used safely for a write.
    pub(crate) fn check_owner(&self, client: &str) -> Result<(), String> {
        let Some(owner) = self.owner.as_deref() else {
            return Ok(());
        };
        let values = [
            ("owner", name(owner)),
            ("client", name(client)),
            ("file", self.file()),
        ];
        let values: Vec<_> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
        backup_storage::reset_missing();
        let unreadable = self.broken || restore::prepare_disconnect(owner, &self.path).is_err();
        if unreadable {
            return Err(crate::i18n::trf("Toolport can't read the recovery file for {file}, which {client} shares, so it won't change that file. Repair or discard the recovery file first. Nothing was changed.", &values));
        }
        if backup_storage::missing() {
            return Err(crate::i18n::tr(
                "Unlock your keyring, then try again. Nothing was changed.",
            ));
        }
        if restore::pending_values(owner, &self.path).unwrap_or(true) {
            return Err(crate::i18n::trf("{owner} still has saved recovery values for {file}, which {client} shares. Choose Retry on {owner}'s recovery to put them back, or Discard these saved values, first. Nothing was changed.", &values));
        }
        Ok(())
    }

    /// The entry's state judged against the record of the client it names.
    pub(crate) fn carrier_state(
        &self,
        managed: &HashMap<String, ManagedEntry>,
    ) -> GatewayEntryState {
        entry_state_for(
            self.entry.as_ref(),
            self.carrier
                .as_deref()
                .and_then(|carrier| managed.get(carrier)),
        )
    }

    /// Point the entry at `member` against the owner's receipt (section 4).
    pub(crate) fn rewrite(
        &self,
        member: &str,
        profile: Option<&str>,
    ) -> Result<WriteOutcome, String> {
        write_entry(
            self.owner.as_deref(),
            member,
            &self.path,
            self.format,
            &gateway_entry(profile, member)?,
        )
    }

    /// The last member out restores the owner's original file. The caller
    /// releases the receipt only after its registry write succeeds.
    pub(crate) fn restore_owner(
        &self,
        owner: &str,
        managed: Option<&ManagedEntry>,
    ) -> Result<WriteOutcome, String> {
        disconnect::restore_write(owner, &self.path, self.format, managed)
    }
}

/// Write `entry` into `path` as the owner: its folder holds the backup and its
/// receipt is updated. With no owner receipt nothing is recorded.
pub(super) fn write_entry(
    owner: Option<&str>,
    member: &str,
    path: &Path,
    format: Format,
    entry: &ServerEntry,
) -> Result<WriteOutcome, String> {
    let writer = owner.unwrap_or(member);
    let write = || {
        mutation::run(writer, path, format, || {
            revision_outcome(writer, path, || {
                let backup = backup_file(writer, path)?;
                edit_format(format, path, Some(entry), config_is_whole_app_state(writer))?;
                let mut outcome = WriteOutcome::unchanged(path);
                outcome.backup = backup.map(|backup| backup.display().to_string());
                outcome.managed = Some(ManagedEntry::from_gateway_entry(entry));
                Ok(outcome)
            })
        })
    };
    if owner.is_some() {
        return write();
    }
    let mut outcome = mutation::without_receipt(write)?;
    outcome.recovery_path = None;
    Ok(outcome)
}

/// Section 6.9: a joined member shows the carrier's entry, and a partner with no
/// record is not connected by an entry that names someone else.
pub(super) fn classify(clients: &mut [DetectedClient], managed: &HashMap<String, ManagedEntry>) {
    for client in clients.iter_mut() {
        if shares_entry_with(&client.id).is_empty() || client.error.is_some() {
            continue;
        }
        let Ok(Some(group)) = derive(&client.id, managed) else {
            continue;
        };
        match group.carrier.as_deref() {
            Some(carrier) if carrier != client.id => {
                let member = managed.contains_key(&client.id) && group.members.contains(&client.id);
                client.entry_state = if member {
                    group.carrier_state(managed)
                } else {
                    GatewayEntryState::Absent
                };
                client.gateway_installed = client.entry_state != GatewayEntryState::Absent;
                if client.entry_state == GatewayEntryState::Managed {
                    client.shared = Some(SharedEntry {
                        joined: true,
                        with: name(carrier),
                    });
                }
            }
            Some(carrier) => {
                let others: Vec<_> = group
                    .connected
                    .iter()
                    .filter(|member| *member != carrier)
                    .map(|member| name(member))
                    .collect();
                if !others.is_empty() {
                    client.shared = Some(SharedEntry {
                        joined: false,
                        with: others.join(", "),
                    });
                }
            }
            None => {}
        }
    }
}

/// Clients `--disconnect-all` should skip with an unreadable registry, each
/// with the receipt owner that restores their shared file.
pub(super) fn restored_with(ids: &[String]) -> HashMap<String, String> {
    let mut skipped = HashMap::new();
    for id in ids {
        if let Ok(Some(group)) = derive(id, &HashMap::new()) {
            if let Some(owner) = group.owner.filter(|owner| owner != id) {
                skipped.insert(id.clone(), owner);
            }
        }
    }
    skipped
}

/// Whether any other client has a record or receipt for `path` (section 11a).
pub(super) fn others_use(id: &str, path: &Path) -> Result<bool, String> {
    let current = crate::registry_controller::registry_for_disconnect()?;
    Ok(holders(path, true).iter().any(|other| other != id)
        || current
            .client_managed_entries
            .iter()
            .any(|(other, record)| other != id && record_on(other, record, path)))
}

/// Whether `id` or a client sharing its entry discarded its recovery for `path`.
pub(super) fn discarded(id: &str, path: &Path) -> bool {
    holders(path, false)
        .iter()
        .any(|other| other == id || shares_entry_with(id).contains(&other.as_str()))
}

/// Give `to` the access settings of `from`, so the running apps keep theirs.
pub(crate) fn copy_access(registry: &mut crate::registry::Registry, from: &str, to: &str) {
    match registry.client_scopes.get(from).cloned() {
        Some(scope) => registry.client_scopes.insert(to.into(), scope),
        None => registry.client_scopes.remove(to),
    };
    match registry.client_discovery.get(from).cloned() {
        Some(mode) => registry.client_discovery.insert(to.into(), mode),
        None => registry.client_discovery.remove(to),
    };
}

/// The record a joining client gets: the carrier's, bound to the shared file.
pub(crate) fn join_record(group: &Group, carrier: &ManagedEntry) -> ManagedEntry {
    let mut record = carrier.clone();
    record.config_path = Some(group.path.to_string_lossy().into_owned());
    record.updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    record
}

/// Section 6.10: a client whose entry moved out of a legacy file into a file
/// whose entry another client carries joins that entry instead of writing.
pub(super) fn join_relocated(
    id: &str,
    managed: &HashMap<String, ManagedEntry>,
) -> Result<Option<ManagedEntry>, String> {
    let Some(path) = find_def(id).and_then(|def| resolved_definition_path(&def).ok()) else {
        return Ok(None);
    };
    let _lock = lock(&path)?;
    let Some(group) = derive_at(id, managed, Some(&path))? else {
        return Ok(None);
    };
    let Some(carrier) = group
        .carrier
        .as_deref()
        .filter(|carrier| *carrier != id && group.connected.iter().any(|m| m == carrier))
        .filter(|_| !group.shared_http)
    else {
        return Ok(None);
    };
    let record = join_record(&group, &managed[carrier]);
    crate::registry::update(|registry| {
        registry.set_client_managed_entry(id, record.clone());
        copy_access(registry, carrier, id);
        Ok(())
    })?;
    Ok(Some(record))
}
