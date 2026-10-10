//! Removal is scoped to Toolport's reserved service, including orphaned entries.
use crate::purge::Leftover;

fn remove_owned<T>(
    entries: impl IntoIterator<Item = T>,
    mut service: impl FnMut(&T) -> Result<String, String>,
    mut path: impl FnMut(&T) -> String,
    mut delete: impl FnMut(&T) -> Result<(), String>,
) -> Vec<Leftover> {
    let mut leftovers = Vec::new();
    for entry in entries {
        let result = service(&entry).and_then(|service| {
            if service == super::SERVICE {
                delete(&entry)
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            leftovers.push(Leftover {
                path: path(&entry),
                error,
            });
        }
    }
    leftovers
}

#[cfg(target_os = "linux")]
pub(crate) fn remove() -> Result<Vec<Leftover>, String> {
    use dbus_secret_service::{EncryptionType, SecretService};
    let service = SecretService::connect_with_max_prompt_timeout(EncryptionType::Dh, 5)
        .map_err(|error| error.to_string())?;
    let entries = service
        .search_items(std::collections::HashMap::from([(
            "service",
            super::SERVICE,
        )]))
        .map_err(|error| error.to_string())?;
    Ok(remove_owned(
        entries.unlocked.into_iter().chain(entries.locked),
        |item| {
            item.get_attributes()
                .map_err(|error| error.to_string())
                .and_then(|attrs| {
                    attrs
                        .get("service")
                        .cloned()
                        .ok_or("Missing service attribute".into())
                })
        },
        |item| format!("secret-service:{}", item.path),
        |item| item.delete().map_err(|error| error.to_string()),
    ))
}

#[cfg(target_os = "windows")]
pub(crate) fn remove() -> Result<Vec<Leftover>, String> {
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
            Ok(Vec::new())
        } else {
            Err(format!("CredEnumerateW: {error}"))
        };
    }
    let result = remove_owned(
        unsafe { std::slice::from_raw_parts(entries, count as usize) },
        |entry| {
            let entry = unsafe { &**entry };
            let target = unsafe { string(entry.TargetName) };
            Ok(if entry.Type == CRED_TYPE_GENERIC
                && target.ends_with(&format!(".{}", super::SERVICE))
            {
                super::SERVICE
            } else {
                ""
            }
            .into())
        },
        |entry| {
            format!("credential-manager:{}", unsafe {
                string((**entry).TargetName)
            })
        },
        |entry| {
            if unsafe { CredDeleteW((**entry).TargetName, CRED_TYPE_GENERIC, 0) } != 0 {
                Ok(())
            } else {
                Err(format!("CredDeleteW: {}", unsafe { GetLastError() }))
            }
        },
    );
    unsafe { CredFree(entries.cast()) };
    Ok(result)
}

#[cfg(target_os = "macos")]
pub(crate) fn remove() -> Result<Vec<Leftover>, String> {
    use core_foundation::{
        base::{CFType, TCFType},
        boolean::CFBoolean,
        dictionary::CFDictionary,
        string::CFString,
    };
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, Reference, SearchResult};
    use security_framework_sys::item::*;
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecItemDelete(query: core_foundation::base::CFTypeRef) -> i32;
    }
    let mut leftovers = Vec::new();
    // The legacy ACL-bearing store requires deleting references, not a broad SecItemDelete.
    let entries = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(super::SERVICE)
        .limit(Limit::All)
        .load_refs(true)
        .load_data(false)
        .search();
    match entries {
        Ok(entries) => {
            for (index, entry) in entries.into_iter().enumerate() {
                if let SearchResult::Ref(Reference::KeychainItem(item)) = entry {
                    let status = unsafe {
                        security_framework_sys::keychain_item::SecKeychainItemDelete(
                            item.as_concrete_TypeRef(),
                        )
                    };
                    if status != 0 {
                        leftovers.push(Leftover {
                            path: format!("keychain:conduit-mcp/legacy/{index}"),
                            error: format!("SecKeychainItemDelete: {status}"),
                        });
                    }
                }
            }
        }
        Err(error) if error.code() == -25300 => {}
        Err(error) => leftovers.push(Leftover {
            path: "keychain:conduit-mcp/legacy".into(),
            error: error.to_string(),
        }),
    }
    unsafe {
        fn k(raw: core_foundation::base::CFTypeRef) -> CFType {
            unsafe { CFType::wrap_under_get_rule(raw) }
        }
        let query = CFDictionary::from_CFType_pairs(&[
            (k(kSecClass), k(kSecClassGenericPassword)),
            (
                k(kSecAttrService),
                CFString::new(super::SERVICE).as_CFType(),
            ),
            (
                k(kSecAttrAccessGroup),
                CFString::new(super::platform::SHARED_ACCESS_GROUP).as_CFType(),
            ),
            (
                k(kSecUseDataProtectionKeychain),
                CFBoolean::true_value().as_CFType(),
            ),
        ]);
        let status = SecItemDelete(query.as_concrete_TypeRef());
        if status != 0 && status != -25300 {
            leftovers.push(Leftover {
                path: format!(
                    "keychain:conduit-mcp/{}",
                    super::platform::SHARED_ACCESS_GROUP
                ),
                error: format!("SecItemDelete: {status}"),
            });
        }
    }
    Ok(leftovers)
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
            |item| Ok(item.0.into()),
            |item| item.1.into(),
            |item| {
                deleted.push(item.1);
                if item.1.ends_with("chunk") {
                    Err("locked".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(deleted, ["/vault/server", "/vault/orphan-chunk"]);
        assert_eq!(leftovers[0].path, "/vault/orphan-chunk");
        assert_eq!(leftovers[0].error, "locked");
    }
}
