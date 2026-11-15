//! Read-only credential inventory followed by explicit account-scoped removal.
//!
//! Every owned item is attempted independently: one item that cannot be removed
//! never stops the others. Each item ends in exactly one list: `removed` (this
//! run deleted it), `absent` (it was already gone when deletion ran, for example
//! a legacy view of an item the data-protection pass had just deleted),
//! `preserved` (not this install's) or `leftovers` (still present, with why).
use crate::purge::Leftover;

#[derive(Default)]
pub(crate) struct Removal {
    pub removed: Vec<String>,
    /// Owned items that no longer existed when deletion ran. Not a failure.
    pub absent: Vec<String>,
    pub team_token_removed: bool,
    pub team_token_in_file: bool,
    pub preserved: Vec<String>,
    pub notes: Vec<String>,
    pub leftovers: Vec<Leftover>,
}

/// What one delete attempt established about the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// This call deleted it.
    Deleted,
    /// It did not exist (any more); nothing is left behind.
    Absent,
}

/// Accounts have no install id. Never infer ownership from the service alone.
pub(crate) struct Scope {
    pub all_installs: bool,
    pub server_ids: std::collections::HashSet<String>,
}

impl Scope {
    fn owns(&self, account: &str) -> bool {
        self.all_installs
            || account.split_once("::").is_some_and(|(id, _)| {
                id != "__toolport_internal__"
                    && id != "__conduit_master_key__"
                    && id != crate::teams::TEAM_TOKEN_SERVER
                    && id != "__toolport_http_bridge__"
                    && id != "__toolport_http_clients__"
                    && self.server_ids.contains(id)
            })
    }
}

/// Inventory every entry first (attributes only, no secret data), then delete
/// each owned entry. An entry whose attributes cannot be read is reported as a
/// leftover and skipped: its owner is unknown, so it is never deleted. It does
/// not stop the readable owned entries from being removed.
#[cfg(any(target_os = "linux", target_os = "windows", test))]
fn remove_owned<T>(
    entries: impl IntoIterator<Item = T>,
    scope: &Scope,
    mut attributes: impl FnMut(&T) -> Result<(String, String), String>,
    mut path: impl FnMut(&T) -> String,
    mut delete: impl FnMut(&T) -> Result<Outcome, String>,
) -> Removal {
    let mut report = Removal::default();
    let inventory: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            let attrs = attributes(&entry);
            (entry, attrs)
        })
        .collect();
    for (entry, attrs) in inventory {
        let path = path(&entry);
        let (service, account) = match attrs {
            Ok(attrs) => attrs,
            Err(error) => {
                report.leftovers.push(Leftover {
                    path,
                    error: format!("{error}; owner unknown, so it was not removed"),
                });
                continue;
            }
        };
        if service != super::SERVICE {
            continue;
        }
        if !scope.owns(&account) {
            report.preserved.push(account);
            continue;
        }
        record_outcome(&account, path, delete(&entry), &mut report);
    }
    report
}

fn record_outcome(
    account: &str,
    path: String,
    outcome: Result<Outcome, String>,
    report: &mut Removal,
) {
    match outcome {
        Ok(Outcome::Deleted) => record_deleted(account, path, report),
        Ok(Outcome::Absent) => report.absent.push(path),
        Err(error) => report.leftovers.push(Leftover { path, error }),
    }
}

fn record_deleted(account: &str, path: String, report: &mut Removal) {
    let team = format!(
        "{}::{}",
        crate::teams::TEAM_TOKEN_SERVER,
        crate::teams::TEAM_TOKEN_KEY
    );
    report.team_token_removed |= account == team
        || account
            .strip_prefix(&team)
            .is_some_and(|suffix| suffix.starts_with("::toolport-chunk::"));
    report.removed.push(path);
}

/// A delete call failed with `first_error`. Decide from a fresh lookup whether
/// the item is really still there: `still_present` re-queries the store and
/// returns whether the item exists now. Gone means it was already removed
/// (another view of the same item, or a concurrent delete), which is `Absent`;
/// still there keeps the original error. A failed lookup keeps both errors.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn confirm_after_failed_delete(
    first_error: String,
    still_present: impl FnOnce() -> Result<bool, String>,
) -> Result<Outcome, String> {
    match still_present() {
        Ok(false) => Ok(Outcome::Absent),
        Ok(true) => Err(first_error),
        Err(lookup) => Err(format!(
            "{first_error}; could not confirm whether it remains: {lookup}"
        )),
    }
}

/// Keychain statuses that say the item (or the keychain holding it) does not
/// exist, rather than that deletion was refused. From Security/SecBase.h:
/// errSecItemNotFound -25300, errSecInvalidItemRef -25304 ("The specified item
/// is no longer valid. It may have been deleted from the keychain."), and
/// errSecNoSuchKeychain -25294. They are only a hint: removal still re-queries
/// the keychain before calling an item absent.
#[cfg(any(target_os = "macos", test))]
pub(crate) const KEYCHAIN_NOT_FOUND: [i32; 3] = [-25300, -25304, -25294];

#[cfg(any(target_os = "macos", test))]
fn keychain_status_says_missing(status: i32) -> bool {
    KEYCHAIN_NOT_FOUND.contains(&status)
}

/// Classify one keychain delete status. 0 is deleted. A not-found status is
/// confirmed with a fresh lookup; any other status is a real failure.
#[cfg(any(target_os = "macos", test))]
fn keychain_delete_outcome(
    api: &str,
    status: i32,
    still_present: impl FnOnce() -> Result<bool, String>,
) -> Result<Outcome, String> {
    if status == 0 {
        Ok(Outcome::Deleted)
    } else if keychain_status_says_missing(status) {
        confirm_after_failed_delete(format!("{api}: {status}"), still_present)
    } else {
        Err(format!("{api}: {status}"))
    }
}

/// A legacy reference failed with a not-found `status`. `lookup` lists the
/// same account in the legacy keychain afresh: nothing there means the item is
/// already gone; anything there is a real item, deleted through its fresh
/// reference. When a fresh reference also says not found, the account is
/// listed once more: an item that is still there is a leftover, never absent.
#[cfg(any(target_os = "macos", test))]
fn legacy_recheck<R>(
    status: i32,
    mut lookup: impl FnMut() -> Result<Vec<(R, String)>, String>,
    mut delete: impl FnMut(&R) -> i32,
) -> Result<Outcome, String> {
    let unconfirmed = |lookup: String| {
        format!("SecKeychainItemDelete: {status}; could not confirm whether it remains: {lookup}")
    };
    let fresh = lookup().map_err(unconfirmed)?;
    let mut deleted = false;
    let mut missing_again = false;
    let mut refused = Vec::new();
    for (reference, _) in &fresh {
        match delete(reference) {
            0 => deleted = true,
            again if keychain_status_says_missing(again) => missing_again = true,
            again => refused.push(again),
        }
    }
    if !refused.is_empty() {
        return Err(format!(
            "SecKeychainItemDelete: {status}; the item is still in the keychain and deleting it failed: {}",
            refused.iter().map(i32::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    if missing_again {
        let remaining = lookup().map_err(unconfirmed)?;
        if !remaining.is_empty() {
            return Err(format!(
                "SecKeychainItemDelete: {status}; the item is still in the keychain although deleting it reported it missing"
            ));
        }
    }
    Ok(if deleted {
        Outcome::Deleted
    } else {
        Outcome::Absent
    })
}

/// `CredDeleteW` fails with ERROR_NOT_FOUND (1168) when no credential has that
/// target name. That is "already removed", not a failure.
#[cfg(any(target_os = "windows", test))]
fn credential_delete_outcome(succeeded: bool, last_error: u32) -> Result<Outcome, String> {
    const ERROR_NOT_FOUND: u32 = 1168;
    if succeeded {
        Ok(Outcome::Deleted)
    } else if last_error == ERROR_NOT_FOUND {
        Ok(Outcome::Absent)
    } else {
        Err(format!("CredDeleteW: {last_error}"))
    }
}

#[cfg(target_os = "linux")]
fn skipped_keyring(reason: &str) -> Removal {
    Removal { leftovers: vec![Leftover {
        path: "secret-service:conduit-mcp (credential cleanup not done)".into(),
        error: format!("{reason}. Credentials may still exist. Start or unlock your Secret Service provider, then retry; the registry was retained."),
    }], ..Removal::default() }
}

#[cfg(target_os = "linux")]
fn no_keyring(team_token_in_file: bool) -> Removal {
    Removal {
        team_token_in_file,
        notes: vec![
            "No keyring in use; encrypted file secrets are removed with the data folder.".into(),
        ],
        ..Removal::default()
    }
}

#[cfg(target_os = "linux")]
fn unavailable(error: &dbus_secret_service::Error) -> bool {
    match error {
        dbus_secret_service::Error::Unavailable => true,
        dbus_secret_service::Error::Dbus(error) => matches!(
            error.name(),
            Some(
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
                    | "org.freedesktop.DBus.Error.NoServer"
                    | "org.freedesktop.DBus.Error.FileNotFound"
            )
        ),
        _ => false,
    }
}

#[cfg(target_os = "linux")]
fn missing_bus(error: &dbus_secret_service::Error) -> bool {
    matches!(error, dbus_secret_service::Error::Dbus(error) if matches!(error.name(),
        Some("org.freedesktop.DBus.Error.NoServer" | "org.freedesktop.DBus.Error.FileNotFound")))
}

#[cfg(target_os = "linux")]
pub(crate) fn remove(scope: &Scope) -> Result<Removal, String> {
    use dbus_secret_service::{EncryptionType, SecretService};
    // Follow the real backend selector, but still inventory an available desktop
    // keyring: selecting the file backend does not remove its older OS tokens.
    let file_active = super::file::active();
    let team_token_in_file = file_active
        && super::file::get_secret_result(
            crate::teams::TEAM_TOKEN_SERVER,
            crate::teams::TEAM_TOKEN_KEY,
        )
        .ok()
        .flatten()
        .is_some();
    crate::hostenv::restore_session_bus_env();
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() }))
        });
    let desktop_runtime = runtime.exists();
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none_or(|address| address.is_empty()) {
        return Ok(if desktop_runtime {
            skipped_keyring("No session bus found in the desktop runtime directory")
        } else {
            // A genuinely headless host has neither a session bus nor runtime data.
            no_keyring(team_token_in_file)
        });
    }
    let service = match SecretService::connect_with_max_prompt_timeout(EncryptionType::Dh, 5) {
        Ok(service) => service,
        Err(error) if missing_bus(&error) && file_active && !desktop_runtime => {
            return Ok(no_keyring(team_token_in_file))
        }
        Err(error) if unavailable(&error) => {
            return Ok(skipped_keyring(&format!(
                "Secret Service is unavailable: {error}"
            )))
        }
        Err(error) => return Err(error.to_string()),
    };
    let query = std::collections::HashMap::from([("service", super::SERVICE)]);
    let entries = service
        .search_items(query.clone())
        .map_err(|error| error.to_string())?;
    // Providers disagree on the error for an item that vanished between the
    // search and the delete (NoSuchObject, UnknownObject, UnknownMethod), so a
    // failed delete is settled by searching again rather than by error name.
    let still_listed = |path: &str| -> Result<bool, String> {
        let now = service
            .search_items(query.clone())
            .map_err(|error| error.to_string())?;
        Ok(now
            .unlocked
            .iter()
            .chain(&now.locked)
            .any(|item| &*item.path == path))
    };
    let mut report = remove_owned(
        entries.unlocked.into_iter().chain(entries.locked),
        scope,
        |item| {
            item.get_attributes()
                .map_err(|error| error.to_string())
                .and_then(|attrs| {
                    Ok((
                        attrs
                            .get("service")
                            .cloned()
                            .ok_or("Missing service attribute")?,
                        attrs
                            .get("username")
                            .cloned()
                            .or_else(|| attrs.get("account").cloned())
                            .ok_or("Missing account attribute")?,
                    ))
                })
        },
        |item| format!("secret-service:{}", item.path),
        |item| match item.delete() {
            Ok(()) => Ok(Outcome::Deleted),
            Err(error) => confirm_after_failed_delete(error.to_string(), || {
                still_listed(&item.path.to_string())
            }),
        },
    );
    report.team_token_in_file = team_token_in_file;
    Ok(report)
}

#[cfg(target_os = "windows")]
pub(crate) fn remove(scope: &Scope) -> Result<Removal, String> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredEnumerateW, CredFree, CREDENTIALW, CRED_TYPE_GENERIC,
    };
    unsafe fn string(mut ptr: *const u16) -> String {
        let mut value = Vec::new();
        if !ptr.is_null() {
            while unsafe { *ptr } != 0 {
                value.push(unsafe { *ptr });
                ptr = unsafe { ptr.add(1) };
            }
        }
        String::from_utf16_lossy(&value)
    }
    let mut count = 0;
    let mut entries: *mut *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredEnumerateW(std::ptr::null(), 0, &mut count, &mut entries) } == 0 {
        let error = unsafe { GetLastError() };
        return if error == ERROR_NOT_FOUND {
            Ok(Removal::default())
        } else {
            Err(format!("CredEnumerateW: {error}"))
        };
    }
    let result = remove_owned(
        unsafe { std::slice::from_raw_parts(entries, count as usize) }
            .iter()
            .copied(),
        scope,
        |entry| {
            let entry = unsafe { &**entry };
            let target = unsafe { string(entry.TargetName) };
            let service = if entry.Type == CRED_TYPE_GENERIC
                && target.ends_with(&format!(".{}", super::SERVICE))
            {
                super::SERVICE
            } else {
                ""
            };
            let account = target
                .strip_suffix(&format!(".{}", super::SERVICE))
                .unwrap_or("");
            Ok((service.into(), account.into()))
        },
        |entry| {
            format!("credential-manager:{}", unsafe {
                string((**entry).TargetName)
            })
        },
        |entry| {
            let succeeded = unsafe { CredDeleteW((**entry).TargetName, CRED_TYPE_GENERIC, 0) } != 0;
            let last_error = if succeeded {
                0
            } else {
                unsafe { GetLastError() }
            };
            credential_delete_outcome(succeeded, last_error)
        },
    );
    unsafe { CredFree(entries.cast()) };
    Ok(result)
}

#[cfg(target_os = "macos")]
pub(crate) fn remove(scope: &Scope) -> Result<Removal, String> {
    use core_foundation::{
        array::CFArray,
        base::{CFType, CFTypeRef, TCFType},
        boolean::CFBoolean,
        dictionary::CFDictionary,
        string::CFString,
    };
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};
    use security_framework_sys::item::*;
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        static kSecUseDataProtectionKeychain: CFTypeRef;
        fn SecItemDelete(query: CFTypeRef) -> i32;
        fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> i32;
    }
    unsafe fn k(raw: CFTypeRef) -> CFType {
        unsafe { CFType::wrap_under_get_rule(raw) }
    }
    fn account(attributes: &CFDictionary) -> Option<String> {
        let value = attributes.find(unsafe { kSecAttrAccount.cast::<std::ffi::c_void>() })?;
        Some(unsafe { CFString::wrap_under_get_rule((*value).cast()) }.to_string())
    }
    /// Legacy (file keychain) items that match `account`, as fresh references.
    /// An entitled app's legacy search can also return data-protection items,
    /// so this is what decides whether a legacy view still has an item behind it.
    fn legacy_refs(wanted: Option<&str>) -> Result<Vec<(CFType, String)>, String> {
        let mut search = ItemSearchOptions::new();
        search
            .class(ItemClass::generic_password())
            .service(super::SERVICE)
            .limit(Limit::All)
            .load_refs(true)
            .load_attributes(true)
            .load_data(false);
        if let Some(wanted) = wanted {
            search.account(wanted);
        }
        match search.search() {
            Ok(entries) => {
                let mut found = Vec::new();
                for entry in entries {
                    let SearchResult::Dict(attributes) = entry else {
                        return Err("Keychain did not return item attributes".into());
                    };
                    let Some(name) = account(&attributes) else {
                        return Err("Missing account attribute".into());
                    };
                    let Some(reference) =
                        attributes.find(unsafe { kSecValueRef.cast::<std::ffi::c_void>() })
                    else {
                        return Err("Keychain did not return an item reference".into());
                    };
                    found.push((unsafe { k((*reference).cast()) }, name));
                }
                Ok(found)
            }
            Err(error) if error.code() == -25300 => Ok(Vec::new()),
            Err(error) => Err(error.to_string()),
        }
    }
    fn delete_ref(reference: &CFType) -> i32 {
        unsafe {
            security_framework_sys::keychain_item::SecKeychainItemDelete(
                reference.as_CFTypeRef().cast_mut().cast(),
            )
        }
    }

    let mut report = Removal::default();
    let mut legacy = Vec::new();
    // Legacy ACL-bearing entries require reference deletion. No secret data is read.
    let entries = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(super::SERVICE)
        .limit(Limit::All)
        .load_refs(true)
        .load_attributes(true)
        .load_data(false)
        .search();
    match entries {
        Ok(entries) => {
            for entry in entries {
                let SearchResult::Dict(attributes) = entry else {
                    report.leftovers.push(Leftover {
                        path: "keychain:conduit-mcp/legacy (unreadable item)".into(),
                        error: "Keychain did not return item attributes; owner unknown, so it was not removed".into(),
                    });
                    continue;
                };
                let Some(account) = account(&attributes) else {
                    report.leftovers.push(Leftover {
                        path: "keychain:conduit-mcp/legacy".into(),
                        error: "Missing account attribute; owner unknown, so it was not removed"
                            .into(),
                    });
                    continue;
                };
                let path = format!("keychain:conduit-mcp/legacy/{account}");
                if !scope.owns(&account) {
                    report.preserved.push(account);
                    continue;
                }
                let reference = attributes.find(unsafe { kSecValueRef.cast::<std::ffi::c_void>() });
                let Some(reference) = reference else {
                    report.leftovers.push(Leftover {
                        path,
                        error: "Keychain did not return an item reference".into(),
                    });
                    continue;
                };
                legacy.push((unsafe { k((*reference).cast()) }, account, path));
            }
        }
        Err(error) if error.code() == -25300 => {}
        Err(error) => report.leftovers.push(Leftover {
            path: "keychain:conduit-mcp/legacy (inventory unavailable)".into(),
            error: error.to_string(),
        }),
    }
    unsafe {
        let base = vec![
            (k(kSecClass.cast()), k(kSecClassGenericPassword.cast())),
            (
                k(kSecAttrService.cast()),
                CFString::new(super::SERVICE).as_CFType(),
            ),
            (
                k(kSecAttrAccessGroup.cast()),
                CFString::new(super::platform::SHARED_ACCESS_GROUP).as_CFType(),
            ),
            (
                k(kSecUseDataProtectionKeychain.cast()),
                CFBoolean::true_value().as_CFType(),
            ),
        ];
        let with_account = |account: &str| {
            let mut query = base.clone();
            query.push((
                k(kSecAttrAccount.cast()),
                CFString::new(account).as_CFType(),
            ));
            query
        };
        let mut search = base.clone();
        search.extend([
            (
                k(kSecReturnAttributes.cast()),
                CFBoolean::true_value().as_CFType(),
            ),
            (k(kSecMatchLimit.cast()), k(kSecMatchLimitAll.cast())),
        ]);
        let query = CFDictionary::from_CFType_pairs(&search);
        let mut result = std::ptr::null();
        let status = SecItemCopyMatching(query.as_concrete_TypeRef().cast(), &mut result);
        if status == 0 && !result.is_null() {
            let entries = CFArray::<CFDictionary>::wrap_under_create_rule(result.cast());
            for attributes in entries.iter() {
                let Some(account) = account(&attributes) else {
                    report.leftovers.push(Leftover {
                        path: "keychain:conduit-mcp/data-protection".into(),
                        error: "Missing account attribute; owner unknown, so it was not removed"
                            .into(),
                    });
                    continue;
                };
                let path = format!("keychain:conduit-mcp/data-protection/{account}");
                if !scope.owns(&account) {
                    report.preserved.push(account);
                    continue;
                }
                let query = CFDictionary::from_CFType_pairs(&with_account(&account));
                let status = SecItemDelete(query.as_concrete_TypeRef().cast());
                let outcome = keychain_delete_outcome("SecItemDelete", status, || {
                    let mut lookup = with_account(&account);
                    lookup.push((
                        k(kSecReturnAttributes.cast()),
                        CFBoolean::true_value().as_CFType(),
                    ));
                    let lookup = CFDictionary::from_CFType_pairs(&lookup);
                    let mut found = std::ptr::null();
                    match SecItemCopyMatching(lookup.as_concrete_TypeRef().cast(), &mut found) {
                        0 => {
                            if !found.is_null() {
                                drop(CFType::wrap_under_create_rule(found));
                            }
                            Ok(true)
                        }
                        -25300 => Ok(false),
                        status => Err(format!("SecItemCopyMatching: {status}")),
                    }
                });
                record_outcome(&account, path, outcome, &mut report);
            }
        } else if status != -25300 {
            report.leftovers.push(Leftover {
                path: "keychain:conduit-mcp/data-protection (inventory unavailable)".into(),
                error: format!("SecItemCopyMatching: {status}"),
            });
        }
    }
    // The legacy pass runs even when a data-protection item failed: each item is
    // independent. A legacy reference can be a second view of a data-protection
    // item deleted above (the v2.3.5 acceptance run saw errSecInvalidItemRef for
    // exactly that), so a not-found status is settled by a fresh legacy lookup.
    for (reference, account, path) in legacy {
        let status = delete_ref(&reference);
        let outcome = if status == 0 {
            Ok(Outcome::Deleted)
        } else if !keychain_status_says_missing(status) {
            Err(format!("SecKeychainItemDelete: {status}"))
        } else {
            legacy_recheck(status, || legacy_refs(Some(&account)), delete_ref)
        };
        record_outcome(&account, path, outcome, &mut report);
    }
    Ok(report)
}

#[cfg(test)]
pub(crate) fn remove_fixture_accounts(scope: &Scope, accounts: &[&str]) -> Removal {
    remove_owned(
        accounts.iter().copied(),
        scope,
        |account| Ok((super::SERVICE.into(), (*account).into())),
        |account| (*account).into(),
        |_| Ok(Outcome::Deleted),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fake_keyring_keeps_other_apps_and_reports_each_failed_item() {
        let items = [
            ("other-app", "/vault/native"),
            ("conduit-mcp", "/vault/server"),
            ("conduit-mcp", "/vault/orphan-chunk"),
        ];
        let mut deleted = Vec::new();
        let leftovers = remove_owned(
            items,
            &Scope {
                all_installs: true,
                server_ids: Default::default(),
            },
            |item| Ok((item.0.into(), item.1.into())),
            |item| item.1.into(),
            |item| {
                deleted.push(item.1);
                if item.1.ends_with("chunk") {
                    Err("locked".into())
                } else {
                    Ok(Outcome::Deleted)
                }
            },
        );
        assert_eq!(deleted, ["/vault/server", "/vault/orphan-chunk"]);
        assert_eq!(leftovers.leftovers[0].path, "/vault/orphan-chunk");
        assert_eq!(leftovers.leftovers[0].error, "locked");
    }
    #[test]
    fn purge_scoped_keyring_preserves_other_install_master_and_chunks() {
        let items = [
            "mine::TOKEN",
            "mine::TOKEN::toolport-chunk::generation::0",
            "main::TOKEN",
            "__conduit_master_key__",
            "__conduit_team__::member_token",
        ];
        let scope = Scope {
            all_installs: false,
            server_ids: std::collections::HashSet::from(["mine".into()]),
        };
        let report = remove_owned(
            items,
            &scope,
            |account| Ok(("conduit-mcp".into(), (*account).into())),
            |account| (*account).into(),
            |_| Ok(Outcome::Deleted),
        );
        assert_eq!(report.removed, items[..2]);
        assert_eq!(report.preserved, items[2..]);
        assert!(report.leftovers.is_empty());
    }

    #[test]
    fn purge_keyring_unreadable_item_is_kept_but_never_blocks_the_readable_ones() {
        let scope = Scope {
            all_installs: true,
            server_ids: Default::default(),
        };
        let mut deleted = Vec::new();
        let report = remove_owned(
            ["readable", "unreadable", "later"],
            &scope,
            |account| {
                if *account == "unreadable" {
                    Err("denied".into())
                } else {
                    Ok(("conduit-mcp".into(), (*account).into()))
                }
            },
            |account| (*account).into(),
            |account| {
                assert_ne!(
                    *account, "unreadable",
                    "an item with no known owner is never deleted"
                );
                deleted.push(*account);
                Ok(Outcome::Deleted)
            },
        );
        assert_eq!(deleted, ["readable", "later"]);
        assert_eq!(report.removed, ["readable", "later"]);
        assert_eq!(report.leftovers.len(), 1);
        assert_eq!(report.leftovers[0].path, "unreadable");
        assert!(report.leftovers[0].error.starts_with("denied"));
    }

    #[test]
    fn purge_keyring_absent_items_are_reported_apart_from_removed_and_leftovers() {
        let scope = Scope {
            all_installs: true,
            server_ids: Default::default(),
        };
        let report = remove_owned(
            ["gone", "deleted", "locked", "after"],
            &scope,
            |account| Ok(("conduit-mcp".into(), (*account).into())),
            |account| (*account).into(),
            |account| match *account {
                "gone" => Ok(Outcome::Absent),
                "locked" => Err("locked".into()),
                _ => Ok(Outcome::Deleted),
            },
        );
        assert_eq!(report.removed, ["deleted", "after"]);
        assert_eq!(report.absent, ["gone"]);
        assert_eq!(report.leftovers.len(), 1);
        assert_eq!(report.leftovers[0].path, "locked");
    }

    #[test]
    fn purge_failed_delete_is_absent_only_when_a_fresh_lookup_agrees() {
        assert_eq!(
            confirm_after_failed_delete("NoSuchObject".into(), || Ok(false)),
            Ok(Outcome::Absent)
        );
        assert_eq!(
            confirm_after_failed_delete("locked".into(), || Ok(true)),
            Err("locked".into())
        );
        let error =
            confirm_after_failed_delete("locked".into(), || Err("bus gone".into())).unwrap_err();
        assert!(
            error.contains("locked") && error.contains("bus gone"),
            "{error}"
        );
    }

    #[test]
    fn purge_keychain_status_classification() {
        // Deleted.
        assert_eq!(
            keychain_delete_outcome("SecItemDelete", 0, || panic!("no lookup on success")),
            Ok(Outcome::Deleted)
        );
        // Every not-found status is confirmed, then reported absent.
        for status in KEYCHAIN_NOT_FOUND {
            assert_eq!(
                keychain_delete_outcome("SecItemDelete", status, || Ok(false)),
                Ok(Outcome::Absent),
                "{status}"
            );
            assert_eq!(
                keychain_delete_outcome("SecItemDelete", status, || Ok(true)),
                Err(format!("SecItemDelete: {status}"))
            );
        }
        // A refusal (errSecAuthFailed, errSecInteractionNotAllowed) is a real leftover.
        for status in [-25293, -25308] {
            assert_eq!(
                keychain_delete_outcome("SecItemDelete", status, || panic!("no lookup")),
                Err(format!("SecItemDelete: {status}"))
            );
        }
    }

    #[test]
    fn purge_legacy_recheck_settles_a_stale_reference_only_on_evidence() {
        type Listing = Result<Vec<(i32, String)>, String>;
        let listed = || -> Listing { Ok(vec![(1, "mock::TOKEN".into())]) };
        let empty = || -> Listing { Ok(Vec::new()) };
        let locked = || -> Listing { Err("locked".into()) };
        // (case, successive lookups, status of each fresh delete, expected)
        type Case = (
            &'static str,
            Vec<Listing>,
            i32,
            Result<Outcome, &'static str>,
        );
        let cases: Vec<Case> = vec![
            // v2.3.5: the legacy search returned the data-protection item, which
            // the first pass deleted; its stale reference then failed with -25304.
            (
                "stale view, nothing listed",
                vec![empty()],
                0,
                Ok(Outcome::Absent),
            ),
            // A real legacy item behind the stale view is deleted through a fresh ref.
            (
                "real item behind the view",
                vec![listed()],
                0,
                Ok(Outcome::Deleted),
            ),
            // One that vanishes during the retry, and is no longer listed, is absent.
            (
                "vanished during retry",
                vec![listed(), empty()],
                -25300,
                Ok(Outcome::Absent),
            ),
            // Listed, the retry says -25304 again, and it is still listed: a leftover.
            (
                "still listed after retry",
                vec![listed(), listed()],
                -25304,
                Err("still in the keychain"),
            ),
            (
                "still listed after -25300",
                vec![listed(), listed()],
                -25300,
                Err("still in the keychain"),
            ),
            // The second look itself fails: unknown, not absent.
            (
                "second look fails",
                vec![listed(), locked()],
                -25304,
                Err("could not confirm"),
            ),
            ("refused", vec![listed()], -25293, Err("-25293")),
            (
                "first look fails",
                vec![locked()],
                0,
                Err("could not confirm"),
            ),
        ];
        for (case, lookups, retry, expected) in cases {
            let mut lookups = lookups.into_iter();
            let outcome = legacy_recheck(
                -25304,
                || {
                    lookups
                        .next()
                        .unwrap_or_else(|| panic!("{case}: looked too often"))
                },
                |_| retry,
            );
            match (outcome, expected) {
                (Ok(got), Ok(want)) => assert_eq!(got, want, "{case}"),
                (Err(error), Err(want)) => assert!(error.contains(want), "{case}: {error}"),
                (got, want) => panic!("{case}: got {got:?}, expected {want:?}"),
            }
        }
    }

    #[test]
    fn purge_credential_manager_not_found_is_absent() {
        assert_eq!(credential_delete_outcome(true, 0), Ok(Outcome::Deleted));
        assert_eq!(credential_delete_outcome(false, 1168), Ok(Outcome::Absent));
        assert_eq!(
            credential_delete_outcome(false, 5),
            Err("CredDeleteW: 5".into())
        );
    }

    /// The numeric codes match the platform's own definitions, checked on the
    /// macOS CI runner against Security.framework's messages.
    #[cfg(target_os = "macos")]
    #[test]
    fn purge_macos_not_found_codes_are_the_security_framework_ones() {
        assert_eq!(
            KEYCHAIN_NOT_FOUND[0],
            security_framework_sys::base::errSecItemNotFound
        );
        for status in KEYCHAIN_NOT_FOUND {
            let message = security_framework::base::Error::from_code(status)
                .message()
                .unwrap_or_default()
                .to_lowercase();
            assert!(
                message.contains("could not be found")
                    || message.contains("no longer valid")
                    || message.contains("does not exist"),
                "{status}: {message}"
            );
        }
    }

    /// Deleting a credential that does not exist really reports ERROR_NOT_FOUND,
    /// which removal treats as already removed.
    #[cfg(target_os = "windows")]
    #[test]
    fn purge_windows_missing_credential_is_absent() {
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
        use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
        assert_eq!(ERROR_NOT_FOUND, 1168);
        let target: Vec<u16> = format!(
            "toolport-purge-test-{}-{}.conduit-mcp-missing",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
        .encode_utf16()
        .chain([0])
        .collect();
        let succeeded = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0;
        let error = unsafe { GetLastError() };
        assert_eq!(
            credential_delete_outcome(succeeded, error),
            Ok(Outcome::Absent)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn purge_missing_secret_service_is_distinct_from_locked_credentials() {
        assert!(unavailable(&dbus_secret_service::Error::Unavailable));
        assert!(!unavailable(&dbus_secret_service::Error::Locked));
        assert!(!unavailable(&dbus_secret_service::Error::Prompt));
    }

    #[test]
    fn purge_explicit_global_scope_removes_master_internal_and_deleted_server_accounts() {
        let items = [
            "__conduit_master_key__",
            "__toolport_internal__::task_key",
            "deleted-server::TOKEN",
            "mine::TOKEN::toolport-chunk::generation::0",
        ];
        let scope = Scope {
            all_installs: true,
            server_ids: Default::default(),
        };
        let report = remove_owned(
            items,
            &scope,
            |account| Ok(("conduit-mcp".into(), (*account).into())),
            |account| (*account).into(),
            |_| Ok(Outcome::Deleted),
        );
        assert_eq!(report.removed, items);
        assert!(report.preserved.is_empty() && report.leftovers.is_empty());
        assert!(!report.team_token_removed);
    }

    #[test]
    fn purge_team_token_note_tracks_successful_deletion_only() {
        let team = "__conduit_team__::member_token";
        let scope = Scope {
            all_installs: true,
            server_ids: Default::default(),
        };
        for delete_team in [false, true] {
            let report = remove_owned(
                ["mine::TOKEN", team],
                &scope,
                |account| Ok(("conduit-mcp".into(), (*account).into())),
                |account| (*account).into(),
                |account| {
                    if *account == team && !delete_team {
                        Err("locked".into())
                    } else {
                        Ok(Outcome::Deleted)
                    }
                },
            );
            assert_eq!(report.team_token_removed, delete_team);
        }
        let report = remove_owned(
            [team],
            &Scope {
                all_installs: false,
                server_ids: Default::default(),
            },
            |account| Ok(("conduit-mcp".into(), (*account).into())),
            |account| (*account).into(),
            |_| panic!("shared token must stay"),
        );
        assert!(!report.team_token_removed);
    }

    #[test]
    fn purge_default_scope_preserves_shared_and_unknown_accounts_even_without_detected_installs() {
        let items = [
            "mine::TOKEN",
            "mine::TOKEN::toolport-chunk::generation::0",
            "work::TOKEN",
            "__conduit_master_key__",
            "__toolport_internal__::task_key",
            "__conduit_team__::member_token",
            "__toolport_http_bridge__::TOKEN",
            "__toolport_http_clients__::fixture",
            "unknown",
        ];
        let scope = Scope {
            all_installs: false,
            server_ids: std::collections::HashSet::from([
                "mine".into(),
                "__toolport_internal__".into(),
                "__conduit_team__".into(),
                "__toolport_http_bridge__".into(),
                "__toolport_http_clients__".into(),
            ]),
        };
        let report = remove_owned(
            items,
            &scope,
            |account| Ok(("conduit-mcp".into(), (*account).into())),
            |account| (*account).into(),
            |_| Ok(Outcome::Deleted),
        );
        assert_eq!(report.removed, items[..2]);
        assert_eq!(report.preserved, items[2..]);
        assert!(!report.team_token_removed);
    }

    #[test]
    fn purge_r4_preserved_credentials_report_accounts_instead_of_object_paths() {
        let report = remove_owned(
            [
                ("/org/freedesktop/secrets/item/1", "__conduit_master_key__"),
                (
                    "/org/freedesktop/secrets/item/2",
                    "__toolport_http_bridge__::TOKEN",
                ),
            ],
            &Scope {
                all_installs: false,
                server_ids: Default::default(),
            },
            |item| Ok(("conduit-mcp".into(), item.1.into())),
            |item| item.0.into(),
            |_| panic!("shared keys must stay"),
        );
        assert_eq!(
            report.preserved,
            ["__conduit_master_key__", "__toolport_http_bridge__::TOKEN"]
        );
    }
}
