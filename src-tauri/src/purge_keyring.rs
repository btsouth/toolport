//! Removal is scoped to Toolport's reserved service, including orphaned entries.
use crate::purge::Leftover;

#[cfg(any(target_os = "linux", target_os = "windows", test))]
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
    let mut leftovers = Vec::new();
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
                    leftovers.push(Leftover {
                        path: "keychain:conduit-mcp/legacy (unreadable item)".into(),
                        error: "Keychain did not return item attributes".into(),
                    });
                    continue;
                };
                let path = format!(
                    "keychain:conduit-mcp/legacy/{}",
                    account(&attributes).unwrap_or_else(|| "(unknown account)".into())
                );
                let reference = attributes.find(unsafe { kSecValueRef.cast::<std::ffi::c_void>() });
                let Some(reference) = reference else {
                    leftovers.push(Leftover {
                        path,
                        error: "Keychain did not return an item reference".into(),
                    });
                    continue;
                };
                let status = unsafe {
                    security_framework_sys::keychain_item::SecKeychainItemDelete(
                        (*reference).cast(),
                    )
                };
                if status != 0 {
                    leftovers.push(Leftover {
                        path,
                        error: format!("SecKeychainItemDelete: {status}"),
                    });
                }
            }
        }
        Err(error) if error.code() == -25300 => {}
        Err(error) => leftovers.push(Leftover {
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
                    leftovers.push(Leftover {
                        path: "keychain:conduit-mcp/data-protection (unknown account)".into(),
                        error: "Keychain did not return an account".into(),
                    });
                    continue;
                };
                let mut delete = base.clone();
                delete.push((
                    k(kSecAttrAccount.cast()),
                    CFString::new(&account).as_CFType(),
                ));
                let query = CFDictionary::from_CFType_pairs(&delete);
                let status = SecItemDelete(query.as_concrete_TypeRef().cast());
                if status != 0 && status != -25300 {
                    leftovers.push(Leftover {
                        path: format!("keychain:conduit-mcp/data-protection/{account}"),
                        error: format!("SecItemDelete: {status}"),
                    });
                }
            }
        } else if status != -25300 {
            leftovers.push(Leftover {
                path: "keychain:conduit-mcp/data-protection (inventory unavailable)".into(),
                error: format!("SecItemCopyMatching: {status}"),
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
