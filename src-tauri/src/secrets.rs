//! Secret storage in the OS keychain (Windows Credential Manager / macOS
//! Keychain / libsecret). Secret env values never live in Toolport's registry
//! file or any client config - only here. The gateway pulls them at spawn time
//! and injects them into the child server's environment.

const SERVICE: &str = "conduit-mcp";

#[path = "purge_keyring.rs"]
pub(crate) mod purge;

const INTERNAL_SERVER_ID: &str = "__toolport_internal__";
#[cfg(not(test))]
const TASK_HANDLE_KEY: &str = "__task_handle_key__";

/// Vault reference for a reviewed endpoint containing inline credentials.
pub(crate) const IMPORTED_URL_KEY: &str = "__imported_endpoint_url__";

/// Reserved secret key for an http server's bearer token (Tier A auth, and where
/// the OAuth flow stores its access token).
pub const HTTP_AUTH_KEY: &str = "__http_auth__";

/// Reserved secret key for the OAuth client-credentials client secret (SBS-524).
///
/// Separate from [`HTTP_AUTH_KEY`], which holds the short-lived *access* token.
/// This is the long-lived credential used to mint those, so it must never be
/// written to `registry.json`, config backups, or exports; only the non-secret
/// `clientId` / scopes / auth method live there.
pub const CLIENT_SECRET_KEY: &str = "__oauth_client_secret__";

#[cfg(any(target_os = "macos", test))]
fn keychain_gateway_path(
    result: Result<Option<std::path::PathBuf>, String>,
) -> Option<std::path::PathBuf> {
    match result {
        Ok(path) => path,
        Err(_) => {
            crate::gatewaylog::append("gateway publication failed; keychain ACL is app-only");
            None
        }
    }
}

fn account(server_id: &str, key: &str) -> String {
    format!("{server_id}::{key}")
}

// ── macOS ──────────────────────────────────────────────────────────────────
//
// On macOS we bypass the `keyring` crate and call `security_framework` /
// `SecItem*` directly.
//
// **Per-server secret storage uses the DATA-PROTECTION keychain** with a
// team-scoped access group (`V4YZPC7T6G.com.tsout.conduit.shared`). Items are
// created via raw `SecItemAdd` with `kSecUseDataProtectionKeychain=true`,
// `kSecAttrAccessGroup=<shared group>`, and
// `kSecAttrAccessible=kSecAttrAccessibleAfterFirstUnlock`. Both the Tauri UI app
// (`com.tsout.conduit`) and the standalone gateway binary
// (`com.tsout.conduit.gateway`) embed the SAME access group in their
// entitlements (`Entitlements.plist` / `Gateway.entitlements`), so BOTH read the
// item with no prompt and no per-application ACL. This replaces the older
// legacy-keychain `SecAccess`/trusted-application ACL approach.
//
// IMPORTANT: the data-protection keychain requires a code-signed binary with a
// valid Application Identifier (from an embedded provisioning profile / the
// `application-identifier` entitlement). On an UNSIGNED binary every DP-keychain
// call returns `errSecMissingEntitlement` (-34018). The DP-keychain round-trip
// tests are therefore `#[ignore]`d and only pass on a signed build (see Phase 7
// of the keychain-access-group rollout). Plain `cargo test` on an unsigned/dev
// build cannot exercise this path.
//
// `migrate_legacy_to_dpk()` runs once at app startup (guarded by a NEW marker
// file, `.keychain-dpk-migrated`) to move per-server secrets out of the legacy
// file-based keychain and into the data-protection store: read the old value,
// write it to the new store, verify the round-trip, and ONLY THEN delete the old
// legacy item. Any failure leaves the old item in place, so no secret is lost.
//
// The `keyring`-crate / `SecKeychainItem` legacy helpers below
// (`get_generic_password`, `delete_entry_by_account`, the master-key `mod file`
// path) are retained: the master key for the encrypted-file backend still lives
// in the legacy keychain, and migration reads the old legacy items through them.
#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::base::TCFType;
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, Reference, SearchResult};
    use security_framework::os::macos::keychain_item::SecKeychainItem;
    use security_framework::passwords::get_generic_password;
    use security_framework_sys::base::errSecItemNotFound;

    use super::{account, SERVICE};

    /// Team-scoped keychain access group shared by the Toolport app and the
    /// gateway. Both binaries declare this group in their entitlements
    /// (`keychain-access-groups`), which is what lets each read the other's
    /// data-protection-keychain items with no prompt. The `V4YZPC7T6G` prefix is
    /// the Apple Developer Team ID; it MUST match the entitlement files
    /// (`Entitlements.plist` / `Gateway.entitlements`) exactly.
    pub const SHARED_ACCESS_GROUP: &str = "V4YZPC7T6G.com.tsout.conduit.shared";

    /// Reserved keychain account for the single 32-byte master key that encrypts
    /// the file backend (`secrets.enc`). Distinct from every server-secret account
    /// (which is `server_id::key`), so it never collides with a real secret. This
    /// is the ONLY keychain item Toolport creates once the file backend is the
    /// default on macOS — per-server secrets then live only in `secrets.enc`.
    pub const MASTER_KEY_ACCOUNT: &str = "__conduit_master_key__";

    /// Read the 32-byte master key from the keychain, if present.
    ///
    /// Returns `Ok(None)` when no master-key item exists yet (`errSecItemNotFound`,
    /// -25300) — the signal that the file backend is not yet the default. Returns
    /// `Err` on a stored value that isn't exactly 32 bytes after base64-decode
    /// (corrupt item) or any other keychain failure. This is **read-only**: both
    /// the app and the gateway call it. Only the app ever creates the key (via
    /// `ensure_master_key`).
    pub fn read_master_key() -> Result<Option<[u8; 32]>, String> {
        use base64::Engine;
        match get_generic_password(SERVICE, MASTER_KEY_ACCOUNT) {
            Ok(bytes) => {
                let encoded = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(encoded.trim())
                    .map_err(|e| e.to_string())?;
                if raw.len() != 32 {
                    return Err(format!(
                        "master key in keychain is {} bytes, expected 32",
                        raw.len()
                    ));
                }
                let mut key = [0u8; 32];
                key.copy_from_slice(&raw);
                Ok(Some(key))
            }
            Err(e) if e.code() == -25300 /* errSecItemNotFound */ => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Return the master key, creating it if it doesn't exist yet.
    ///
    /// If a key is already stored, returns it. Otherwise generates 32 random bytes
    /// and stores them base64-encoded via the EXISTING `add_with_shared_access`
    /// path, so the shared-access ACL lets BOTH the app and the separately-signed
    /// gateway read the master key with no prompt. **Only the app's startup calls
    /// this** — the gateway is read-only and must never create the key (it calls
    /// `read_master_key` instead).
    pub fn ensure_master_key() -> Result<[u8; 32], String> {
        use base64::Engine;
        if let Some(key) = read_master_key()? {
            return Ok(key);
        }
        let mut key = [0u8; 32];
        getrandom::getrandom(&mut key).map_err(|e| e.to_string())?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(key);
        add_with_shared_access(MASTER_KEY_ACCOUNT, &encoded)?;
        Ok(key)
    }

    /// Store a per-server secret in the **data-protection keychain** under the
    /// shared access group, via raw `SecItemAdd`.
    ///
    /// Builds a dictionary with `kSecClass=kSecClassGenericPassword`,
    /// `kSecAttrService`, `kSecAttrAccount`, `kSecValueData`,
    /// `kSecUseDataProtectionKeychain=true`, `kSecAttrAccessGroup=<shared group>`,
    /// and `kSecAttrAccessible=kSecAttrAccessibleAfterFirstUnlock`. Existing
    /// items are replaced with `SecItemUpdate`; `SecItemAdd` is used only when
    /// the account does not exist, so a failed update cannot destroy the old value.
    ///
    /// The high-level `security_framework::passwords` API targets the *legacy*
    /// keychain and cannot set the access group or the data-protection flag, so
    /// this MUST be raw FFI.
    ///
    /// Returns `Err` on an unsigned/dev build: the data-protection keychain
    /// rejects every call with `errSecMissingEntitlement` (-34018) when the binary
    /// has no Application Identifier (no embedded provisioning profile).
    pub fn set_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
        dpk::set(&account(server_id, key), value)
    }

    /// Create a generic-password item in the **legacy** keychain that BOTH the
    /// Toolport app and the `toolport-gateway` binary can read with no keychain
    /// prompt: build a legacy `SecAccess` whose trusted-applications list names
    /// both binaries and pass it as `kSecAttrAccess` on `SecItemAdd`. Setting the
    /// ACL atomically at creation avoids the keychain-password prompt that a
    /// post-hoc `SecKeychainItemSetAccess` would raise.
    ///
    /// **Scope after the data-protection rewrite:** per-server secrets now use the
    /// data-protection keychain + the team-scoped access group (`dpk::set`). This
    /// legacy trusted-application path is retained ONLY for the encrypted-file
    /// backend's master key (`ensure_master_key`), which the gateway must read as
    /// a *bare CLI binary* that cannot carry the provisioning profile required for
    /// an access-group entitlement (a profile-less binary with a restricted
    /// `keychain-access-groups` entitlement is SIGKILLed by AMFI at launch,
    /// -34018 / amfid -413). The legacy ACL works for Developer ID distribution
    /// with no profile; the APIs are deprecated-but-functional.
    fn add_with_shared_access(account_str: &str, value: &str) -> Result<(), String> {
        use core_foundation::array::CFArray;
        use core_foundation::base::{CFType, CFTypeRef, TCFType};
        use core_foundation::data::CFData;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::string::CFString;
        use std::ffi::{c_void, CString};
        use std::os::raw::c_char;

        #[link(name = "Security", kind = "framework")]
        extern "C" {
            fn SecTrustedApplicationCreateFromPath(
                path: *const c_char,
                app: *mut *mut c_void,
            ) -> i32;
            fn SecAccessCreate(
                descriptor: *const c_void,
                trustedlist: *const c_void,
                access_ref: *mut *mut c_void,
            ) -> i32;
            fn SecItemAdd(attributes: *const c_void, result: *mut *const c_void) -> i32;
            fn SecItemUpdate(query: *const c_void, attributes: *const c_void) -> i32;
            static kSecClass: CFTypeRef;
            static kSecClassGenericPassword: CFTypeRef;
            static kSecAttrService: CFTypeRef;
            static kSecAttrAccount: CFTypeRef;
            static kSecValueData: CFTypeRef;
            static kSecAttrAccess: CFTypeRef;
        }

        // 1. Build a SecAccess trusting the two binaries (this app + the gateway).
        let trusted_app = |p: &std::path::Path| -> Result<CFType, String> {
            let c = CString::new(p.to_string_lossy().into_owned()).map_err(|e| e.to_string())?;
            let mut app: *mut c_void = std::ptr::null_mut();
            let st = unsafe { SecTrustedApplicationCreateFromPath(c.as_ptr(), &mut app) };
            if st != 0 || app.is_null() {
                return Err(format!(
                    "SecTrustedApplicationCreateFromPath({}) failed: {st}",
                    p.display()
                ));
            }
            Ok(unsafe { CFType::wrap_under_create_rule(app as CFTypeRef) })
        };
        // The app (this process) must be trustable; if it isn't there's nothing
        // usable to write. The gateway is best-effort: add it when its path resolves
        // and a trusted-application can be built for it, otherwise proceed app-only
        // (the item stays ACL-protected, NOT world-readable; the gateway just falls
        // back to a one-time "Always Allow" until the next rewrite names it). This
        // also keeps unit tests working, where the gateway binary isn't on disk.
        let app_path = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut trusted_apps: Vec<CFType> = Vec::with_capacity(2);
        trusted_apps.push(trusted_app(&app_path)?);
        match super::keychain_gateway_path(crate::clients::resolve_gateway_path()) {
            Some(gw_path) => match trusted_app(&gw_path) {
                Ok(t) => trusted_apps.push(t),
                Err(e) => eprintln!("toolport: gateway not added to keychain ACL ({e}); app-only"),
            },
            None => eprintln!("toolport: gateway path unresolved; keychain ACL is app-only"),
        }
        let trusted = CFArray::from_CFTypes(&trusted_apps);
        let label = CFString::new("conduit-mcp");
        let mut access: *mut c_void = std::ptr::null_mut();
        let st = unsafe {
            SecAccessCreate(
                label.as_concrete_TypeRef() as *const c_void,
                trusted.as_concrete_TypeRef() as *const c_void,
                &mut access,
            )
        };
        if st != 0 || access.is_null() {
            return Err(format!("SecAccessCreate failed: {st}"));
        }
        let access_cf = unsafe { CFType::wrap_under_create_rule(access as CFTypeRef) };

        // The kSec* keys are CFString constants; pull them into safe CFType values.
        let (k_class, k_generic, k_service, k_account, k_value, k_access) = unsafe {
            (
                CFType::wrap_under_get_rule(kSecClass),
                CFType::wrap_under_get_rule(kSecClassGenericPassword),
                CFType::wrap_under_get_rule(kSecAttrService),
                CFType::wrap_under_get_rule(kSecAttrAccount),
                CFType::wrap_under_get_rule(kSecValueData),
                CFType::wrap_under_get_rule(kSecAttrAccess),
            )
        };
        let service_cf = CFString::new(SERVICE).as_CFType();
        let account_cf = CFString::new(account_str).as_CFType();

        // 2. Update in place when the master key already exists. A failed update
        // leaves the previous value intact; delete-then-add could destroy the only
        // key capable of decrypting the file backend.
        let query = CFDictionary::from_CFType_pairs(&[
            (k_class.clone(), k_generic.clone()),
            (k_service.clone(), service_cf.clone()),
            (k_account.clone(), account_cf.clone()),
        ]);
        let data_cf = CFData::from_buffer(value.as_bytes()).as_CFType();
        let update = CFDictionary::from_CFType_pairs(&[
            (k_value.clone(), data_cf.clone()),
            (k_access.clone(), access_cf.clone()),
        ]);
        let update_status = unsafe {
            SecItemUpdate(
                query.as_concrete_TypeRef() as *const c_void,
                update.as_concrete_TypeRef() as *const c_void,
            )
        };
        if update_status == 0 {
            return Ok(());
        }
        if update_status != -25300 {
            return Err(format!(
                "SecItemUpdate with shared access failed: {update_status}"
            ));
        }

        // 3. No prior item: add it WITH the shared-access ACL atomically.
        let add = CFDictionary::from_CFType_pairs(&[
            (k_class, k_generic),
            (k_service, service_cf),
            (k_account, account_cf),
            (k_value, data_cf),
            (k_access, access_cf),
        ]);
        let st = unsafe {
            SecItemAdd(
                add.as_concrete_TypeRef() as *const c_void,
                std::ptr::null_mut(),
            )
        };
        if st != 0 {
            return Err(format!("SecItemAdd with shared access failed: {st}"));
        }
        Ok(())
    }

    /// Test-only: seed an item in the **legacy** keychain (the one the
    /// legacy -> data-protection migration reads FROM) so a signed-build test can
    /// exercise `migrate_legacy_to_dpk`. Writes via `add_with_shared_access`,
    /// which targets the legacy keychain under the same SERVICE/account the
    /// migration scans with `get_generic_password`.
    #[cfg(test)]
    pub fn seed_legacy_for_test(server_id: &str, key: &str, value: &str) -> Result<(), String> {
        add_with_shared_access(&account(server_id, key), value)
    }

    pub fn get_secret_result(server_id: &str, key: &str) -> Result<Option<String>, String> {
        #[cfg(test)]
        crate::secrets::tests::record_vault_read(server_id, key);
        read_secret(server_id, key).map_err(super::ReadFailure::into_message)
    }

    /// [`get_secret_result`] with the kind of failure: an item whose data is
    /// not text is damaged; every keychain status is the store's.
    pub fn read_secret(server_id: &str, key: &str) -> Result<Option<String>, super::ReadFailure> {
        dpk::get_kinded(&account(server_id, key))
    }

    pub fn delete_secret(server_id: &str, key: &str) -> Result<(), String> {
        dpk::delete(&account(server_id, key))
    }

    pub fn get_backup_secret_status(
        server_id: &str,
        key: &str,
    ) -> Result<Option<String>, super::BackupSecretError> {
        dpk::get_backup(&account(server_id, key))
    }
    pub fn delete_backup_secret_status(
        server_id: &str,
        key: &str,
    ) -> Result<(), super::BackupSecretError> {
        dpk::delete_backup(&account(server_id, key))
    }

    /// Raw `SecItem*` FFI bound to the **data-protection keychain** with the
    /// shared access group. All three operations carry
    /// `kSecUseDataProtectionKeychain=true` and `kSecAttrAccessGroup=<shared
    /// group>` so the app and gateway see the same items.
    ///
    /// `kSecAttrAccessible` / `kSecAttrAccessibleAfterFirstUnlock` /
    /// `kSecMatchLimitOne` are NOT re-exported by `security_framework_sys` (and
    /// `kSecUseDataProtectionKeychain` is feature-gated behind `OSX_10_15` there),
    /// so this block declares every `kSec*` constant it needs itself via
    /// `#[link(name = "Security")]`. `kCFBooleanTrue` for the data-protection flag
    /// comes from `core_foundation::boolean::CFBoolean::true_value()`.
    mod dpk {
        use core_foundation::base::{CFType, CFTypeRef, TCFType};
        use core_foundation::boolean::CFBoolean;
        use core_foundation::data::CFData;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::string::CFString;
        use std::os::raw::c_void;

        use super::{SERVICE, SHARED_ACCESS_GROUP};

        const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

        #[link(name = "Security", kind = "framework")]
        extern "C" {
            fn SecItemAdd(attributes: *const c_void, result: *mut *const c_void) -> i32;
            fn SecItemCopyMatching(query: *const c_void, result: *mut *const c_void) -> i32;
            fn SecItemDelete(query: *const c_void) -> i32;
            fn SecItemUpdate(query: *const c_void, attributes: *const c_void) -> i32;

            static kSecClass: CFTypeRef;
            static kSecClassGenericPassword: CFTypeRef;
            static kSecAttrService: CFTypeRef;
            static kSecAttrAccount: CFTypeRef;
            static kSecValueData: CFTypeRef;
            static kSecAttrAccessGroup: CFTypeRef;
            static kSecAttrAccessible: CFTypeRef;
            static kSecAttrAccessibleAfterFirstUnlock: CFTypeRef;
            static kSecUseDataProtectionKeychain: CFTypeRef;
            static kSecUseAuthenticationUI: CFTypeRef;
            static kSecUseAuthenticationUIFail: CFTypeRef;
            static kSecReturnData: CFTypeRef;
            static kSecMatchLimit: CFTypeRef;
            static kSecMatchLimitOne: CFTypeRef;
        }

        /// The kSec* constants are CFString/CFType singletons; wrap them under the
        /// get rule (no ownership transfer) into safe `CFType`s.
        fn k(raw: CFTypeRef) -> CFType {
            unsafe { CFType::wrap_under_get_rule(raw) }
        }

        /// The shared base query keys present in EVERY operation: class,
        /// service, account, the access group, and the data-protection flag.
        fn base_query(account_str: &str) -> Vec<(CFType, CFType)> {
            unsafe {
                vec![
                    (k(kSecClass), k(kSecClassGenericPassword)),
                    (k(kSecAttrService), CFString::new(SERVICE).as_CFType()),
                    (k(kSecAttrAccount), CFString::new(account_str).as_CFType()),
                    (
                        k(kSecAttrAccessGroup),
                        CFString::new(SHARED_ACCESS_GROUP).as_CFType(),
                    ),
                    (
                        k(kSecUseDataProtectionKeychain),
                        CFBoolean::true_value().as_CFType(),
                    ),
                ]
            }
        }

        /// Update an existing item in place, or add it when absent. A failed
        /// replacement never deletes the previous value.
        pub fn set(account_str: &str, value: &str) -> Result<(), String> {
            let query = CFDictionary::from_CFType_pairs(&base_query(account_str));
            let value_data = CFData::from_buffer(value.as_bytes()).as_CFType();
            let update = CFDictionary::from_CFType_pairs(&[
                (k(unsafe { kSecValueData }), value_data.clone()),
                (
                    k(unsafe { kSecAttrAccessible }),
                    k(unsafe { kSecAttrAccessibleAfterFirstUnlock }),
                ),
            ]);
            let update_status = unsafe {
                SecItemUpdate(
                    query.as_concrete_TypeRef() as *const c_void,
                    update.as_concrete_TypeRef() as *const c_void,
                )
            };
            if update_status == 0 {
                return Ok(());
            }
            if update_status != ERR_SEC_ITEM_NOT_FOUND {
                return Err(format!(
                    "SecItemUpdate (data-protection keychain) failed: {update_status}"
                ));
            }

            // No prior item: add the fresh value + accessibility class.
            let mut pairs = base_query(account_str);
            pairs.push((k(unsafe { kSecValueData }), value_data));
            pairs.push((
                k(unsafe { kSecAttrAccessible }),
                k(unsafe { kSecAttrAccessibleAfterFirstUnlock }),
            ));
            let add = CFDictionary::from_CFType_pairs(&pairs);
            let st = unsafe {
                SecItemAdd(
                    add.as_concrete_TypeRef() as *const c_void,
                    std::ptr::null_mut(),
                )
            };
            if st != 0 {
                return Err(format!(
                    "SecItemAdd (data-protection keychain) failed: {st}"
                ));
            }
            Ok(())
        }

        /// `SecItemCopyMatching` with `kSecReturnData=true` and
        /// `kSecMatchLimit=kSecMatchLimitOne`. `errSecItemNotFound` (-25300) maps
        /// to `Ok(None)`; the returned CFData is decoded to a `String`.
        pub fn get(account_str: &str) -> Result<Option<String>, String> {
            get_inner(account_str, false).map_err(|e| e.to_string())
        }
        pub fn get_kinded(
            account_str: &str,
        ) -> Result<Option<String>, crate::secrets::ReadFailure> {
            get_inner(account_str, false).map_err(|e| match e {
                crate::secrets::BackupSecretError::Corrupt => {
                    crate::secrets::ReadFailure::Damaged(e.to_string())
                }
                _ => crate::secrets::ReadFailure::Unavailable(e.to_string()),
            })
        }
        pub fn get_backup(
            account_str: &str,
        ) -> Result<Option<String>, crate::secrets::BackupSecretError> {
            get_inner(account_str, true)
        }
        fn get_inner(
            account_str: &str,
            noninteractive: bool,
        ) -> Result<Option<String>, crate::secrets::BackupSecretError> {
            let mut pairs = base_query(account_str);
            if noninteractive {
                pairs.push((
                    k(unsafe { kSecUseAuthenticationUI }),
                    k(unsafe { kSecUseAuthenticationUIFail }),
                ));
            }
            pairs.push((
                k(unsafe { kSecReturnData }),
                CFBoolean::true_value().as_CFType(),
            ));
            pairs.push((
                k(unsafe { kSecMatchLimit }),
                k(unsafe { kSecMatchLimitOne }),
            ));
            let query = CFDictionary::from_CFType_pairs(&pairs);

            let mut result: *const c_void = std::ptr::null_mut();
            let st = unsafe {
                SecItemCopyMatching(query.as_concrete_TypeRef() as *const c_void, &mut result)
            };
            if st == ERR_SEC_ITEM_NOT_FOUND {
                return Ok(None);
            }
            if st != 0 {
                return Err(crate::secrets::BackupSecretError::macos(st));
            }
            if result.is_null() {
                return Ok(None);
            }
            // SecItemCopyMatching returns a CFDataRef (we requested return-data);
            // it's a +1 (create-rule) reference we own. `as _` resolves to
            // `CFData::Ref` (the concrete `CFDataRef`) from the function arg type.
            let data = unsafe { CFData::wrap_under_create_rule(result as _) };
            String::from_utf8(data.bytes().to_vec())
                .map(Some)
                .map_err(|_| crate::secrets::BackupSecretError::Corrupt)
        }

        /// `SecItemDelete` against the data-protection keychain.
        /// `errSecItemNotFound` maps to `Ok(())`.
        pub fn delete(account_str: &str) -> Result<(), String> {
            delete_inner(account_str, false).map_err(|e| e.to_string())
        }
        pub fn delete_backup(account_str: &str) -> Result<(), crate::secrets::BackupSecretError> {
            delete_inner(account_str, true)
        }
        fn delete_inner(
            account_str: &str,
            noninteractive: bool,
        ) -> Result<(), crate::secrets::BackupSecretError> {
            let mut pairs = base_query(account_str);
            if noninteractive {
                pairs.push((
                    k(unsafe { kSecUseAuthenticationUI }),
                    k(unsafe { kSecUseAuthenticationUIFail }),
                ));
            }
            let query = CFDictionary::from_CFType_pairs(&pairs);
            let st = unsafe { SecItemDelete(query.as_concrete_TypeRef() as *const c_void) };
            if st == 0 || st == ERR_SEC_ITEM_NOT_FOUND {
                Ok(())
            } else {
                Err(crate::secrets::BackupSecretError::macos(st))
            }
        }
    }

    /// Migrate per-server secrets OUT of the **legacy file-based keychain** and
    /// INTO the **data-protection keychain** (shared access group).
    ///
    /// **Per-key flow (write -> verify -> only-then-delete, no data loss):**
    ///
    /// For each `(server_id, key)` pair:
    /// 1. **Read** the OLD value from the legacy keychain via the existing
    ///    `get_generic_password`. Safe from the app process, which has ACL grants
    ///    from the old version.
    /// 2. **Write** it to the NEW data-protection store (`set_secret` ->
    ///    `dpk::set`).
    /// 3. **Verify** by reading it back from the new store (`dpk::get`); the
    ///    round-tripped value must equal the original.
    /// 4. **Only then delete** the old legacy item via the existing
    ///    `delete_entry_by_account` (account-filtered ref search +
    ///    `SecKeychainItemDelete`, the only API that reliably removes legacy
    ///    file-based items).
    ///
    /// On ANY failure for a key (read/write/verify error, or a verify mismatch),
    /// the old legacy item is LEFT IN PLACE — no secret is lost — and the key is
    /// counted as `failed`. Keys that don't exist in the legacy keychain (e.g.
    /// `__oauth_state__` on a non-OAuth server) are counted as `not_found`, not a
    /// failure.
    ///
    /// **Signed-build-only:** step 2/3 hit the data-protection keychain, which
    /// returns `errSecMissingEntitlement` (-34018) on an unsigned binary, so this
    /// can only succeed on a signed build with the embedded provisioning profile.
    ///
    /// Wired into app startup via `migrate_secrets_to_dpk`, which guards it behind
    /// `DPK_MIGRATION_MARKER` so it runs once per install. The app process is the
    /// only caller (the gateway is read-only).
    pub fn migrate_legacy_to_dpk(
        keys: &[(String, String)],
    ) -> Result<super::MigrationReport, String> {
        let mut migrated = 0;
        let mut failed = 0;
        let mut not_found = 0;

        for (server_id, key) in keys {
            if server_id.as_str() == super::INTERNAL_SERVER_ID {
                failed += 1;
                continue;
            }
            let acct = account(server_id, key);
            match get_generic_password(SERVICE, &acct) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(value) => {
                        // write (new store) -> verify (new store) -> only then
                        // delete the OLD legacy item. Leave the legacy item in
                        // place on any failure so no secret is lost.
                        let moved = set_secret(server_id, key, &value).is_ok()
                            && matches!(
                                dpk::get(&acct),
                                Ok(Some(ref v)) if *v == value
                            );
                        if moved {
                            let _ = delete_entry_by_account(&acct);
                            migrated += 1;
                        } else {
                            failed += 1;
                        }
                    }
                    Err(_) => failed += 1, // non-UTF-8 value — can't round-trip
                },
                Err(e) if e.code() == -25300 => not_found += 1, // expected for reserved keys
                Err(_) => failed += 1,                          // locked keychain, denied access
            }
        }

        Ok(super::MigrationReport {
            migrated,
            failed,
            not_found,
        })
    }

    /// Migrate per-server secrets OUT of individual keychain items and INTO the
    /// encrypted file backend (`secrets.enc`), so they stop living as separate
    /// ACL'd keychain items that prompt on every app update.
    ///
    /// Per-key flow (no secret loss, with rollback):
    /// 1. **Read** the value from the OLD keychain item (`get_generic_password`).
    /// 2. **Write** it into the file backend (`super::file::set_secret`).
    /// 3. **Verify** by reading it back from the file backend; the round-tripped
    ///    value must equal the original.
    /// 4. **Only then delete** the old keychain item (`delete_entry_by_account`).
    ///
    /// On ANY failure for a key (read/write/verify error, or a verify mismatch),
    /// the old keychain item is LEFT IN PLACE — no data loss — and the key is
    /// counted as `failed`. `errSecItemNotFound` is counted as `not_found`
    /// (expected for reserved keys on servers that don't use them).
    ///
    /// Requires the master key to already exist (so `super::file::active()` is
    /// true); the caller ensures that before invoking this.
    pub fn migrate_keychain_to_file(
        keys: &[(String, String)],
    ) -> Result<super::MigrationReport, String> {
        let mut migrated = 0;
        let mut failed = 0;
        let mut not_found = 0;

        for (server_id, key) in keys {
            if server_id.as_str() == super::INTERNAL_SERVER_ID {
                failed += 1;
                continue;
            }
            let acct = account(server_id, key);
            match get_generic_password(SERVICE, &acct) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(value) => {
                        // write -> verify -> delete. Leave the keychain item in
                        // place on any failure so no secret is lost.
                        let moved = super::file::set_secret(server_id, key, &value).is_ok()
                            && matches!(
                                super::file::get_secret_result(server_id, key),
                                Ok(Some(ref v)) if *v == value
                            );
                        if moved {
                            let _ = delete_entry_by_account(&acct);
                            migrated += 1;
                        } else {
                            failed += 1;
                        }
                    }
                    Err(_) => failed += 1, // non-UTF-8 value — can't round-trip
                },
                Err(e) if e.code() == -25300 => not_found += 1, // expected for reserved keys
                Err(_) => failed += 1,                          // locked keychain, denied access
            }
        }

        Ok(super::MigrationReport {
            migrated,
            failed,
            not_found,
        })
    }

    /// Delete all generic-password entries matching a specific account string.
    /// Uses an account-filtered ref search + FFI `SecKeychainItemDelete` — the
    /// only API that can reliably remove legacy file-based items (`SecItemDelete`
    /// returns `errSecAuthFailed` for ACL-bearing entries on some macOS versions).
    fn delete_entry_by_account(account_str: &str) -> Result<usize, String> {
        let results = match ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(SERVICE)
            .account(account_str)
            .limit(Limit::All)
            .load_refs(true)
            .load_data(false)
            .search()
        {
            Ok(results) => results,
            Err(e) if e.code() == errSecItemNotFound => return Ok(0),
            Err(e) => return Err(format!("keychain delete search failed: {e}")),
        };

        let mut deleted = 0;
        for result in results {
            if let SearchResult::Ref(Reference::KeychainItem(item)) = result {
                if delete_keychain_item(&item).is_ok() {
                    deleted += 1;
                }
            }
        }
        Ok(deleted)
    }

    /// Delete a single `SecKeychainItem` via the FFI `SecKeychainItemDelete`.
    fn delete_keychain_item(item: &SecKeychainItem) -> Result<(), i32> {
        use security_framework_sys::keychain_item::SecKeychainItemDelete;
        let status = unsafe { SecKeychainItemDelete(item.as_concrete_TypeRef()) };
        if status == 0 {
            Ok(())
        } else {
            Err(status)
        }
    }
}

// ── Windows / Linux ────────────────────────────────────────────────────────
#[cfg(target_os = "windows")]
#[path = "windows_keyring.rs"]
mod platform;

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
mod platform {
    use keyring::Entry;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::{account, SERVICE};

    /// Serializes every Secret Service operation in this process (SBS-815).
    ///
    /// The `sync-secret-service` backend negotiates a fresh encrypted session
    /// (`dh-ietf1024-sha256-aes128-cbc-pkcs7`) per operation. gnome-keyring 50
    /// races its own per-client bookkeeping when several of those land at once
    /// and aborts the daemon outright (`gkd_secret_service_get_pkcs11_session:
    /// assertion 'client' failed` -> `aes_negotiate: assertion 'session' failed`
    /// -> SIGABRT in `g_variant_new_va`). systemd restarts it, but the login
    /// keyring comes back LOCKED, so a crash costs the user every vault read
    /// until they unlock it by hand.
    ///
    /// These calls used to be serialized for free by the GTK main loop. SBS-813
    /// moved them onto the blocking pool, where they are genuinely concurrent,
    /// so the serialization has to be explicit now.
    static SERVICE_LOCK: Mutex<()> = Mutex::new(());

    const BACKUP_BATCH_TIMEOUT: Duration = Duration::from_secs(15);
    struct BackupBatch {
        deadline: std::time::Instant,
        service: Option<std::rc::Rc<dbus_secret_service::SecretService>>,
        failure: Option<super::BackupSecretError>,
        reads: std::collections::HashMap<String, Result<Option<String>, super::BackupSecretError>>,
        #[cfg(test)]
        connection_attempts: usize,
    }
    thread_local! {
        static BACKUP_BATCH: std::cell::RefCell<Option<BackupBatch>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn with_backup_batch<T>(f: impl FnOnce() -> T) -> T {
        if BACKUP_BATCH.with(|slot| slot.borrow().is_some()) {
            return f();
        }
        BACKUP_BATCH.with(|slot| {
            *slot.borrow_mut() = Some(BackupBatch {
                deadline: std::time::Instant::now() + BACKUP_BATCH_TIMEOUT,
                service: None,
                failure: None,
                reads: Default::default(),
                #[cfg(test)]
                connection_attempts: 0,
            })
        });
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                BACKUP_BATCH.with(|slot| *slot.borrow_mut() = None);
            }
        }
        let _clear = Clear;
        f()
    }

    fn invalidate_backup_read(server_id: &str, key: &str) {
        BACKUP_BATCH.with(|slot| {
            if let Some(batch) = slot.borrow_mut().as_mut() {
                batch.reads.remove(&account(server_id, key));
            }
        });
    }

    fn backup_deadline() -> Result<std::time::Instant, super::BackupSecretError> {
        let deadline = BACKUP_BATCH
            .with(|slot| slot.borrow().as_ref().map(|batch| batch.deadline))
            .ok_or(super::BackupSecretError::Unavailable)?;
        if std::time::Instant::now() >= deadline {
            return Err(super::BackupSecretError::Unavailable);
        }
        Ok(deadline)
    }

    /// Reuse one encrypted session for a bounded recovery operation. Retain the
    /// existing process/file serialization, but never queue behind an unrelated
    /// unlock prompt indefinitely. No operation in this path invokes Unlock.
    fn with_backup_service<T>(
        op: impl FnOnce(&dbus_secret_service::SecretService) -> Result<T, super::BackupSecretError>,
    ) -> Result<T, super::BackupSecretError> {
        use super::BackupSecretError as State;
        let deadline = backup_deadline()?;
        let _serialized = loop {
            match SERVICE_LOCK.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    backup_deadline()?;
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        };
        let dir = crate::registry::conduit_dir().ok_or(State::Unavailable)?;
        let _across_processes = crate::registry::lock_at_for(
            &dir.join("secret-service"),
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(LOCK_TIMEOUT),
        )
        .map_err(|_| State::Unavailable)?;
        backup_deadline()?;
        let service = BACKUP_BATCH.with(|slot| {
            let mut slot = slot.borrow_mut();
            let batch = slot.as_mut().ok_or(State::Unavailable)?;
            if let Some(failure) = batch.failure {
                return Err(failure);
            }
            if let Some(service) = &batch.service {
                return Ok(service.clone());
            }
            #[cfg(test)]
            {
                batch.connection_attempts += 1;
            }
            match dbus_secret_service::SecretService::connect_with_max_prompt_timeout(
                dbus_secret_service::EncryptionType::Dh,
                0,
            ) {
                Ok(service) => {
                    let service = std::rc::Rc::new(service);
                    batch.service = Some(service.clone());
                    Ok(service)
                }
                Err(error) => {
                    let state = classify(&error);
                    batch.failure = Some(state);
                    Err(state)
                }
            }
        })?;
        backup_deadline()?;
        op(&service)
    }

    /// Base wait before the single retry in `with_service`. systemd brings the
    /// daemon back within ~1s of an abort, so one pause slightly longer than that
    /// turns a crash into a recovered operation.
    const RETRY_DELAY: Duration = Duration::from_millis(1200);

    /// Distinct retry slots a process can land in. A process occupies exactly one,
    /// so two processes are either in the same slot or a whole `RETRY_SLOT_MS`
    /// apart — a continuous jitter cannot promise that, and a linear
    /// `pid * k % window` is worse still: it puts every pid pair a fixed distance
    /// apart, so a whole class of pid spacings collapses onto a few milliseconds.
    const RETRY_SLOTS: u64 = 5;

    /// Slot width, comfortably longer than one DH session negotiation, so two
    /// processes in different slots cannot overlap their handshakes.
    const RETRY_SLOT_MS: u64 = 150;

    /// True when the error means the connection died under us rather than
    /// answering the question.
    ///
    /// Only `PlatformFailure` qualifies. keyring 3.6.3 maps exactly `Locked`,
    /// `NoResult` and `Prompt` onto `NoStorageAccess` (`secret_service.rs`
    /// `decode_error`) and everything else — including a vanished D-Bus peer —
    /// onto `PlatformFailure`. So `NoStorageAccess` never means a dead daemon; it
    /// means the collection is locked or an unlock prompt was dismissed. Retrying
    /// it cannot succeed, and it is the state the login keyring comes back in
    /// after the crash this module guards against, so treating it as transient
    /// would tax every later vault read with a pointless sleep and could raise a
    /// second unlock prompt. `NoEntry` is likewise an answer, not a failure.
    ///
    /// One `PlatformFailure` is an answer too: a write to a collection that is
    /// still locked, see [`is_locked_collection`].
    fn is_transient(err: &keyring::Error) -> bool {
        matches!(err, keyring::Error::PlatformFailure(_)) && !is_locked_collection(err)
    }

    /// The Secret Service's own error for a call on a locked collection or item.
    const IS_LOCKED: &str = "org.freedesktop.Secret.Error.IsLocked";

    /// True when a write failed because the collection is still locked.
    ///
    /// keyring 3.6.3's `set_password` asks to unlock the collection; when that ask
    /// is dismissed it falls back to `create_collection` and then `create_item`,
    /// which the service refuses with `IsLocked`. keyring passes that on as a
    /// `PlatformFailure` wrapping `dbus_secret_service::Error::Dbus`, not as
    /// `NoStorageAccess`. Matched by the D-Bus error name, never by its text.
    /// Retrying it asks to unlock a second time and cannot succeed.
    fn is_locked_collection(err: &keyring::Error) -> bool {
        let keyring::Error::PlatformFailure(inner) = err else {
            return false;
        };
        matches!(
            inner.downcast_ref::<dbus_secret_service::Error>(),
            Some(dbus_secret_service::Error::Dbus(error)) if error.name() == Some(IS_LOCKED)
        )
    }

    /// Cross-process serialization for Secret Service access.
    ///
    /// `SERVICE_LOCK` only covers this process, but the app and the gateway are
    /// two processes talking to the same daemon and either pair racing is enough
    /// to abort it. This is the registry's own file-lock primitive on a dedicated
    /// `secret-service.lock`, so both binaries queue behind one another instead of
    /// negotiating sessions at the same moment.
    ///
    /// Best effort on purpose. If the lock cannot be taken — no data directory, or
    /// a peer held it past the timeout — the operation still runs under the
    /// in-process mutex and the staggered retry. Failing a vault read because a
    /// lock file was contended would be a worse outcome than the race it guards.
    fn cross_process_guard() -> Option<crate::registry::FileLock> {
        let dir = crate::registry::conduit_dir()?;
        crate::registry::lock_at_for(&dir.join("secret-service"), LOCK_TIMEOUT).ok()
    }

    /// Deadline for `cross_process_guard`, deliberately NOT the registry's 5s
    /// default. That default is sized for a load-modify-save of a whole file;
    /// this lock guards one D-Bus call that normally takes tens of milliseconds.
    ///
    /// The longest a peer can legitimately hold it is its own staggered retry:
    /// `RETRY_DELAY` plus the widest slot (1200 + 4 × 150 = 1800ms) plus the two
    /// attempts either side. 2.5s covers that with headroom and still bounds the
    /// wait — which matters because `install_gateway` and `uninstall_gateway`
    /// reach the vault on the GTK main loop, where 5s of held lock would be a
    /// visible freeze. Past the deadline the operation proceeds unlocked rather
    /// than failing, so this caps a stall; it does not turn one into an error.
    const LOCK_TIMEOUT: Duration = Duration::from_millis(2500);

    /// Wait before the retry, staggered per process.
    ///
    /// Belt and braces behind `cross_process_guard`: when that lock is
    /// unavailable, a crash can still be triggered by the app and the gateway
    /// racing. If both then slept exactly `RETRY_DELAY` they would wake on the
    /// same tick and negotiate two sessions at once — the same race again, with
    /// each side's single retry already spent.
    fn retry_delay() -> Duration {
        RETRY_DELAY + Duration::from_millis(retry_slot(std::process::id()) * RETRY_SLOT_MS)
    }

    /// Which retry slot this pid occupies, via splitmix64's finalizer: one changed
    /// bit in the pid changes about half the output bits, so pids differing by 1,
    /// 2 or 4 — the usual spacing when one process spawns another — do not fall
    /// into a fixed pattern the way a plain multiply-and-mod does.
    fn retry_slot(pid: u32) -> u64 {
        let mut z = u64::from(pid).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        z % RETRY_SLOTS
    }

    /// Run one Secret Service operation with the process lock held, retrying it
    /// once if the service died mid-call.
    ///
    /// `op` covers `Entry::new` as well as the read or write, but only for
    /// convenience: in keyring 3.6.3 `Entry::new` just builds an in-memory
    /// attribute map (`SsCredential::new_with_target`) and never touches D-Bus.
    /// The retry exists for the read or write that follows it, which is where
    /// `SecretService::connect` opens the session.
    ///
    /// The lock is deliberately held across the retry sleep: any thread that would
    /// take it in that window is talking to the same restarting daemon and would
    /// only fail. Most callers are on the blocking pool (SBS-813), but
    /// `install_gateway` and `uninstall_gateway` are still sync commands that
    /// reach the vault on the GTK main loop, so a genuine daemon death can stall
    /// window controls for the delay. Narrowing `is_transient` keeps that off the
    /// common paths (a locked keyring no longer sleeps at all); moving those two
    /// commands onto the blocking pool is the real fix and is tracked separately.
    ///
    /// Other processes are covered by `cross_process_guard`; the staggered retry
    /// backs that up for the case where the file lock could not be taken.
    fn with_service<T>(op: impl FnMut() -> Result<T, keyring::Error>) -> Result<T, keyring::Error> {
        with_service_after(retry_delay(), op)
    }

    fn with_service_after<T>(
        delay: Duration,
        mut op: impl FnMut() -> Result<T, keyring::Error>,
    ) -> Result<T, keyring::Error> {
        // Always this order — in-process mutex, then the file lock — so two
        // threads can never hold one and wait on the other.
        let _serialized = SERVICE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _across_processes = cross_process_guard();
        let result = match op() {
            Err(e) if is_transient(&e) => {
                // Say so. Otherwise the only evidence that SBS-815 recovery ran
                // is a 1.2-1.7s stall, and a vault failure that persists past the
                // retry (the keyring coming back LOCKED, say) looks identical to
                // one that never had a daemon crash behind it.
                eprintln!("toolport: secret service died mid-call ({e}); retrying once");
                std::thread::sleep(delay);
                let retried = op();
                if let Err(e) = &retried {
                    eprintln!("toolport: secret service still failing after the retry ({e})");
                }
                retried
            }
            other => other,
        };
        // Locked, or its unlock dialog dismissed: this call asked and was refused.
        if matches!(result, Err(keyring::Error::NoStorageAccess(_)))
            || result.as_ref().err().is_some_and(is_locked_collection)
        {
            super::note_refused_ask();
        }
        result
    }

    pub fn set_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
        invalidate_backup_read(server_id, key);
        with_service(|| Entry::new(SERVICE, &account(server_id, key))?.set_password(value))
            .map_err(|e| e.to_string())
    }

    pub fn get_secret_result(server_id: &str, key: &str) -> Result<Option<String>, String> {
        #[cfg(test)]
        crate::secrets::tests::record_vault_read(server_id, key);
        read_secret(server_id, key).map_err(super::ReadFailure::into_message)
    }

    /// [`get_secret_result`] with the kind of failure.
    pub fn read_secret(server_id: &str, key: &str) -> Result<Option<String>, super::ReadFailure> {
        read_kinded(with_service(|| {
            Entry::new(SERVICE, &account(server_id, key))?.get_password()
        }))
    }

    /// An item whose secret is not text is damaged; a locked, missing or
    /// failing service, or more than one match, is the store's state.
    pub(super) fn read_kinded(
        read: Result<String, keyring::Error>,
    ) -> Result<Option<String>, super::ReadFailure> {
        match read {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e @ keyring::Error::BadEncoding(_)) => {
                Err(super::ReadFailure::Damaged(e.to_string()))
            }
            Err(e) => Err(super::ReadFailure::Unavailable(e.to_string())),
        }
    }

    /// How long a check of its own waits for another keyring operation before
    /// it answers "unknown".
    const STORE_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

    /// Whether a read could ask to unlock, by one search that never unlocks.
    /// Only a search that finds no locked Toolport item, or no Secret Service
    /// to connect to (a read cannot prompt either), says no. Unknown (another
    /// keyring operation holds the store, a timeout, a failed search) says
    /// yes: background work waits rather than risk a prompt.
    pub fn store_would_prompt() -> bool {
        would_prompt(false)
    }

    /// [`store_would_prompt`] for a write: a new item goes into the default
    /// collection, and writing to a locked (or missing) one asks too, even
    /// with no Toolport item in the store.
    pub fn write_would_prompt() -> bool {
        would_prompt(true)
    }

    fn would_prompt(write: bool) -> bool {
        let search = || {
            let found = with_backup_service(|service| {
                super::would_prompt_by(
                    write,
                    || match service.get_default_collection() {
                        Ok(collection) => collection
                            .is_locked()
                            .map(Some)
                            .map_err(|error| classify(&error)),
                        Err(dbus_secret_service::Error::NoResult) => Ok(None),
                        Err(error) => Err(classify(&error)),
                    },
                    || {
                        service
                            .search_items(std::collections::HashMap::from([("service", SERVICE)]))
                            .map(|found| !found.locked.is_empty())
                            .map_err(|error| classify(&error))
                    },
                )
            });
            let no_service = || {
                BACKUP_BATCH.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .is_some_and(|batch| batch.service.is_none() && batch.failure.is_some())
                })
            };
            match found {
                Ok(locked) => locked,
                Err(_) if no_service() => false,
                Err(_) => true,
            }
        };
        if BACKUP_BATCH.with(|slot| slot.borrow().is_some()) {
            search()
        } else {
            with_backup_batch(|| {
                BACKUP_BATCH.with(|slot| {
                    if let Some(batch) = slot.borrow_mut().as_mut() {
                        batch.deadline = batch
                            .deadline
                            .min(std::time::Instant::now() + STORE_CHECK_TIMEOUT);
                    }
                });
                search()
            })
        }
    }

    /// Owners with a locked Toolport value in any collection, never asking.
    /// The default collection open: the one search, as before. Locked (where
    /// KeePassXC answers a search with its dialog): property reads of the
    /// locked collections' items only. `None`: unknown (busy, timed out, a
    /// failed read); the next probe round checks again.
    pub fn owners_with_locked_values() -> Option<std::collections::HashSet<String>> {
        let read = || {
            let found = with_backup_service(|service| {
                let failed = |error: dbus_secret_service::Error| classify(&error);
                let owner_of = |item: &dbus_secret_service::Item| {
                    item.get_attributes().map_err(failed).map(|attributes| {
                        (attributes.get("service").map(String::as_str) == Some(SERVICE))
                            .then(|| attributes.get("username").cloned())
                            .flatten()
                    })
                };
                super::owners_with_locked_by(
                    || match service.get_default_collection() {
                        Ok(collection) => collection.is_locked().map(Some).map_err(failed),
                        Err(dbus_secret_service::Error::NoResult) => Ok(None),
                        Err(error) => Err(failed(error)),
                    },
                    || {
                        let found = service
                            .search_items(std::collections::HashMap::from([("service", SERVICE)]))
                            .map_err(failed)?;
                        found.locked.iter().map(owner_of).collect()
                    },
                    || {
                        let collections = service.get_all_collections().map_err(failed)?;
                        super::accounts_in_locked(
                            &collections,
                            |collection| collection.is_locked().map_err(failed),
                            |collection| {
                                collection
                                    .get_all_items()
                                    .map_err(failed)?
                                    .iter()
                                    .map(owner_of)
                                    .collect()
                            },
                        )
                    },
                )
            });
            let no_service = BACKUP_BATCH.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .is_some_and(|batch| batch.service.is_none() && batch.failure.is_some())
            });
            match found {
                Ok(owners) => Some(owners),
                // Nothing to read from, so nothing to ask for either.
                Err(_) if no_service => Some(Default::default()),
                Err(_) => None,
            }
        };
        if BACKUP_BATCH.with(|slot| slot.borrow().is_some()) {
            read()
        } else {
            with_backup_batch(|| {
                BACKUP_BATCH.with(|slot| {
                    if let Some(batch) = slot.borrow_mut().as_mut() {
                        batch.deadline = batch
                            .deadline
                            .min(std::time::Instant::now() + STORE_CHECK_TIMEOUT);
                    }
                });
                read()
            })
        }
    }

    fn read_backup_item<T>(
        unlocked: usize,
        locked: usize,
        read: impl FnOnce() -> Result<T, keyring::Error>,
    ) -> Result<Option<T>, keyring::Error> {
        if locked > 0 || unlocked > 1 {
            return Err(keyring::Error::NoStorageAccess(Box::new(
                if locked + unlocked > 1 {
                    super::BackupSecretError::Ambiguous
                } else {
                    super::BackupSecretError::Locked
                },
            )));
        }
        if unlocked == 0 {
            return Ok(None);
        }
        read().map(Some)
    }

    /// Recovery must not wait for a desktop unlock prompt. Search classifies
    /// locked items without unlocking them; GetSecret also fails if an item is
    /// locked between the search and read. Neither call requests Unlock.
    fn classify(error: &dbus_secret_service::Error) -> super::BackupSecretError {
        use super::BackupSecretError as State;
        match error {
            dbus_secret_service::Error::Locked => State::Locked,
            dbus_secret_service::Error::Prompt => State::Cancelled,
            dbus_secret_service::Error::Crypto(_)
            | dbus_secret_service::Error::Parse
            | dbus_secret_service::Error::UnsupportedSecretFormat => State::Corrupt,
            dbus_secret_service::Error::Dbus(error) if error.name() == Some(IS_LOCKED) => {
                State::Locked
            }
            dbus_secret_service::Error::Dbus(error)
                if error.name() == Some("org.freedesktop.DBus.Error.AccessDenied") =>
            {
                State::AccessDenied
            }
            _ => State::Unavailable,
        }
    }
    fn backup_keyring_error(error: keyring::Error) -> super::BackupSecretError {
        use super::BackupSecretError as State;
        match error {
            keyring::Error::PlatformFailure(e) | keyring::Error::NoStorageAccess(e) => e
                .downcast_ref::<State>()
                .copied()
                .or_else(|| e.downcast_ref::<dbus_secret_service::Error>().map(classify))
                .unwrap_or(State::Unavailable),
            keyring::Error::BadEncoding(_) => State::Corrupt,
            _ => State::Unavailable,
        }
    }
    pub fn get_backup_secret_status(
        server_id: &str,
        key: &str,
    ) -> Result<Option<String>, super::BackupSecretError> {
        use super::BackupSecretError as State;
        if !BACKUP_BATCH.with(|slot| slot.borrow().is_some()) {
            return with_backup_batch(|| get_backup_secret_status(server_id, key));
        }
        let account = account(server_id, key);
        if let Some(cached) = BACKUP_BATCH.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|batch| batch.reads.get(&account).cloned())
        }) {
            return cached;
        }
        let result = with_backup_service(|service| {
            let entries = service
                .search_items(std::collections::HashMap::from([
                    ("service", SERVICE),
                    ("username", account.as_str()),
                ]))
                .map_err(|error| classify(&error))?;
            if entries.unlocked.len() + entries.locked.len() > 1 {
                return Err(State::Ambiguous);
            }
            if !entries.locked.is_empty() {
                return Err(State::Locked);
            }
            let Some(item) = entries.unlocked.first() else {
                return Ok(None);
            };
            backup_deadline()?;
            let bytes = item.get_secret().map_err(|error| classify(&error))?;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| State::Corrupt)
        });
        BACKUP_BATCH.with(|slot| {
            if let Some(batch) = slot.borrow_mut().as_mut() {
                batch.reads.insert(account, result.clone());
            }
        });
        result
    }

    /// Never prompt, even if an item locks between SearchItems and Delete.
    pub fn delete_backup_secret_status(
        server_id: &str,
        key: &str,
        discard_all: bool,
    ) -> Result<(), super::BackupSecretError> {
        use super::BackupSecretError as State;
        if !BACKUP_BATCH.with(|slot| slot.borrow().is_some()) {
            return with_backup_batch(|| delete_backup_secret_status(server_id, key, discard_all));
        }
        let account = account(server_id, key);
        // A failed partial deletion must not leave a cached credential behind.
        BACKUP_BATCH.with(|slot| {
            if let Some(batch) = slot.borrow_mut().as_mut() {
                batch.reads.remove(&account);
            }
        });
        with_backup_service(|service| {
            let entries = service
                .search_items(std::collections::HashMap::from([
                    ("service", SERVICE),
                    ("username", account.as_str()),
                ]))
                .map_err(|error| classify(&error))?;
            if discard_all {
                if !entries.locked.is_empty() {
                    return Err(State::Locked);
                }
                for item in entries.unlocked {
                    backup_deadline()?;
                    item.delete().map_err(|error| classify(&error))?;
                }
                Ok(())
            } else {
                read_backup_item(entries.unlocked.len(), entries.locked.len(), || {
                    backup_deadline()
                        .map_err(|state| keyring::Error::NoStorageAccess(Box::new(state)))?;
                    entries.unlocked[0].delete().map_err(|error| {
                        keyring::Error::PlatformFailure(Box::new(classify(&error)))
                    })
                })
                .map(|_| ())
                .map_err(backup_keyring_error)
            }
        })
    }

    pub fn delete_secret(server_id: &str, key: &str) -> Result<(), String> {
        invalidate_backup_read(server_id, key);
        match with_service(|| Entry::new(SERVICE, &account(server_id, key))?.delete_credential()) {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn backup_batch_cache_is_scoped_and_cleared_even_after_panic() {
            with_backup_batch(|| {
                BACKUP_BATCH.with(|slot| {
                    slot.borrow_mut()
                        .as_mut()
                        .unwrap()
                        .reads
                        .insert("fixture".into(), Ok(Some("private value".into())))
                });
                with_backup_batch(|| {
                    assert!(BACKUP_BATCH.with(|slot| slot
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .reads
                        .contains_key("fixture")));
                });
            });
            assert!(BACKUP_BATCH.with(|slot| slot.borrow().is_none()));
            let result = std::panic::catch_unwind(|| {
                with_backup_batch(|| {
                    BACKUP_BATCH.with(|slot| {
                        slot.borrow_mut()
                            .as_mut()
                            .unwrap()
                            .reads
                            .insert("fixture".into(), Ok(Some("private value".into())))
                    });
                    panic!("test unwind");
                })
            });
            assert!(result.is_err());
            assert!(BACKUP_BATCH.with(|slot| slot.borrow().is_none()));
        }

        #[test]
        fn backup_batch_deadline_cancels_before_opening_the_vault() {
            with_backup_batch(|| {
                BACKUP_BATCH.with(|slot| {
                    slot.borrow_mut().as_mut().unwrap().deadline = std::time::Instant::now()
                });
                assert_eq!(
                    get_backup_secret_status("fixture", "key"),
                    Err(super::super::BackupSecretError::Unavailable)
                );
                assert_eq!(
                    BACKUP_BATCH.with(|slot| slot.borrow().as_ref().unwrap().connection_attempts),
                    0
                );
            });
        }

        /// Failure mode: while another keyring operation held the store, the
        /// check could not search and answered "would not prompt", so
        /// background work went on to a read that asks to unlock.
        #[test]
        fn a_busy_store_counts_as_one_that_would_prompt() {
            let _held = SERVICE_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let started = std::time::Instant::now();
            assert!(store_would_prompt(), "unknown must wait, not read");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{:?}",
                started.elapsed()
            );
        }

        #[test]
        fn backup_batch_does_not_wait_indefinitely_for_another_keyring_operation() {
            let _held = SERVICE_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            with_backup_batch(|| {
                BACKUP_BATCH.with(|slot| {
                    slot.borrow_mut().as_mut().unwrap().deadline =
                        std::time::Instant::now() + Duration::from_millis(40)
                });
                let started = std::time::Instant::now();
                assert_eq!(
                    get_backup_secret_status("fixture", "key"),
                    Err(super::super::BackupSecretError::Unavailable)
                );
                assert!(started.elapsed() < Duration::from_secs(2));
                assert_eq!(
                    BACKUP_BATCH.with(|slot| slot.borrow().as_ref().unwrap().connection_attempts),
                    0
                );
            });
        }

        #[test]
        #[ignore = "requires an isolated unlocked Secret Service"]
        fn native_backup_batch_reuses_one_encrypted_session_and_invalidates_deleted_values() {
            let _data = crate::registry::DataDirTestEnv::new("native-backup-batch");
            let server = "toolport-native-backup-batch-fixture";
            struct Cleanup(&'static str);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    for i in 0..21 {
                        let _ = delete_secret(self.0, &format!("KEY_{i}"));
                    }
                }
            }
            let _cleanup = Cleanup(server);
            for i in 0..20 {
                set_secret(
                    server,
                    &format!("KEY_{i}"),
                    &format!("native-private-value-{i}"),
                )
                .unwrap();
            }
            with_backup_batch(|| {
                for i in 0..20 {
                    for _ in 0..2 {
                        assert_eq!(
                            get_backup_secret_status(server, &format!("KEY_{i}")).unwrap(),
                            Some(format!("native-private-value-{i}"))
                        );
                    }
                }
                set_secret(server, "KEY_0", "updated-native-private-value").unwrap();
                assert_eq!(
                    get_backup_secret_status(server, "KEY_0")
                        .unwrap()
                        .as_deref(),
                    Some("updated-native-private-value")
                );
                assert_eq!(get_backup_secret_status(server, "KEY_20").unwrap(), None);
                set_secret(server, "KEY_20", "new-native-private-value").unwrap();
                assert_eq!(
                    get_backup_secret_status(server, "KEY_20")
                        .unwrap()
                        .as_deref(),
                    Some("new-native-private-value")
                );
                delete_secret(server, "KEY_20").unwrap();
                assert_eq!(get_backup_secret_status(server, "KEY_20").unwrap(), None);
                for i in 0..20 {
                    delete_backup_secret_status(server, &format!("KEY_{i}"), false).unwrap();
                    assert_eq!(
                        get_backup_secret_status(server, &format!("KEY_{i}")).unwrap(),
                        None
                    );
                    delete_backup_secret_status(server, &format!("KEY_{i}"), true).unwrap();
                }
                assert_eq!(
                    BACKUP_BATCH.with(|slot| slot.borrow().as_ref().unwrap().connection_attempts),
                    1
                );
            });
            assert!(BACKUP_BATCH.with(|slot| slot.borrow().is_none()));
        }

        #[test]
        fn backup_recovery_never_reads_locked_or_ambiguous_items() {
            for (unlocked, locked) in [(0, 1), (1, 1), (2, 0)] {
                let result =
                    read_backup_item(unlocked, locked, || -> Result<(), keyring::Error> {
                        panic!("locked recovery must not read or request unlock")
                    });
                assert!(matches!(result, Err(keyring::Error::NoStorageAccess(_))));
            }
            assert_eq!(read_backup_item(0, 0, || Ok("unexpected")).unwrap(), None);
            assert_eq!(
                read_backup_item(1, 0, || Ok("exact payload")).unwrap(),
                Some("exact payload")
            );
        }
        #[test]
        fn backup_cleanup_never_deletes_locked_or_ambiguous_items() {
            for (unlocked, locked) in [(0, 1), (1, 1), (2, 0)] {
                let result =
                    read_backup_item(unlocked, locked, || -> Result<(), keyring::Error> {
                        panic!(
                            "locked cleanup must retain recovery instead of deleting or prompting"
                        )
                    });
                assert!(matches!(result, Err(keyring::Error::NoStorageAccess(_))));
            }
            assert_eq!(read_backup_item(0, 0, || Ok(())).unwrap(), None);
            assert_eq!(read_backup_item(1, 0, || Ok(())).unwrap(), Some(()));
        }

        use std::sync::atomic::{AtomicUsize, Ordering};

        /// A secret that is not text can never be read back, so it is
        /// damaged; a locked or failing service may give it up later.
        #[test]
        fn a_read_that_cannot_decode_is_damaged_and_a_failing_store_is_not() {
            assert!(matches!(
                read_kinded(Err(keyring::Error::BadEncoding(vec![0xff, 0xfe]))),
                Err(crate::secrets::ReadFailure::Damaged(_))
            ));
            for error in [
                keyring::Error::NoStorageAccess("the collection is locked".into()),
                keyring::Error::PlatformFailure("org.freedesktop.secrets vanished".into()),
            ] {
                assert!(matches!(
                    read_kinded(Err(error)),
                    Err(crate::secrets::ReadFailure::Unavailable(_))
                ));
            }
            assert_eq!(read_kinded(Err(keyring::Error::NoEntry)), Ok(None));
        }

        fn dead_daemon() -> keyring::Error {
            keyring::Error::PlatformFailure("org.freedesktop.secrets vanished".into())
        }

        /// The gnome-keyring 50 abort recovery path (SBS-815): the daemon dies
        /// mid-operation, systemd restarts it, and the retry succeeds where the
        /// first attempt could not.
        #[test]
        fn a_secret_service_op_retries_once_when_the_daemon_dies_mid_call() {
            let _data = crate::registry::DataDirTestEnv::new(
                "a_secret_service_op_retries_once_when_the_daemon_dies_mid_call",
            );
            let calls = AtomicUsize::new(0);
            let result = with_service_after(Duration::ZERO, || {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(dead_daemon())
                } else {
                    Ok("recovered")
                }
            });

            assert_eq!(result.unwrap(), "recovered");
            assert_eq!(calls.load(Ordering::SeqCst), 2, "one retry, not a loop");
        }

        /// `NoEntry` answers the question, so retrying it would only double the
        /// D-Bus traffic that causes the crash in the first place.
        #[test]
        fn a_secret_service_op_does_not_retry_a_definitive_answer() {
            let _data = crate::registry::DataDirTestEnv::new(
                "a_secret_service_op_does_not_retry_a_definitive_answer",
            );
            let calls = AtomicUsize::new(0);
            let result = with_service_after(Duration::ZERO, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(keyring::Error::NoEntry)
            });

            assert!(matches!(result, Err(keyring::Error::NoEntry)));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry for an answer");
        }

        /// A locked keyring is exactly the state the daemon comes back in after
        /// the crash, and keyring reports it as `NoStorageAccess`. Retrying it
        /// cannot succeed, would tax every later read with a sleep, and can raise
        /// a second unlock prompt — so it must be attempted once only.
        #[test]
        fn a_secret_service_op_does_not_retry_a_locked_keyring() {
            let _data = crate::registry::DataDirTestEnv::new(
                "a_secret_service_op_does_not_retry_a_locked_keyring",
            );
            let calls = AtomicUsize::new(0);
            let result = with_service_after(Duration::ZERO, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(keyring::Error::NoStorageAccess(
                    "the collection is locked".into(),
                ))
            });

            assert!(matches!(result, Err(keyring::Error::NoStorageAccess(_))));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "a locked collection is an answer, not a dead daemon"
            );
        }

        fn locked_collection() -> keyring::Error {
            keyring::Error::PlatformFailure(Box::new(dbus_secret_service::Error::Dbus(
                dbus::Error::new_custom(IS_LOCKED, "Cannot create an item in a locked collection"),
            )))
        }

        /// A write to a locked collection whose unlock prompt was dismissed is an
        /// answer: it is attempted once, so the user is asked once, and it counts
        /// as a refused ask so the rest of an `asking_once` run does not ask again.
        #[test]
        fn a_write_to_a_locked_collection_is_not_retried_and_counts_as_refused() {
            let _data = crate::registry::DataDirTestEnv::new(
                "a_write_to_a_locked_collection_is_not_retried_and_counts_as_refused",
            );
            let calls = AtomicUsize::new(0);
            let (result, refused) = super::super::asking_once(|| {
                with_service_after(Duration::ZERO, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(locked_collection())
                })
            });
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1, "asked once, not twice");
            assert!(refused, "a dismissed unlock is a refused ask");
        }

        /// Only that one D-Bus error is an answer: any other platform failure,
        /// another D-Bus error included, keeps the single retry.
        #[test]
        fn other_platform_failures_keep_their_retry() {
            let _data =
                crate::registry::DataDirTestEnv::new("other_platform_failures_keep_their_retry");
            for error in [
                || {
                    keyring::Error::PlatformFailure(Box::new(dbus_secret_service::Error::Dbus(
                        dbus::Error::new_custom("org.freedesktop.DBus.Error.NoReply", "no reply"),
                    )))
                },
                || {
                    keyring::Error::PlatformFailure(Box::new(
                        dbus_secret_service::Error::Unavailable,
                    ))
                },
                dead_daemon,
            ] {
                let calls = AtomicUsize::new(0);
                let (result, refused) = super::super::asking_once(|| {
                    with_service_after(Duration::ZERO, || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Err::<(), _>(error())
                    })
                });
                assert!(result.is_err());
                assert_eq!(calls.load(Ordering::SeqCst), 2, "one retry");
                assert!(!refused);
            }
        }

        /// The classification above rests on how keyring 3.6.3 and
        /// dbus-secret-service 4.1 report a refused write: as a platform failure
        /// carrying the D-Bus error, shown as below (the line a real locked keyring
        /// logs). A version bump that changes either needs `is_locked_collection`
        /// checked again against a real locked keyring.
        #[test]
        fn the_locked_collection_error_is_the_one_keyring_reports() {
            assert_eq!(
                locked_collection().to_string(),
                "Platform secure storage failure: DBus error: Cannot create an item in a locked collection"
            );
            let lock = include_str!("../Cargo.lock");
            for (name, version) in [("keyring", "3.6.3"), ("dbus-secret-service", "4.1.0")] {
                assert!(
                    lock.contains(&format!("name = \"{name}\"\nversion = \"{version}\"\n")),
                    "{name} is no longer {version}: check how it reports a write to a locked \
                     collection before keeping is_locked_collection"
                );
            }
        }

        /// The app and the gateway must not wake from the retry on the same tick,
        /// or they re-run the very race that killed the daemon. Slots make the
        /// separation structural: different slot => a full `RETRY_SLOT_MS` apart,
        /// never a few milliseconds.
        #[test]
        fn the_retry_stagger_separates_processes_by_a_whole_slot() {
            for pid in [1_u32, 2, 999, 4_100, 65_535, u32::MAX] {
                assert!(retry_slot(pid) < RETRY_SLOTS, "slot out of range for {pid}");
            }

            // Two processes in different slots are a whole slot apart, which is
            // longer than the DH handshake they must not overlap.
            let delay_of = |pid: u32| retry_slot(pid) * RETRY_SLOT_MS;
            let a = 4_100_u32;
            let b = (a + 1..).find(|p| retry_slot(*p) != retry_slot(a)).unwrap();
            assert!(delay_of(a).abs_diff(delay_of(b)) >= RETRY_SLOT_MS);
        }

        /// The failure mode of the multiply-and-mod stagger this replaced: it was
        /// linear, so EVERY pair of pids a given distance apart landed the same
        /// fixed gap apart (22ms for a delta of 2), and a whole class of spawn
        /// spacings collapsed together. A mixing hash must not do that.
        #[test]
        fn the_retry_stagger_does_not_map_a_pid_delta_to_one_fixed_gap() {
            for delta in [1_u32, 2, 4] {
                let gaps: std::collections::HashSet<u64> = (4_000_u32..4_200)
                    .map(|pid| {
                        (retry_slot(pid) * RETRY_SLOT_MS)
                            .abs_diff(retry_slot(pid + delta) * RETRY_SLOT_MS)
                    })
                    .collect();
                assert!(
                    gaps.len() > 2,
                    "delta {delta} produces only {} distinct gap(s) — the stagger is linear",
                    gaps.len()
                );
            }
        }

        /// The cross-process deadline has to sit in a window: long enough to wait
        /// out a peer's staggered retry (giving up exactly while the daemon is
        /// restarting would drop the serialization when it matters most), short
        /// enough that a held lock is not a visible freeze on the GTK main-loop
        /// callers. Pins the relationship so bumping the retry constants alone
        /// cannot silently invalidate it.
        #[test]
        fn the_cross_process_deadline_outlasts_a_peer_retry_but_stays_bounded() {
            let widest_retry_sleep =
                RETRY_DELAY + Duration::from_millis((RETRY_SLOTS - 1) * RETRY_SLOT_MS);

            assert!(
                LOCK_TIMEOUT > widest_retry_sleep,
                "{LOCK_TIMEOUT:?} would abandon the lock while a peer is still retrying \
                 ({widest_retry_sleep:?})"
            );
            assert!(
                LOCK_TIMEOUT < Duration::from_secs(5),
                "the registry's 5s default is for a file rewrite, not one D-Bus call"
            );
        }

        /// A hash that funnelled every nearby pid into one slot would separate
        /// nothing, so check the slots are actually used.
        #[test]
        fn the_retry_stagger_uses_every_slot() {
            let used: std::collections::HashSet<u64> = (4_000_u32..4_100).map(retry_slot).collect();
            assert_eq!(
                used.len() as u64,
                RETRY_SLOTS,
                "100 consecutive pids should reach all {RETRY_SLOTS} slots, saw {used:?}"
            );
        }

        /// A service that stays down surfaces the failure instead of retrying
        /// forever — callers distinguish a read failure from "never saved"
        /// (SBS-789), so the error has to reach them.
        #[test]
        fn a_secret_service_op_gives_up_after_a_single_retry() {
            let _data = crate::registry::DataDirTestEnv::new(
                "a_secret_service_op_gives_up_after_a_single_retry",
            );
            let calls = AtomicUsize::new(0);
            let result = with_service_after(Duration::ZERO, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(dead_daemon())
            });

            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 2, "one attempt, one retry");
        }

        /// The actual fix: concurrent callers never negotiate two sessions at
        /// once, which is what races gnome-keyring's per-client bookkeeping.
        #[test]
        fn secret_service_ops_never_overlap_across_threads() {
            let _data = crate::registry::DataDirTestEnv::new(
                "secret_service_ops_never_overlap_across_threads",
            );
            static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
            static MAX_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

            std::thread::scope(|scope| {
                for _ in 0..8 {
                    scope.spawn(|| {
                        with_service_after(Duration::ZERO, || {
                            let now = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
                            MAX_IN_FLIGHT.fetch_max(now, Ordering::SeqCst);
                            // Wide enough that unserialized threads would overlap.
                            std::thread::sleep(Duration::from_millis(5));
                            IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
                            Ok::<_, keyring::Error>(())
                        })
                        .unwrap();
                    });
                }
            });

            assert_eq!(
                MAX_IN_FLIGHT.load(Ordering::SeqCst),
                1,
                "two sessions negotiated at once is exactly what crashes the daemon"
            );
        }
    }
}

/// Encrypted-file secret backend for headless / no-keychain environments. It is
/// activated by setting `TOOLPORT_SECRET_KEY` (legacy `CONDUIT_SECRET_KEY`) to any
/// non-empty string: a 32-byte key is
/// derived from it (SHA-256) and secrets live in `secrets.enc` as one XChaCha20-Poly1305
/// sealed JSON map, re-sealed on every write. With the env var unset the OS keychain is
/// used exactly as before, so desktop installs are untouched. The same env var must be
/// present for both the app (writes) and the gateway (reads).
mod file {
    use super::account;
    use base64::Engine;
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const NONCE_LEN: usize = 24;

    /// account -> secret value.
    type Store = BTreeMap<String, String>;

    /// The 32-byte key that encrypts `secrets.enc`, or None when the file backend
    /// is inactive (the signal to use the OS keychain directly).
    ///
    /// The file backend is **headless-only**: it activates IFF `TOOLPORT_SECRET_KEY`
    /// (legacy `CONDUIT_SECRET_KEY`) is set + non-empty (the key is SHA-256 of it; the app and gateway must agree
    /// on the env var). On every desktop OS this returns None, so secrets live in
    /// the OS keychain — on macOS that is the *data-protection* keychain under the
    /// team-scoped shared access group (`platform`), which keeps keys off disk and
    /// lets the separately-signed gateway read them with no prompt.
    ///
    /// We deliberately do NOT derive the key from a keychain master item on
    /// desktop. Doing so would activate `secrets.enc` on disk and shadow the
    /// data-protection keychain — the "keys never on disk" property we sell — and
    /// reading that master item across an app update is itself a prompt source.
    fn key_material() -> Option<[u8; 32]> {
        let secret = crate::brand::env_var("TOOLPORT_SECRET_KEY", "CONDUIT_SECRET_KEY")?;
        if secret.is_empty() {
            return None;
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&Sha256::digest(secret.as_bytes()));
        Some(key)
    }

    pub fn active() -> bool {
        key_material().is_some()
    }

    fn path() -> Result<PathBuf, String> {
        crate::registry::conduit_dir()
            .map(|d| d.join("secrets.enc"))
            .ok_or_else(|| "no Toolport data directory".to_string())
    }

    /// Seal `plain` as base64(`nonce` || ciphertext) under `key` with a fresh nonce.
    pub(super) fn seal(key: &[u8; 32], plain: &[u8]) -> Result<String, String> {
        let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|e| e.to_string())?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).map_err(|e| e.to_string())?;
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), plain)
            .map_err(|_| "encryption failed".to_string())?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
    }

    /// Reverse of `seal`: authenticate and decrypt. Fails on a wrong key or tamper.
    pub(super) fn open(key: &[u8; 32], encoded: &str) -> Result<Vec<u8>, String> {
        let blob = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|e| e.to_string())?;
        if blob.len() < NONCE_LEN {
            return Err("secrets.enc is truncated or corrupt".to_string());
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|e| e.to_string())?;
        cipher.decrypt(XNonce::from_slice(nonce), ct).map_err(|_| {
            "could not decrypt secrets.enc (wrong TOOLPORT_SECRET_KEY/CONDUIT_SECRET_KEY?)"
                .to_string()
        })
    }

    fn load() -> Result<Store, String> {
        let key =
            key_material().ok_or("TOOLPORT_SECRET_KEY (legacy CONDUIT_SECRET_KEY) is not set")?;
        let encoded = match std::fs::read_to_string(path()?) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Store::new()),
            Err(e) => return Err(e.to_string()),
        };
        serde_json::from_slice(&open(&key, &encoded)?).map_err(|e| e.to_string())
    }

    fn save(store: &Store) -> Result<(), String> {
        let key =
            key_material().ok_or("TOOLPORT_SECRET_KEY (legacy CONDUIT_SECRET_KEY) is not set")?;
        let plain = serde_json::to_vec(store).map_err(|e| e.to_string())?;
        crate::registry::atomic_write(&path()?, &seal(&key, &plain)?)
    }

    pub fn set_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
        // Cross-process lock around load-modify-save so concurrent writers cannot
        // drop each other's keys (SOU-332). Same sibling-lock pattern as the registry.
        let path = path()?;
        let _lock = crate::registry::lock_at(&path)?;
        let mut store = load()?;
        store.insert(account(server_id, key), value.to_string());
        save(&store)
    }

    pub fn get_secret_result(server_id: &str, key: &str) -> Result<Option<String>, String> {
        #[cfg(test)]
        crate::secrets::tests::record_vault_read(server_id, key);
        Ok(load()?.get(&account(server_id, key)).cloned())
    }

    pub fn delete_secret(server_id: &str, key: &str) -> Result<(), String> {
        let path = path()?;
        let _lock = crate::registry::lock_at(&path)?;
        let mut store = load()?;
        if store.remove(&account(server_id, key)).is_some() {
            save(&store)?;
        }
        Ok(())
    }
}

fn set_secret_raw(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    #[cfg(test)]
    if tests::write_is_injected_failure(key) {
        return Err("injected secret persistence failure".into());
    }
    // Before the write, so a refreshed OAuth token is covered even if the store fails.
    crate::secret_redact::register(server_id, key, value);
    write_stored(server_id, key, value)
}

/// A write with no registration, for Toolport's own internal keys.
fn write_stored(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    if write_would_prompt() {
        return Err("the keyring is locked; this background write waits for it".into());
    }
    // A write to a locked Secret Service collection asks to unlock it too.
    if locked_store::locked() || locked_store::collection_locked() {
        return Err(locked_store::prompted(server_id, key));
    }
    scrub_previous(server_id, key, Some(value))?;
    if file::active() {
        return file::set_secret(server_id, key, value);
    }
    platform::set_secret(server_id, key, value)
}

pub fn set_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    #[cfg(test)]
    if tests::write_is_injected_failure(key) {
        return Err("injected secret persistence failure".into());
    }
    if server_id == INTERNAL_SERVER_ID {
        return Err("reserved Toolport secret namespace".to_string());
    }
    set_secret_raw(&crate::local_auth::owner(server_id)?, key, value)
}

/// A client-config backup's vault write. Its key is new for each backup
/// (it holds the content hash), so there is no old value whose log cleanup
/// could be lost, and it is never registered: the text is a whole client
/// config, never sent to a server, and its ordinary lines would otherwise be
/// redacted from every log.
pub(crate) fn set_backup_secret(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    #[cfg(test)]
    if tests::write_is_injected_failure(key) {
        return Err("injected secret persistence failure".into());
    }
    if server_id == INTERNAL_SERVER_ID {
        return Err("reserved Toolport secret namespace".to_string());
    }
    if write_would_prompt() {
        return Err("the keyring is locked; this background write waits for it".into());
    }
    if locked_store::locked() || locked_store::collection_locked() {
        return Err(locked_store::prompted(server_id, key));
    }
    let owner = crate::local_auth::owner(server_id)?;
    if file::active() {
        return file::set_secret(&owner, key, value);
    }
    platform::set_secret(&owner, key, value)
}

pub fn get_secret(server_id: &str, key: &str) -> Option<String> {
    get_secret_result(server_id, key).ok().flatten()
}

/// Stable installation-local key used to seal client-facing Tasks handles.
/// Keeping the owner and native task id inside an authenticated ciphertext makes
/// the handle unguessable and tamper-evident while still surviving gateway and
/// app restarts. It is internal metadata, never a downstream server credential.
pub(crate) fn task_handle_key() -> Result<[u8; 32], String> {
    #[cfg(test)]
    {
        Ok([0x54; 32])
    }

    #[cfg(not(test))]
    {
        use base64::Engine as _;
        use std::sync::OnceLock;

        static KEY: OnceLock<[u8; 32]> = OnceLock::new();
        if let Some(key) = KEY.get() {
            return Ok(*key);
        }
        let loaded = (|| {
            let decode = |encoded: &str| -> Result<[u8; 32], String> {
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(encoded.trim())
                    .map_err(|_| "Toolport task-handle key is corrupt".to_string())?;
                raw.try_into()
                    .map_err(|_| "Toolport task-handle key has the wrong length".to_string())
            };
            let _first_use_lock = crate::registry::conduit_dir()
                .map(|dir| crate::registry::lock_at(&dir.join("task-handle-key")))
                .transpose()?;
            if let Some(encoded) = get_secret_result_raw(INTERNAL_SERVER_ID, TASK_HANDLE_KEY)? {
                return decode(&encoded);
            }

            let mut key = [0u8; 32];
            getrandom::getrandom(&mut key).map_err(|e| e.to_string())?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(key);
            set_secret_raw(INTERNAL_SERVER_ID, TASK_HANDLE_KEY, &encoded)?;
            // Read back through the active backend before caching the key.
            let stored = get_secret_result_raw(INTERNAL_SERVER_ID, TASK_HANDLE_KEY)?
                .ok_or_else(|| "Toolport task-handle key was not persisted".to_string())?;
            decode(&stored)
        })()?;
        let _ = KEY.set(loaded);
        Ok(*KEY.get().unwrap_or(&loaded))
    }
}

/// Like `get_secret`, but distinguishes "no such secret was saved" (`Ok(None)`)
/// from an actual keychain failure (`Err`, e.g. the keychain is locked or denied
/// access to this app). Callers that need to explain *why* a secret is missing
/// use this so a read failure isn't silently treated as "never saved".
///
/// Resolution order for headless / container deploys:
/// 1. Process env `TOOLPORT_SECRET_<KEY>` (legacy `CONDUIT_SECRET_<KEY>`; preferred
///    over bare names, avoids colliding with unrelated host env vars that share a
///    common name like `TOKEN`)
/// 2. Process env `<KEY>` when `TOOLPORT_ALLOW_BARE_SECRET_ENV=1` (legacy
///    `CONDUIT_ALLOW_BARE_SECRET_ENV=1`; opt-in)
/// 3. Encrypted `secrets.enc` when `TOOLPORT_SECRET_KEY` (legacy
///    `CONDUIT_SECRET_KEY`) is set
/// 4. OS keychain / platform backend
fn get_secret_result_raw(server_id: &str, key: &str) -> Result<Option<String>, String> {
    registered(server_id, key, read_stored(server_id, key))
}

/// A read with no registration, for Toolport's own internal keys.
fn read_stored(server_id: &str, key: &str) -> Result<Option<String>, String> {
    if let Some(v) = env_secret_override(key) {
        Ok(Some(v))
    } else if locked_store::locked() {
        Err(locked_store::prompted(server_id, key))
    } else if file::active() {
        file::get_secret_result(server_id, key)
    } else {
        platform::get_secret_result(server_id, key)
    }
}

/// Whether reading a stored value now could ask the user to unlock the store:
/// some Toolport item in it is locked. Asks nothing itself. Work nobody asked
/// for (a health check at start, a cleanup) waits while it is true; the first
/// real use of a value still asks. The encrypted file, the macOS
/// data-protection keychain and Windows never prompt.
pub fn store_would_prompt() -> bool {
    if locked_store::locked() || locked_store::collection_locked() {
        return true;
    }
    if file::active() {
        return false;
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        platform::store_would_prompt()
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        false
    }
}

/// Which servers (vault owners) keep a locked value in the store, without a
/// search or an unlock: a health check skips only those, so a server with
/// nothing stored is checked even while the default collection stays
/// locked. `None`: unknown; treat every server that may read the store as
/// locked. The encrypted file, macOS and Windows never ask: none.
pub fn owners_with_locked_values() -> Option<std::collections::HashSet<String>> {
    if locked_store::locked() {
        return None;
    }
    if locked_store::collection_locked() {
        return Some(locked_store::locked_owners());
    }
    if file::active() {
        return Some(Default::default());
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        platform::owners_with_locked_values()
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Some(Default::default())
    }
}

/// [`get_secret_result`] for work nobody asked for: never asks to unlock the
/// store, and a locked one is an error. Registers what it reads, the same way.
pub fn get_secret_without_prompt(server_id: &str, key: &str) -> Result<Option<String>, String> {
    if server_id == INTERNAL_SERVER_ID {
        return Err("reserved Toolport secret namespace".to_string());
    }
    let owner = crate::local_auth::owner(server_id)?;
    if let Some(value) = env_secret_override(key) {
        return registered(owner.as_str(), key, Ok(Some(value)));
    }
    registered(
        owner.as_str(),
        key,
        backup_secret_status(owner.as_str(), key).map_err(|error| error.to_string()),
    )
}

/// [`get_vault_secret_result`] for work nobody asked for, like
/// [`get_secret_without_prompt`].
pub fn get_vault_secret_without_prompt(
    server_id: &str,
    key: &str,
) -> Result<Option<String>, String> {
    if server_id == INTERNAL_SERVER_ID {
        return Err("reserved Toolport secret namespace".to_string());
    }
    let owner = crate::local_auth::owner(server_id)?;
    registered(
        owner.as_str(),
        key,
        backup_secret_status(owner.as_str(), key).map_err(|error| error.to_string()),
    )
}

/// Test support: a store that is locked, so every read that would ask the user
/// to unlock it is recorded (and fails) instead. Reads that never prompt see a
/// locked store. Process-wide: hold the data-directory test lock around it.
pub mod locked_store {
    #[cfg(any(test, feature = "test-support"))]
    static LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    #[cfg(any(test, feature = "test-support"))]
    static PROMPTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    #[cfg(any(test, feature = "test-support"))]
    static COLLECTION_LOCKED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    #[cfg(any(test, feature = "test-support"))]
    static LOCKED_OWNERS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// Run `f` with the store locked; every `server/key` read that would have
    /// asked to unlock it, in order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn prompts_during(f: impl FnOnce()) -> Vec<String> {
        use std::sync::atomic::Ordering;
        struct Unlock;
        impl Drop for Unlock {
            fn drop(&mut self) {
                LOCKED.store(false, Ordering::SeqCst);
            }
        }
        PROMPTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        LOCKED.store(true, Ordering::SeqCst);
        let unlock = Unlock;
        f();
        drop(unlock);
        std::mem::take(
            &mut *PROMPTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Run `f` with only the default collection locked and no Toolport item
    /// in it: reads find nothing to unlock, a new item's write would ask.
    /// Every write that would have asked, in order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn write_prompts_during(f: impl FnOnce()) -> Vec<String> {
        use std::sync::atomic::Ordering;
        struct Unlock;
        impl Drop for Unlock {
            fn drop(&mut self) {
                COLLECTION_LOCKED.store(false, Ordering::SeqCst);
            }
        }
        PROMPTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        COLLECTION_LOCKED.store(true, Ordering::SeqCst);
        let unlock = Unlock;
        f();
        drop(unlock);
        std::mem::take(
            &mut *PROMPTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// [`write_prompts_during`] with these owners' values in the store,
    /// locked.
    #[cfg(any(test, feature = "test-support"))]
    pub fn locked_values_during(owners: &[&str], f: impl FnOnce()) -> Vec<String> {
        *LOCKED_OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            owners.iter().map(|owner| owner.to_string()).collect();
        let prompts = write_prompts_during(f);
        LOCKED_OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        prompts
    }

    pub(crate) fn locked_owners() -> std::collections::HashSet<String> {
        #[cfg(any(test, feature = "test-support"))]
        {
            LOCKED_OWNERS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .cloned()
                .collect()
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            Default::default()
        }
    }

    pub(crate) fn collection_locked() -> bool {
        #[cfg(any(test, feature = "test-support"))]
        {
            COLLECTION_LOCKED.load(std::sync::atomic::Ordering::SeqCst)
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            false
        }
    }

    pub(crate) fn locked() -> bool {
        #[cfg(any(test, feature = "test-support"))]
        {
            LOCKED.load(std::sync::atomic::Ordering::SeqCst)
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            false
        }
    }

    /// Record a read that would have prompted; its error.
    pub(crate) fn prompted(server_id: &str, key: &str) -> String {
        super::note_refused_ask();
        #[cfg(any(test, feature = "test-support"))]
        PROMPTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("{server_id}/{key}"));
        let _ = (server_id, key);
        "keyring locked".to_string()
    }
}

/// Every value this process reads or stores is registered for redaction at the log
/// writers ([`crate::secret_redact`]), so no echo of it reaches disk.
fn registered(
    server_id: &str,
    key: &str,
    value: Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    if let Ok(Some(v)) = &value {
        crate::secret_redact::register(server_id, key, v);
    }
    value
}

/// Inspect only the managed identity's vault, never environment overrides or aliases.
pub(crate) fn has_own_credentials(server: &crate::registry::ServerEntry) -> Result<bool, String> {
    let mut keys: Vec<&str> = server.env.iter().map(|e| e.key.as_str()).collect();
    if let Some(launch) = &server.launch {
        keys.extend(
            launch
                .inputs
                .iter()
                .filter(|i| i.secret)
                .map(|i| i.key.as_str()),
        );
    }
    if crate::import_credentials::has_imported_url(server) {
        keys.push(IMPORTED_URL_KEY);
    }
    keys.extend([
        HTTP_AUTH_KEY,
        CLIENT_SECRET_KEY,
        "__oauth_state__",
        "__oauth_cc_state__",
    ]);
    for key in keys {
        let value = if locked_store::locked() {
            Err(locked_store::prompted(&server.id, key))
        } else if file::active() {
            file::get_secret_result(&server.id, key)
        } else {
            platform::get_secret_result(&server.id, key)
        }?;
        if value.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

thread_local! {
    static WITHOUT_PROMPTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Run work nobody asked for (background sync, a restore at start) so that no
/// keyring call in it, on this thread, can ask to unlock: reads use the
/// non-prompting path, and a write or delete fails while the store would
/// prompt. A check made before the work cannot cover a keyring that locks
/// during it; this does.
pub fn without_prompts<T>(f: impl FnOnce() -> T) -> T {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            WITHOUT_PROMPTS.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    WITHOUT_PROMPTS.with(|depth| depth.set(depth.get() + 1));
    let _leave = Leave;
    f()
}

/// Undo any enclosing [`without_prompts`] for `f`: the step it runs is one the
/// user asked for and may prompt as usual.
pub fn with_prompts<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(u32);
    impl Drop for Restore {
        fn drop(&mut self) {
            WITHOUT_PROMPTS.with(|depth| depth.set(self.0));
        }
    }
    let _restore = Restore(WITHOUT_PROMPTS.with(|depth| depth.replace(0)));
    f()
}

fn prompts_allowed() -> bool {
    WITHOUT_PROMPTS.with(|depth| depth.get() == 0) && !ask_spent()
}

thread_local! {
    static REFUSED_ASKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static ONE_ASK_FROM: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// A call on this thread asked to unlock the store and was refused: the store
/// was locked and its unlock dialog dismissed (or, in the test store, recorded).
fn note_refused_ask() {
    REFUSED_ASKS.with(|asks| asks.set(asks.get().saturating_add(1)));
}

/// Run `f` so that it asks to unlock the store at most once on this thread: after
/// one refused ask, the rest of `f` runs as in [`without_prompts`]. Returns whether
/// an ask in `f` was refused.
pub fn asking_once<T>(f: impl FnOnce() -> T) -> (T, bool) {
    struct Restore(Option<u32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ONE_ASK_FROM.with(|from| from.set(self.0));
        }
    }
    let before = REFUSED_ASKS.with(std::cell::Cell::get);
    let _restore = Restore(ONE_ASK_FROM.with(|from| from.replace(Some(before))));
    let result = f();
    (result, REFUSED_ASKS.with(std::cell::Cell::get) > before)
}

fn ask_spent() -> bool {
    ONE_ASK_FROM
        .with(std::cell::Cell::get)
        .is_some_and(|from| REFUSED_ASKS.with(std::cell::Cell::get) > from)
}

/// Inside [`without_prompts`] on this thread, so work it hands to another thread can
/// carry it there.
pub(crate) fn prompts_blocked() -> bool {
    !prompts_allowed()
}

/// Inside [`without_prompts`], on a store whose ordinary reads can ask to
/// unlock (Secret Service). macOS, Windows and the file vault never ask, so
/// their reads keep their usual path.
fn read_must_not_prompt() -> bool {
    !prompts_allowed()
        && (locked_store::locked()
            || (cfg!(all(not(target_os = "macos"), not(target_os = "windows"))) && !file::active()))
}

/// The background check: `default_locked` reads the default collection's
/// `Locked` property (`None`: there is none), which never asks; `search`
/// looks for locked Toolport items, which KeePassXC answers with its unlock
/// dialog while its database is locked. So a locked default collection is
/// the answer without a search, and the polls that repeat this check every
/// few seconds never raise that dialog. A write also asks with no default
/// collection to put a new item in.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn would_prompt_by<E>(
    write: bool,
    default_locked: impl FnOnce() -> Result<Option<bool>, E>,
    search: impl FnOnce() -> Result<bool, E>,
) -> Result<bool, E> {
    match default_locked()? {
        Some(true) => Ok(true),
        None if write => Ok(true),
        _ => search(),
    }
}

/// [`owners_with_locked_values`]'s plan: `default_locked` is a property read;
/// open, `search` returns the accounts of the locked Toolport items it finds
/// (in other collections), one call however large the store; locked or
/// missing (no search: KeePassXC opens its dialog for one), `walk` reads the
/// accounts in the locked collections. An account is `owner::key`.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn owners_with_locked_by<E>(
    default_locked: impl FnOnce() -> Result<Option<bool>, E>,
    search: impl FnOnce() -> Result<Vec<Option<String>>, E>,
    walk: impl FnOnce() -> Result<Vec<Option<String>>, E>,
) -> Result<std::collections::HashSet<String>, E> {
    let accounts = match default_locked()? {
        Some(false) => search()?,
        Some(true) | None => walk()?,
    };
    Ok(accounts
        .into_iter()
        .flatten()
        .filter_map(|account| account.split_once("::").map(|(owner, _)| owner.to_string()))
        .collect())
}

/// The accounts of the items in the locked `collections`: every item of a
/// locked collection is locked, and an open one is skipped without listing
/// its items.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn accounts_in_locked<C, E>(
    collections: &[C],
    locked: impl Fn(&C) -> Result<bool, E>,
    accounts: impl Fn(&C) -> Result<Vec<Option<String>>, E>,
) -> Result<Vec<Option<String>>, E> {
    let mut found = Vec::new();
    for collection in collections {
        if locked(collection)? {
            found.extend(accounts(collection)?);
        }
    }
    Ok(found)
}

/// A write or delete inside [`without_prompts`] while the store would ask.
fn write_would_prompt() -> bool {
    !prompts_allowed() && write_would_ask()
}

/// Whether a write now could ask to unlock the store: [`store_would_prompt`],
/// or a locked default collection a new item would go into.
pub(crate) fn write_would_ask() -> bool {
    if locked_store::locked() || locked_store::collection_locked() {
        return true;
    }
    if file::active() {
        return false;
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        platform::write_would_prompt()
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        false
    }
}

pub fn get_secret_result(server_id: &str, key: &str) -> Result<Option<String>, String> {
    #[cfg(test)]
    tests::record_read(server_id, key);
    #[cfg(test)]
    if tests::read_is_injected_failure(key) {
        return Err("injected secret read failure".into());
    }
    if server_id == INTERNAL_SERVER_ID {
        return Err("reserved Toolport secret namespace".to_string());
    }
    if read_must_not_prompt() {
        return get_secret_without_prompt(server_id, key);
    }
    get_secret_result_raw(&crate::local_auth::owner(server_id)?, key)
}

/// Launch argument bindings read only Toolport's vault. Environment overrides
/// remain available for legacy env keys, but must never construct an argv value.
pub fn get_vault_secret_result(server_id: &str, key: &str) -> Result<Option<String>, String> {
    if server_id == INTERNAL_SERVER_ID {
        return Err("reserved Toolport secret namespace".to_string());
    }
    if read_must_not_prompt() {
        return get_vault_secret_without_prompt(server_id, key);
    }
    let owner = crate::local_auth::owner(server_id)?;
    let server_id = owner.as_str();
    let value = if locked_store::locked() {
        Err(locked_store::prompted(server_id, key))
    } else if file::active() {
        file::get_secret_result(server_id, key)
    } else {
        platform::get_secret_result(server_id, key)
    };
    registered(server_id, key, value)
}

/// Why a stored value could not be read. [`scrub_previous`] needs the kind:
/// a damaged value can never be read back, so there is nothing to scrub or
/// keep, while a locked or unavailable store may give it up later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadFailure {
    /// The stored data is damaged or incomplete.
    Damaged(String),
    /// The store is locked or unavailable right now.
    Unavailable(String),
}

impl ReadFailure {
    pub(crate) fn into_message(self) -> String {
        match self {
            Self::Damaged(message) | Self::Unavailable(message) => message,
        }
    }
}

/// Stable recovery categories. No backend error text or secret bytes cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum BackupSecretError {
    Locked,
    Missing,
    Corrupt,
    Ambiguous,
    Unavailable,
    AuthenticationFailed,
    Cancelled,
    AccessDenied,
    Unchecked,
}

/// A recovery/cleanup batch never keeps a credential cache after it returns.
/// Linux shares a noninteractive DH session; other native APIs do not prompt.
pub(crate) fn with_backup_batch<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        platform::with_backup_batch(f)
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        f()
    }
}
impl std::fmt::Display for BackupSecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Saved credential recovery: {self:?}")
    }
}
impl std::error::Error for BackupSecretError {}
impl BackupSecretError {
    #[cfg(any(target_os = "macos", test))]
    fn macos(code: i32) -> Self {
        match code {
            -25308 => Self::Locked,
            -25293 => Self::AuthenticationFailed,
            -128 => Self::Cancelled,
            -25300 => Self::Missing,
            -34018 => Self::AccessDenied,
            _ => Self::Unavailable,
        }
    }
    #[cfg(any(target_os = "windows", test))]
    pub(super) fn windows(code: u32) -> Self {
        match code {
            5 => Self::AccessDenied,
            1223 => Self::Cancelled,
            1326 => Self::AuthenticationFailed,
            1168 => Self::Missing,
            13 => Self::Corrupt,
            _ => Self::Unavailable,
        }
    }
}
pub(crate) fn backup_secret_status(
    server_id: &str,
    key: &str,
) -> Result<Option<String>, BackupSecretError> {
    if server_id == INTERNAL_SERVER_ID {
        return Err(BackupSecretError::AccessDenied);
    }
    let owner = crate::local_auth::owner(server_id).map_err(|_| BackupSecretError::AccessDenied)?;
    // These reads never ask to unlock the store: a locked one says so.
    if locked_store::locked() {
        return Err(BackupSecretError::Locked);
    }
    if file::active() {
        return file::get_secret_result(owner.as_str(), key)
            .map_err(|_| BackupSecretError::Unavailable);
    }
    platform::get_backup_secret_status(owner.as_str(), key)
}

/// Vault-only backup access, with no interactive unlock during Linux recovery.
pub(crate) fn get_backup_secret_result(
    server_id: &str,
    key: &str,
) -> Result<Option<String>, String> {
    backup_secret_status(server_id, key).map_err(|error| error.to_string())
}

/// Backup-only deletion follows the same noninteractive policy as recovery reads.
#[cfg(test)]
pub(crate) fn delete_backup_secret_status(
    server_id: &str,
    key: &str,
) -> Result<(), BackupSecretError> {
    delete_backup_secret_copies_status(server_id, key, false)
}
pub(crate) fn delete_backup_secret_copies_status(
    server_id: &str,
    key: &str,
    discard_all: bool,
) -> Result<(), BackupSecretError> {
    #[cfg(test)]
    if tests::write_is_injected_failure(key) {
        return Err(BackupSecretError::Unavailable);
    }
    if server_id == INTERNAL_SERVER_ID {
        return Err(BackupSecretError::AccessDenied);
    }
    let owner = crate::local_auth::owner(server_id).map_err(|_| BackupSecretError::AccessDenied)?;
    if file::active() {
        return file::delete_secret(owner.as_str(), key)
            .map_err(|_| BackupSecretError::Unavailable);
    }
    #[cfg(target_os = "linux")]
    {
        platform::delete_backup_secret_status(owner.as_str(), key, discard_all)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = discard_all;
        platform::delete_backup_secret_status(owner.as_str(), key)
    }
}
#[cfg(test)]
pub(crate) fn delete_backup_secret(server_id: &str, key: &str) -> Result<(), String> {
    delete_backup_secret_status(server_id, key).map_err(|e| e.to_string())
}

/// Look up a secret from the process environment for container / env-file deploys.
/// Prefers `TOOLPORT_SECRET_<KEY>` (legacy `CONDUIT_SECRET_<KEY>`); falls back to the
/// bare key name only when `TOOLPORT_ALLOW_BARE_SECRET_ENV` (legacy
/// `CONDUIT_ALLOW_BARE_SECRET_ENV`) is set. Empty values are treated as unset.
fn env_secret_override(key: &str) -> Option<String> {
    let toolport_prefixed = format!("TOOLPORT_SECRET_{key}");
    let conduit_prefixed = format!("CONDUIT_SECRET_{key}");
    env_var_nonblank(&toolport_prefixed)
        .or_else(|| env_var_nonblank(&conduit_prefixed))
        .or_else(|| {
            if bare_secret_env_enabled() {
                env_var_nonblank(key)
            } else {
                None
            }
        })
}

fn bare_secret_env_enabled() -> bool {
    crate::brand::env_flag(
        "TOOLPORT_ALLOW_BARE_SECRET_ENV",
        "CONDUIT_ALLOW_BARE_SECRET_ENV",
    )
}

fn env_var_nonblank(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Before a stored value is replaced or deleted, scrub the logs for it: once it
/// is gone from the vault nothing could find a copy an older build (or another
/// tool) left on disk. Reads the stored value directly, never an environment
/// override, and skips Toolport's own internal keys. An error when a log may
/// still hold the old value and it could not be kept for a later cleanup: the
/// caller then changes nothing, so the stored value is not lost.
fn scrub_previous(server_id: &str, key: &str, replacement: Option<&str>) -> Result<(), String> {
    if server_id == INTERNAL_SERVER_ID {
        return Ok(());
    }
    // The whole file vault is loaded for any write too, so its read errors
    // always stop the change.
    let stored = if file::active() {
        file::get_secret_result(server_id, key).map_err(ReadFailure::Unavailable)
    } else {
        platform::read_secret(server_id, key)
    };
    #[cfg(test)]
    let stored = if tests::read_is_injected_failure(key) {
        Err(ReadFailure::Unavailable(
            "injected secret read failure".to_string(),
        ))
    } else if tests::read_is_injected_damage(key) {
        Err(ReadFailure::Damaged("injected damaged secret".to_string()))
    } else {
        stored
    };
    let stored = match stored {
        Ok(stored) => stored,
        // Damaged data can never be read back: there is no old value to
        // scrub or keep, and refusing would leave it unfixable.
        Err(ReadFailure::Damaged(error)) => {
            eprintln!(
                "toolport: the old value could not be read ({error}), so its log cleanup was skipped"
            );
            None
        }
        // A locked or unavailable store may give the value up later: change
        // nothing, so the chance to scrub it is not lost.
        Err(ReadFailure::Unavailable(error)) => {
            eprintln!("toolport: the stored value could not be read for its log cleanup ({error})");
            return Err(crate::i18n::tr(
                "The old value could not be kept for its log cleanup, so nothing was changed. Try again.",
            ));
        }
    };
    if let Some(old) = stored {
        if Some(old.as_str()) != replacement {
            if let Err(error) = crate::secret_redact::scrub_value_now(server_id, key, &old) {
                // A log may still hold it: keep the value in the vault, under
                // Toolport's own namespace, until a later scrub succeeds.
                eprintln!("toolport: {error}; keeping the old value for a later cleanup");
                retire(server_id, key, &old)?;
            }
        }
    }
    Ok(())
}

/// Values replaced or deleted while a log may still hold them, as a JSON list of
/// `{server, key, value}` in the vault (never on disk in the clear).
const RETIRED_KEY: &str = "retired-secret-values";

fn retired_load() -> Vec<(String, String, String)> {
    // Unregistered: the values in it are registered on their own, and the
    // list changes on every save.
    let Ok(Some(raw)) = read_stored(INTERNAL_SERVER_ID, RETIRED_KEY) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<serde_json::Value>>(&raw)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            Some((
                v.get("server")?.as_str()?.to_string(),
                v.get("key")?.as_str()?.to_string(),
                v.get("value")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

fn retired_save(entries: &[(String, String, String)]) -> Result<(), String> {
    crate::secret_redact::RETIRED_PENDING
        .store(entries.len(), std::sync::atomic::Ordering::Relaxed);
    if entries.is_empty() {
        return match read_stored(INTERNAL_SERVER_ID, RETIRED_KEY) {
            Ok(Some(_)) => delete_secret_raw(INTERNAL_SERVER_ID, RETIRED_KEY),
            _ => Ok(()),
        };
    }
    let list: Vec<serde_json::Value> = entries
        .iter()
        .map(|(server, key, value)| serde_json::json!({"server": server, "key": key, "value": value}))
        .collect();
    #[cfg(test)]
    if tests::write_is_injected_failure(RETIRED_KEY) {
        return Err("injected secret persistence failure".into());
    }
    write_stored(
        INTERNAL_SERVER_ID,
        RETIRED_KEY,
        &serde_json::Value::Array(list).to_string(),
    )
}

/// The cross-process lock every read-modify-write of the retired list holds,
/// so two processes never save over each other's entries.
fn retired_lock() -> Result<crate::registry::FileLock, String> {
    let dir = crate::registry::conduit_dir().ok_or("no data directory")?;
    crate::registry::lock_at(&dir.join(RETIRED_KEY))
}

fn retire(server_id: &str, key: &str, value: &str) -> Result<(), String> {
    let saved = retired_lock().and_then(|_lock| {
        let mut entries = retired_load();
        let entry = (server_id.to_string(), key.to_string(), value.to_string());
        if !entries.contains(&entry) {
            entries.push(entry);
        }
        retired_save(&entries)
    });
    saved.map_err(|error| {
        eprintln!("toolport: could not keep a retired secret for cleanup: {error}");
        crate::i18n::tr(
            "The old value could not be kept for its log cleanup, so nothing was changed. Try again.",
        )
    })
}

/// Register every retired value and retry its cleanup; entries whose scrub now
/// succeeds are dropped from the vault. Returns how many still wait. Called at
/// gateway start, so a value is redacted again before anything is written.
pub fn retry_retired_scrubs() -> usize {
    // The list lives in the store: reading it while locked would ask the user
    // to unlock it at start. It waits for the next start that finds it open.
    if store_would_prompt() {
        return crate::secret_redact::RETIRED_PENDING.load(std::sync::atomic::Ordering::Relaxed);
    }
    let entries = retired_load();
    if entries.is_empty() {
        crate::secret_redact::RETIRED_PENDING.store(0, std::sync::atomic::Ordering::Relaxed);
        return 0;
    }
    // The scrubs run unlocked (each may wait for a busy scrub); then, under
    // the lock, only the entries they cleaned are removed from the list as it
    // is now, so a value another process retired meanwhile is kept.
    let cleaned: Vec<(String, String, String)> = entries
        .into_iter()
        .filter(|(server, key, value)| {
            crate::secret_redact::scrub_value_now(server, key, value).is_ok()
        })
        .collect();
    #[cfg(test)]
    tests::run_retry_hook();
    let _lock = match retired_lock() {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("toolport: could not update retired secrets: {error}");
            return retired_load().len();
        }
    };
    let remaining: Vec<(String, String, String)> = retired_load()
        .into_iter()
        .filter(|entry| !cleaned.contains(entry))
        .collect();
    let left = remaining.len();
    if let Err(error) = retired_save(&remaining) {
        eprintln!("toolport: could not update retired secrets: {error}");
    }
    left
}

/// How many retired values still wait for their log cleanup (for status).
pub fn retired_scrubs_pending() -> usize {
    retired_load().len()
}

fn delete_secret_raw(server_id: &str, key: &str) -> Result<(), String> {
    if write_would_prompt() {
        return Err("the keyring is locked; this background delete waits for it".into());
    }
    if locked_store::locked() {
        return Err(locked_store::prompted(server_id, key));
    }
    scrub_previous(server_id, key, None)?;
    if file::active() {
        return file::delete_secret(server_id, key);
    }
    platform::delete_secret(server_id, key)
}

pub fn delete_secret(server_id: &str, key: &str) -> Result<(), String> {
    if server_id == INTERNAL_SERVER_ID {
        return Err("reserved Toolport secret namespace".to_string());
    }
    delete_secret_raw(&crate::local_auth::owner(server_id)?, key)
}

// ── Legacy keychain migration (macOS) ──────────────────────────────────────
//
// Older versions of Toolport used the `keyring` crate, which created keychain
// items with per-application ACLs. This migration reads each entry's value,
// deletes it, and re-creates it via the ACL-free `SecItemAdd` path.
//
// The migration is guarded by a marker file so it runs once per marker version.
// Only the UI app runs it — the gateway can't rewrite entries without triggering
// prompts (it's a separately signed process). Bumping the marker name
// (".keychain-migrated" -> ".keychain-acl-migrated") makes the migration re-run
// once on upgrade so EXISTING secrets are rewritten WITH the shared-access ACL,
// not just legacy keyring-API entries.

/// Marker file name for the keychain -> encrypted-file migration. A NEW name so
/// the migration runs exactly once on upgrade to the file-backend-by-default
/// build, even on installs that already ran the older ACL migration. An older
/// `.keychain-acl-migrated` marker file may exist on disk from that legacy
/// migration; it is intentionally ignored.
#[cfg(target_os = "macos")]
const FILE_MIGRATION_MARKER: &str = ".secrets-file-migrated";

/// Marker file name for the legacy-keychain -> data-protection-keychain
/// migration (the team-scoped shared access group). A NEW name (the older
/// markers are left untouched) so this runs exactly once on upgrade to the
/// data-protection-keychain build. App-only; written only on a clean pass.
#[cfg(target_os = "macos")]
const DPK_MIGRATION_MARKER: &str = ".keychain-dpk-migrated";

/// Result of the one-time keychain migration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MigrationReport {
    /// Entries whose values were read and re-created via the ACL-free path.
    pub migrated: usize,
    /// Entries that existed but couldn't be read or re-created (locked
    /// keychain, denied access, non-UTF-8 value). The user must
    /// re-authenticate these servers.
    pub failed: usize,
    /// Keys that had no keychain entry — expected for reserved keys like
    /// `__oauth_state__` on non-OAuth servers. Not an error.
    pub not_found: usize,
}

/// Run the one-time macOS secret migration to the encrypted file backend.
///
/// On macOS this:
/// 1. **Ensures the master key** (`platform::ensure_master_key`) FIRST, so the
///    file backend becomes active (`file::active()` is true). If ensuring the
///    key fails (e.g. the keychain is locked), it logs and returns the default
///    report WITHOUT writing the marker, so the whole thing retries next launch.
/// 2. If the file-migration marker is absent, **moves each secret** from its old
///    per-server keychain item into `secrets.enc` via
///    `platform::migrate_keychain_to_file` (read -> write-file -> verify -> only
///    then delete the keychain item; on any failure the keychain item is left in
///    place so nothing is lost). The marker is written ONLY when that pass
///    returns `Ok`.
///
/// Only call from the UI app (not the gateway): the gateway is read-only and
/// must never create the master key or rewrite entries.
///
/// `secret_keys` is a list of `(server_id, key)` pairs for every secret env var
/// in the registry (and reserved keys like `HTTP_AUTH_KEY` for remote servers).
pub fn migrate_legacy_entries(secret_keys: &[(String, String)]) -> MigrationReport {
    #[cfg(target_os = "macos")]
    {
        // 1. Ensure the master key exists before anything else. This is what
        //    flips the file backend on for this install. If it fails (locked
        //    keychain), don't write the marker — retry on the next launch.
        if let Err(e) = platform::ensure_master_key() {
            eprintln!(
                "toolport: could not ensure secrets master key, will retry next launch ({e})"
            );
            return MigrationReport::default();
        }

        // 2. One-time migration of per-server secrets into the file backend.
        if file_migration_marker_exists() {
            return MigrationReport::default();
        }

        let report = match platform::migrate_keychain_to_file(secret_keys) {
            Ok(report) => report,
            Err(e) => {
                // Don't write the marker — the migration didn't run, so it
                // should retry on the next launch.
                eprintln!("toolport: secret migration skipped, will retry next launch ({e})");
                return MigrationReport::default();
            }
        };

        let _ = create_file_migration_marker();
        report
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = secret_keys;
        MigrationReport::default()
    }
}

/// Whether the keychain -> file migration marker exists in the Toolport data dir.
#[cfg(target_os = "macos")]
fn file_migration_marker_exists() -> bool {
    crate::registry::conduit_dir()
        .map(|d| d.join(FILE_MIGRATION_MARKER))
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Create the keychain -> file migration marker file. Returns `true` on success.
#[cfg(target_os = "macos")]
fn create_file_migration_marker() -> bool {
    let Some(dir) = crate::registry::conduit_dir() else {
        return false;
    };
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join(FILE_MIGRATION_MARKER), b"1").is_ok()
}

/// Run the one-time macOS migration of per-server secrets from the legacy
/// file-based keychain INTO the data-protection keychain (the team-scoped shared
/// access group), guarded by `DPK_MIGRATION_MARKER` so it runs once per install.
///
/// This is the migration that replaces the old ACL / encrypted-file paths: after
/// it runs, every secret lives in the data-protection keychain, which the
/// separately-signed gateway reads with NO prompt across app updates.
///
/// Only the UI app calls this (the gateway is read-only and must never rewrite
/// entries). Best-effort and lossless: `migrate_legacy_to_dpk` reads each legacy
/// item, writes + verifies the data-protection copy, then deletes the legacy item;
/// an item it can't move is left in the legacy keychain. The marker is written
/// only on a fully clean pass (`failed == 0`), so a transient denial or locked
/// keychain is retried on the next launch rather than stranding a secret.
pub fn migrate_secrets_to_dpk(secret_keys: &[(String, String)]) -> MigrationReport {
    #[cfg(target_os = "macos")]
    {
        if dpk_migration_marker_exists() {
            return MigrationReport::default();
        }
        let report = match platform::migrate_legacy_to_dpk(secret_keys) {
            Ok(report) => report,
            Err(e) => {
                eprintln!(
                    "toolport: data-protection keychain migration skipped, will retry next launch ({e})"
                );
                return MigrationReport::default();
            }
        };
        // Only mark complete once nothing failed, so a denied prompt or locked
        // keychain gets another chance next launch instead of being marked done.
        if report.failed == 0 {
            let _ = create_dpk_migration_marker();
        }
        report
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = secret_keys;
        MigrationReport::default()
    }
}

/// Whether the legacy -> data-protection keychain migration marker exists.
#[cfg(target_os = "macos")]
fn dpk_migration_marker_exists() -> bool {
    crate::registry::conduit_dir()
        .map(|d| d.join(DPK_MIGRATION_MARKER))
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Create the legacy -> data-protection keychain migration marker. Returns `true`
/// on success.
#[cfg(target_os = "macos")]
fn create_dpk_migration_marker() -> bool {
    let Some(dir) = crate::registry::conduit_dir() else {
        return false;
    };
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join(DPK_MIGRATION_MARKER), b"1").is_ok()
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn keychain_publication_failure_keeps_app_only_acl() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir =
            std::env::temp_dir().join(format!("toolport-acl-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);

        assert_eq!(
            super::keychain_gateway_path(Err("manifest failed".into())),
            None
        );
        let path = std::path::PathBuf::from("gateway");
        assert_eq!(
            super::keychain_gateway_path(Ok(Some(path.clone()))),
            Some(path)
        );
        assert!(crate::telemetry::flush_for_test(
            std::time::Duration::from_secs(5)
        ));
        assert!(std::fs::read_to_string(dir.join("gateway.log"))
            .unwrap()
            .contains("publication failed"));
        drop(_data);
        assert!(crate::telemetry::retire_dir_for_test(
            &dir,
            std::time::Duration::from_secs(5)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that read or mutate the process-global `CONDUIT_SECRET_KEY`
    /// env var (and thus `file::active()`). Without this, a test that sets the var
    /// races with a sibling that assumes the keychain path, causing spurious
    /// failures under the default multi-threaded test runner.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        static FAILED_READ: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
        static FAILED_WRITE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    }

    thread_local! {
        static READS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    thread_local! {
        static VAULT_READS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    pub(crate) fn record_vault_read(server_id: &str, key: &str) {
        VAULT_READS.with(|reads| reads.borrow_mut().push(format!("{server_id}/{key}")));
    }

    /// The `server/key` reads of every secret store backend (keychain or file vault)
    /// on this thread while `test` runs, whichever entry point made them.
    pub(crate) fn vault_reads_during<T>(test: impl FnOnce() -> T) -> (T, Vec<String>) {
        VAULT_READS.with(|reads| reads.borrow_mut().clear());
        let value = test();
        (
            value,
            VAULT_READS.with(|reads| std::mem::take(&mut *reads.borrow_mut())),
        )
    }

    pub(crate) fn record_read(server_id: &str, key: &str) {
        READS.with(|reads| reads.borrow_mut().push(format!("{server_id}/{key}")));
    }

    /// The `server/key` reads of [`super::get_secret_result`] on this thread
    /// while `test` runs.
    pub(crate) fn reads_during(test: impl FnOnce()) -> Vec<String> {
        READS.with(|reads| reads.borrow_mut().clear());
        test();
        READS.with(|reads| std::mem::take(&mut *reads.borrow_mut()))
    }

    thread_local! {
        static DAMAGED_READ: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    }

    pub(crate) fn read_is_injected_damage(key: &str) -> bool {
        DAMAGED_READ.with(|damaged| damaged.get() == Some(key))
    }

    /// Reads of `key`'s stored value find it damaged (a broken chunk manifest,
    /// an undecodable item) while `test` runs: nothing can ever read it back.
    pub(crate) fn with_damaged_read<T>(key: &'static str, test: impl FnOnce() -> T) -> T {
        struct Restore(Option<&'static str>);
        impl Drop for Restore {
            fn drop(&mut self) {
                DAMAGED_READ.with(|damaged| damaged.set(self.0));
            }
        }
        let _restore = Restore(DAMAGED_READ.with(|damaged| damaged.replace(Some(key))));
        test()
    }

    /// `"*"` fails every key.
    pub(crate) fn read_is_injected_failure(key: &str) -> bool {
        FAILED_READ.with(|failed| matches!(failed.get(), Some(k) if k == key || k == "*"))
    }

    pub(crate) fn with_failed_read<T>(key: &'static str, test: impl FnOnce() -> T) -> T {
        struct Restore(Option<&'static str>);
        impl Drop for Restore {
            fn drop(&mut self) {
                FAILED_READ.with(|failed| failed.set(self.0));
            }
        }
        let _restore = Restore(FAILED_READ.with(|failed| failed.replace(Some(key))));
        test()
    }

    thread_local! {
        static RETRY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Run `hook` inside the next [`super::retry_retired_scrubs`] on this
    /// thread, after its scrubs and before it updates the retired list.
    pub(crate) fn with_retry_hook<T>(hook: impl FnOnce() + 'static, test: impl FnOnce() -> T) -> T {
        RETRY_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        let out = test();
        RETRY_HOOK.with(|h| h.borrow_mut().take());
        out
    }

    pub(crate) fn run_retry_hook() {
        if let Some(hook) = RETRY_HOOK.with(|h| h.borrow_mut().take()) {
            hook();
        }
    }

    pub(crate) fn write_is_injected_failure(key: &str) -> bool {
        FAILED_WRITE.with(|failed| failed.get() == Some(key))
    }

    pub(crate) fn with_failed_write<T>(key: &'static str, test: impl FnOnce() -> T) -> T {
        struct Restore(Option<&'static str>);
        impl Drop for Restore {
            fn drop(&mut self) {
                FAILED_WRITE.with(|failed| failed.set(self.0));
            }
        }
        let _restore = Restore(FAILED_WRITE.with(|failed| failed.replace(Some(key))));
        test()
    }

    pub(crate) fn with_isolated_vault(test: impl FnOnce()) {
        let _data = crate::registry::data_dir_test_lock();
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "toolport-publisher-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _override = crate::registry::DataDirOverride::set(&dir);
        struct Cleanup(std::path::PathBuf, Option<std::ffi::OsString>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                match &self.1 {
                    Some(value) => std::env::set_var("TOOLPORT_SECRET_KEY", value),
                    None => std::env::remove_var("TOOLPORT_SECRET_KEY"),
                }
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir, std::env::var_os("TOOLPORT_SECRET_KEY"));
        std::env::set_var("TOOLPORT_SECRET_KEY", "publisher-synthetic-vault");
        test();
    }

    /// Failure mode: the owner check listed every item of every collection on
    /// each probe round, which a large keyring could stretch past its 2 s
    /// deadline, and then every server showed "Keyring locked" though the
    /// keyring was open.
    #[test]
    fn an_open_default_collection_is_one_search_and_a_locked_one_walks_locked_collections() {
        let (searches, walks) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let owners = owners_with_locked_by(
            || Ok::<_, ()>(Some(false)),
            || {
                searches.set(searches.get() + 1);
                Ok(vec![Some("other-collection::API_KEY".into())])
            },
            || {
                walks.set(walks.get() + 1);
                Ok(Vec::new())
            },
        )
        .unwrap();
        assert_eq!((searches.get(), walks.get()), (1, 0));
        assert_eq!(owners, ["other-collection".to_string()].into());

        // Locked: no search; only the locked collections are listed.
        let listed = std::cell::RefCell::new(Vec::new());
        let collections = [("login", true), ("open", false), ("second", true)];
        let owners = owners_with_locked_by(
            || Ok::<_, ()>(Some(true)),
            || {
                searches.set(searches.get() + 1);
                Ok(Vec::new())
            },
            || {
                accounts_in_locked(
                    &collections,
                    |(_, locked)| Ok(*locked),
                    |(name, _)| {
                        listed.borrow_mut().push(*name);
                        Ok(vec![Some(format!("{name}-server::KEY")), None])
                    },
                )
            },
        )
        .unwrap();
        assert_eq!(searches.get(), 1, "searched while the default was locked");
        assert_eq!(*listed.borrow(), ["login", "second"]);
        assert_eq!(
            owners,
            ["login-server".to_string(), "second-server".to_string()].into()
        );
    }

    /// Times one owner check against the session's real keyring. Run in a
    /// box with a filled keyring:
    /// `cargo test ... -- --ignored --exact secrets::tests::time_owner_check --nocapture`
    #[test]
    #[ignore = "times the session keyring; run in omabox"]
    fn time_owner_check() {
        let _data = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("toolport-owner-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _override = crate::registry::DataDirOverride::set(&dir);
        std::env::remove_var("TOOLPORT_SECRET_KEY");
        let started = std::time::Instant::now();
        let owners = owners_with_locked_values();
        println!(
            "owner check: {:?} in {} ms",
            owners.map(|owners| owners.len()),
            started.elapsed().as_millis()
        );
    }

    /// Failure mode: every check searched the store, and KeePassXC answers a
    /// search with its unlock dialog while its database is locked, so the 3 s
    /// polls raised it again after every cancel.
    #[test]
    fn polls_never_search_while_the_default_collection_is_locked() {
        let searches = std::cell::Cell::new(0);
        let search = || {
            searches.set(searches.get() + 1);
            Ok::<_, ()>(false)
        };
        for _ in 0..20 {
            for write in [false, true] {
                assert_eq!(would_prompt_by(write, || Ok(Some(true)), search), Ok(true));
            }
        }
        assert_eq!(
            searches.get(),
            0,
            "searched while the default collection was locked"
        );
        // Open: the search still finds locked items in another collection.
        assert_eq!(
            would_prompt_by(false, || Ok(Some(false)), || Ok::<_, ()>(true)),
            Ok(true)
        );
        assert_eq!(
            would_prompt_by(false, || Ok(Some(false)), search),
            Ok(false)
        );
        // No default collection: a read searches, a write would ask.
        assert_eq!(would_prompt_by(false, || Ok(None), search), Ok(false));
        assert_eq!(would_prompt_by(true, || Ok(None), search), Ok(true));
        assert_eq!(searches.get(), 2);
    }

    /// Failure mode: the background write gate asked only whether a Toolport
    /// item was locked, so with none in the store a new item's write to the
    /// locked default collection asked to unlock it.
    #[test]
    fn a_background_write_waits_while_the_default_collection_is_locked() {
        with_isolated_vault(|| {
            let mut written = None;
            let prompts = locked_store::write_prompts_during(|| {
                written = Some(without_prompts(|| {
                    set_secret("srv", "API_KEY", "collection-locked-value")
                }));
            });
            assert!(prompts.is_empty(), "the write asked: {prompts:?}");
            assert!(written.unwrap().is_err());
            assert_eq!(get_secret("srv", "API_KEY"), None);
        });
    }

    /// Deletes a real keychain credential even when a test assertion unwinds.
    ///
    /// The Windows integration tests intentionally exercise Credential Manager rather than a
    /// mock. A trailing `delete_secret` is skipped when an assertion panics, and accumulated
    /// chunk credentials eventually make later test setup fail. Keep cleanup in `Drop` so every
    /// exit path removes the base credential and all generation chunks (SBS-711).
    struct SecretCleanupGuard<'a> {
        server_id: &'a str,
        key: &'a str,
    }

    impl<'a> SecretCleanupGuard<'a> {
        fn new(server_id: &'a str, key: &'a str) -> Self {
            Self { server_id, key }
        }
    }

    impl Drop for SecretCleanupGuard<'_> {
        fn drop(&mut self) {
            if let Err(error) = delete_secret(self.server_id, self.key) {
                eprintln!(
                    "toolport: test credential cleanup failed for {}/{}: {error}",
                    self.server_id, self.key
                );
            }
        }
    }

    #[test]
    fn macos_secret_replacement_never_deletes_before_writing() {
        // This source-contract test runs on Windows/Linux CI even though the
        // Security.framework implementation is only compiled on macOS. It
        // protects the data-loss invariant: both replacement paths must update
        // an existing item in place and reserve SecItemAdd for a missing item.
        let source = include_str!("secrets.rs");

        let legacy_start = source
            .find("fn add_with_shared_access(")
            .expect("legacy keychain writer must exist");
        let legacy_end = source[legacy_start..]
            .find("pub fn seed_legacy_for_test")
            .map(|offset| legacy_start + offset)
            .expect("legacy writer must end before its test helper");
        let legacy_writer = &source[legacy_start..legacy_end];

        let dpk_module_start = source
            .find("mod dpk {")
            .expect("data-protection keychain module must exist");
        let dpk_set_start = source[dpk_module_start..]
            .find("pub fn set(")
            .map(|offset| dpk_module_start + offset)
            .expect("data-protection keychain writer must exist");
        let dpk_set_end = source[dpk_set_start..]
            .find("pub fn get(")
            .map(|offset| dpk_set_start + offset)
            .expect("data-protection writer must end before its reader");
        let dpk_writer = &source[dpk_set_start..dpk_set_end];

        for writer in [legacy_writer, dpk_writer] {
            assert!(writer.contains("SecItemUpdate("));
            assert!(writer.contains("SecItemAdd("));
            assert!(!writer.contains("SecItemDelete("));
        }
    }

    #[test]
    fn native_recovery_status_codes_are_explicit_and_never_use_error_text() {
        use BackupSecretError::*;
        for (code, state) in [
            (-25308, Locked),
            (-25293, AuthenticationFailed),
            (-128, Cancelled),
            (-25300, Missing),
            (-34018, AccessDenied),
            (-25294, Unavailable),
        ] {
            assert_eq!(BackupSecretError::macos(code), state);
        }
        for (code, state) in [
            (5, AccessDenied),
            (1223, Cancelled),
            (1326, AuthenticationFailed),
            (1168, Missing),
            (13, Corrupt),
            (1312, Unavailable),
        ] {
            assert_eq!(BackupSecretError::windows(code), state);
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_backup_recovery_maps_locked_authentication_and_cancelled_statuses() {
        assert_eq!(BackupSecretError::macos(-25308), BackupSecretError::Locked);
        assert_eq!(
            BackupSecretError::macos(-25293),
            BackupSecretError::AuthenticationFailed
        );
        assert_eq!(BackupSecretError::macos(-128), BackupSecretError::Cancelled);
        assert!(include_str!("secrets.rs")
            .split_whitespace()
            .collect::<String>()
            .contains("get_inner(account_str,true)"));
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_backup_recovery_maps_credential_manager_errors() {
        assert_eq!(
            BackupSecretError::windows(1312),
            BackupSecretError::Unavailable
        );
        assert_eq!(
            BackupSecretError::windows(5),
            BackupSecretError::AccessDenied
        );
        assert_eq!(BackupSecretError::windows(1168), BackupSecretError::Missing);
        assert_eq!(
            BackupSecretError::windows(1223),
            BackupSecretError::Cancelled
        );
    }

    #[test]
    fn server_secrets_cannot_use_the_internal_task_key_namespace() {
        assert!(set_secret(INTERNAL_SERVER_ID, "user-key", "value").is_err());
        assert!(get_secret_result(INTERNAL_SERVER_ID, "user-key").is_err());
        assert!(delete_secret(INTERNAL_SERVER_ID, "user-key").is_err());
    }

    // Round-trips through the real OS keychain. Headless Linux CI has no Secret
    // Service (D-Bus), so skip it there; it still runs on Windows.
    //
    // On macOS this now targets the DATA-PROTECTION keychain (shared access
    // group), which returns errSecMissingEntitlement (-34018) on an unsigned
    // binary, so it's `#[ignore]`d there: run only on a signed build with the
    // embedded provisioning profile (see Phase 7).
    #[test]
    #[cfg_attr(target_os = "linux", ignore = "no Secret Service in headless CI")]
    #[cfg_attr(
        target_os = "macos",
        ignore = "data-protection keychain needs a signed build w/ provisioning profile (Phase 7)"
    )]
    fn set_get_delete_round_trip() {
        let _data = crate::registry::DataDirTestEnv::new("set_get_delete_round_trip");
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let sid = "conduit-test-server";
        let key = "CONDUIT_TEST_KEY";
        let _cleanup = SecretCleanupGuard::new(sid, key);
        set_secret(sid, key, "s3cr3t").unwrap();
        assert_eq!(get_secret(sid, key).as_deref(), Some("s3cr3t"));
        delete_secret(sid, key).unwrap();
        assert_eq!(get_secret(sid, key), None);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_oversized_secret_round_trips_updates_and_deletes() {
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_oversized_secret_round_trips_updates_and_deletes",
        );
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let sid = format!("toolport-windows-large-secret-{unique}");
        let key = "OAUTH_TOKEN";
        let _cleanup = SecretCleanupGuard::new(&sid, key);
        let first = format!("{}{}", "a".repeat(3_500), "🔒".repeat(300));
        let second = format!("{}{}", "b".repeat(6_100), "🚀".repeat(100));

        delete_secret(&sid, key).unwrap();
        set_secret(&sid, key, &first).unwrap();
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some(first.as_str())
        );

        set_secret(&sid, key, &second).unwrap();
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some(second.as_str())
        );

        set_secret(&sid, key, "short replacement").unwrap();
        assert_eq!(get_secret(&sid, key).as_deref(), Some("short replacement"));

        set_secret(&sid, key, &first).unwrap();
        delete_secret(&sid, key).unwrap();
        assert_eq!(get_secret_result(&sid, key).unwrap(), None);

        let reserved = "toolport-chunked-v1:0123456789abcdef0123456789abcdef:1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        set_secret(&sid, key, reserved).unwrap();
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some(reserved)
        );

        platform::set_raw_for_test(&sid, key, "toolport-chunked-v1:damaged").unwrap();
        set_secret(&sid, key, "recovered after corrupt manifest").unwrap();
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some("recovered after corrupt manifest")
        );
        delete_secret(&sid, key).unwrap();
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_chunk_reader_survives_concurrent_generation_swaps() {
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_chunk_reader_survives_concurrent_generation_swaps",
        );
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let sid = format!("toolport-windows-concurrent-secret-{unique}");
        let key = "OAUTH_TOKEN";
        let _cleanup = SecretCleanupGuard::new(&sid, key);
        let first = "a".repeat(4_000);
        let second = "b".repeat(4_000);
        set_secret(&sid, key, &first).unwrap();

        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                for index in 0..6 {
                    let value = if index % 2 == 0 { &second } else { &first };
                    set_secret(&sid, key, value).unwrap();
                }
            });
            let mut inconclusive = 0;
            for _ in 0..24 {
                match get_secret_result(&sid, key) {
                    // The guarantee that holds absolutely: a value that comes back is a
                    // WHOLE value from ONE generation. Never two spliced together, and
                    // never a manifest read against another generation's chunks.
                    Ok(Some(value)) => assert!(
                        value == first || value == second,
                        "a read must never splice two generations together"
                    ),
                    // Not a guarantee, deliberately. Windows can report a live credential
                    // as absent (and leave its chunks briefly inconsistent) while
                    // `CredWriteW` replaces it. `get_secret_result` retries to make that
                    // rare — measured across 9,600 racing reads, 37 torn reads without the
                    // retry against 4 with it — but the platform offers no promise it
                    // cannot happen. Asserting it never does is exactly what made this test
                    // fail on CI roughly half the time. The retry itself is pinned
                    // deterministically by
                    // `windows_absent_base_credential_is_retried_before_reporting_none`,
                    // which drives the absence directly instead of racing for it.
                    Ok(None) | Err(_) => inconclusive += 1,
                }
            }
            assert!(
                inconclusive < 24,
                "every read was inconclusive - the retry is not working at all"
            );
            writer.join().unwrap();
        });
        delete_secret(&sid, key).unwrap();
    }

    /// A base credential that reads as absent must be retried, not reported as
    /// "no secret".
    ///
    /// `set_secret` replaces the base credential with a plain `CredWriteW`
    /// overwrite and never deletes it, so within Toolport an absent base can only
    /// mean the credential is gone. Windows reports it absent anyway for the
    /// instant that replace is in flight, which used to return `Ok(None)` — a live
    /// token indistinguishable from never having authenticated.
    ///
    /// `windows_chunk_reader_survives_concurrent_generation_swaps` reaches the same
    /// window by racing a writer, but only sometimes: it fails ~2% of the time on
    /// an idle machine and most of the time under load. This drives the retry
    /// directly so the guarantee is pinned regardless of scheduling.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_absent_base_credential_is_retried_before_reporting_none() {
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_absent_base_credential_is_retried_before_reporting_none",
        );
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let sid = format!("toolport-windows-absent-base-{unique}");
        let key = "OAUTH_TOKEN";
        let _cleanup = SecretCleanupGuard::new(&sid, key);
        let value = "a-live-oauth-token";
        set_secret(&sid, key, value).unwrap();

        // Every read but the last reports absent: the value must still come back.
        platform::script_base_reads(&[true; platform::READ_ATTEMPTS - 1]);
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some(value),
            "a base credential absent for a transient window must be retried"
        );

        // Absent for every attempt: only now is it conclusively gone. This pins the
        // retry as BOUNDED — without it the loop could spin on a deleted secret.
        platform::script_base_reads(&[true; platform::READ_ATTEMPTS]);
        assert_eq!(
            get_secret_result(&sid, key).unwrap(),
            None,
            "absence that survives every attempt is a genuine missing secret"
        );

        // The injection is consumed, so the secret reads normally again.
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some(value)
        );
        delete_secret(&sid, key).unwrap();
        assert_eq!(get_secret_result(&sid, key).unwrap(), None);
    }

    /// An integrity failure must not decay into "no secret" when later base reads
    /// land in the transient `CredWriteW` absence window.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_chunk_error_survives_trailing_absent_base_reads() {
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_chunk_error_survives_trailing_absent_base_reads",
        );
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let sid = format!("toolport-windows-chunk-error-{unique}");
        let key = "OAUTH_TOKEN";
        let _cleanup = SecretCleanupGuard::new(&sid, key);
        let missing_chunk_manifest = concat!(
            "toolport-chunked-v1:0123456789abcdef0123456789abcdef:1:",
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
        platform::set_raw_for_test(&sid, key, missing_chunk_manifest).unwrap();

        // Read the broken manifest once, then report the base absent for every
        // remaining attempt. The earlier integrity error must win over absence.
        let mut read_script = vec![true; platform::READ_ATTEMPTS];
        read_script[0] = false;
        platform::script_base_reads(&read_script);
        let error = get_secret_result(&sid, key).unwrap_err();
        assert!(
            error.contains("chunk 0 is missing"),
            "expected the missing-chunk error, got: {error}"
        );

        delete_secret(&sid, key).unwrap();
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_secret_cleanup_guard_deletes_on_unwind() {
        let _data =
            crate::registry::DataDirTestEnv::new("windows_secret_cleanup_guard_deletes_on_unwind");
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let sid = format!("toolport-windows-cleanup-unwind-{unique}");
        let key = "OAUTH_TOKEN";

        let cleanup = SecretCleanupGuard::new(&sid, key);
        set_secret(&sid, key, &"x".repeat(5_000)).unwrap();
        let raw_entries = platform::raw_entries_for_test(&sid, key).unwrap();
        assert!(
            raw_entries.len() > 1,
            "fixture must create chunk credentials"
        );

        let unwound = std::panic::catch_unwind(|| {
            let _cleanup = cleanup;
            panic!("exercise panic-safe credential cleanup");
        });

        let payload = unwound.expect_err("the fixture must actually unwind");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"exercise panic-safe credential cleanup")
        );
        assert_eq!(
            get_secret_result(&sid, key).unwrap(),
            None,
            "the drop guard must remove the credential and its chunks during unwind"
        );
        for account in raw_entries {
            assert!(
                !platform::raw_entry_exists_for_test(&account).unwrap(),
                "cleanup left Windows credential entry {account} behind"
            );
        }
    }

    /// One process of `windows_cross_process_access_never_tears_a_chunked_secret`.
    #[test]
    #[cfg(target_os = "windows")]
    #[ignore = "spawned by windows_cross_process_access_never_tears_a_chunked_secret"]
    fn windows_cross_process_child() {
        let _data = crate::registry::DataDirTestEnv::new("windows_cross_process_child");
        let sid = std::env::var("TOOLPORT_CROSS_PROCESS_SID").unwrap();
        let writer = std::env::var("TOOLPORT_CROSS_PROCESS_ROLE").as_deref() == Ok("writer");
        let values = cross_process_values();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (mut reads, mut torn) = (0u64, Vec::new());
        let mut index = 0;
        while std::time::Instant::now() < deadline {
            if writer {
                set_secret(&sid, "OAUTH_TOKEN", &values[index % values.len()]).unwrap();
                index += 1;
                continue;
            }
            reads += 1;
            match get_secret_result(&sid, "OAUTH_TOKEN") {
                Ok(Some(value)) if values.contains(&value) => {}
                Ok(Some(value)) => torn.push(format!("mixed value of {} bytes", value.len())),
                Ok(None) => torn.push("absent".to_string()),
                Err(error) => torn.push(error),
            }
        }
        assert!(
            torn.is_empty(),
            "{} of {reads} reads were torn, first: {:?}",
            torn.len(),
            torn.iter().take(3).collect::<Vec<_>>()
        );
        println!("cross-process child done: writes {index}, reads {reads}");
    }

    #[cfg(target_os = "windows")]
    fn cross_process_values() -> [String; 3] {
        [
            format!("{}{}", "a".repeat(3_500), "🔒".repeat(300)),
            format!("{}{}", "b".repeat(6_100), "🚀".repeat(100)),
            "short replacement".to_string(),
        ]
    }

    /// The app and the gateway are separate processes sharing one vault. A chunked
    /// secret must read back whole in one process while another rewrites it, and
    /// replaced generations must not be leaked.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_cross_process_access_never_tears_a_chunked_secret() {
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
        use windows_sys::Win32::Security::Credentials::{CredEnumerateW, CredFree, CREDENTIALW};
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_cross_process_access_never_tears_a_chunked_secret",
        );
        // Keep env-switching tests (e.g. CONDUIT_SECRET_KEY) from changing the
        // backend for the parent or the children it spawns.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sid = format!("toolport-windows-cross-process-{unique}");
        let _cleanup = SecretCleanupGuard::new(&sid, "OAUTH_TOKEN");
        set_secret(&sid, "OAUTH_TOKEN", &cross_process_values()[0]).unwrap();
        let children: Vec<_> = ["writer", "reader", "reader", "reader"]
            .into_iter()
            .map(|role| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "secrets::tests::windows_cross_process_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("TOOLPORT_CROSS_PROCESS_SID", &sid)
                    .env("TOOLPORT_CROSS_PROCESS_ROLE", role)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        // Reap every child before asserting, so none outlives the test.
        let outputs: Vec<_> = children
            .into_iter()
            .map(|child| child.wait_with_output().unwrap())
            .collect();
        for output in &outputs {
            assert!(
                output.status.success(),
                "cross-process child failed: {}\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        delete_secret(&sid, "OAUTH_TOKEN").unwrap();
        let filter: Vec<u16> = format!("{sid}::*").encode_utf16().chain([0]).collect();
        let mut count = 0;
        let mut entries: *mut *mut CREDENTIALW = std::ptr::null_mut();
        if unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut entries) } != 0 {
            unsafe { CredFree(entries.cast()) };
        } else {
            // Only "nothing matched" may count as zero leftovers.
            let error = unsafe { GetLastError() };
            assert_eq!(error, ERROR_NOT_FOUND, "CredEnumerateW failed: {error}");
        }
        assert_eq!(count, 0, "replaced chunk generations were left behind");
    }

    /// Orphaned chunk generations, from crashed or racing writes, are removed. The
    /// live generation and anything that is not exactly a Toolport chunk stay.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_sweep_removes_only_orphaned_toolport_chunks() {
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_sweep_removes_only_orphaned_toolport_chunks",
        );
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Every account here starts with `sid`, the only scope the sweep may see.
        let sid = format!("toolport-windows-sweep-{unique}");
        let gone_sid = format!("{sid}-gone");
        let key = "OAUTH_TOKEN";
        let _cleanup = SecretCleanupGuard::new(&sid, key);
        let value = format!("{}{}", "a".repeat(3_500), "🔒".repeat(300));
        set_secret(&sid, key, &value).unwrap();
        let live = platform::raw_entries_for_test(&sid, key).unwrap();
        assert!(live.len() > 1, "fixture must be chunked");

        let base = format!("{sid}::{key}");
        let stale = "0123456789abcdef0123456789abcdef";
        let orphans = [
            // A replaced generation the cleanup never removed.
            platform::plant_chunk_for_test(&base, stale, 0).unwrap(),
            platform::plant_chunk_for_test(&base, stale, 1).unwrap(),
            // Chunks whose base credential no longer exists at all.
            platform::plant_chunk_for_test(&format!("{gone_sid}::{key}"), stale, 0).unwrap(),
        ];
        // Look-alikes that are not Toolport chunks must never be touched.
        let lookalikes = [
            format!("{base}::toolport-chunk::not-a-generation::0"),
            format!("{base}::toolport-chunk::{stale}::x"),
        ];
        for account in &lookalikes {
            platform::set_raw_account_for_test(account, "keep me").unwrap();
        }
        let foreign = keyring::Entry::new(
            "toolport-sweep-other-app",
            &format!("{base}::toolport-chunk::{stale}::0"),
        )
        .unwrap();
        foreign.set_password("other app").unwrap();

        // Fresh orphans are left alone: a pre-lock process may still be writing them.
        assert_eq!(
            platform::sweep_orphaned_chunks_for_test(std::time::Duration::from_secs(3600), &sid),
            0
        );
        for account in &orphans {
            assert!(platform::raw_entry_exists_for_test(account).unwrap());
        }

        assert_eq!(
            platform::sweep_orphaned_chunks_for_test(std::time::Duration::ZERO, &sid),
            orphans.len()
        );
        for account in &orphans {
            assert!(
                !platform::raw_entry_exists_for_test(account).unwrap(),
                "orphaned chunk {account} survived the sweep"
            );
        }
        for account in &live {
            assert!(
                platform::raw_entry_exists_for_test(account).unwrap(),
                "the sweep removed live entry {account}"
            );
        }
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some(value.as_str())
        );
        assert_eq!(foreign.get_password().unwrap(), "other app");
        foreign.delete_credential().unwrap();
        for account in &lookalikes {
            assert!(
                platform::raw_entry_exists_for_test(account).unwrap(),
                "the sweep removed non-chunk entry {account}"
            );
            platform::delete_raw_account_for_test(account).unwrap();
        }
    }

    /// The sweep deletes only on positive evidence. A base that the vault lists but
    /// a read reports absent (Windows does this mid-rewrite), a damaged manifest,
    /// and a manifest version newer than this build all keep every generation.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_sweep_keeps_chunks_it_cannot_prove_orphaned() {
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_sweep_keeps_chunks_it_cannot_prove_orphaned",
        );
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(platform::lock_mutex_opens_for_test().is_ok());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sid = format!("toolport-windows-sweep-keep-{unique}");
        let (future_sid, damaged_sid) = (format!("{sid}-future"), format!("{sid}-damaged"));
        let key = "OAUTH_TOKEN";
        let _cleanup = [
            SecretCleanupGuard::new(&sid, key),
            SecretCleanupGuard::new(&future_sid, key),
            SecretCleanupGuard::new(&damaged_sid, key),
        ];
        let value = format!("{}{}", "a".repeat(3_500), "🔒".repeat(300));
        set_secret(&sid, key, &value).unwrap();
        let live = platform::raw_entries_for_test(&sid, key).unwrap();
        let stale = "0123456789abcdef0123456789abcdef";
        let replaced = platform::plant_chunk_for_test(&format!("{sid}::{key}"), stale, 0).unwrap();

        // Listed by the vault, absent to every read: nothing is provably orphaned.
        let removed = platform::with_sweep_reads_absent(|| {
            platform::sweep_orphaned_chunks_for_test(std::time::Duration::ZERO, &sid)
        });
        assert_eq!(
            removed, 0,
            "a false absence must not condemn any generation"
        );
        for account in live.iter().chain([&replaced]) {
            assert!(
                platform::raw_entry_exists_for_test(account).unwrap(),
                "a false absence removed {account}"
            );
        }

        // Manifests this build cannot read keep their chunks too.
        platform::set_raw_for_test(&future_sid, key, "toolport-chunked-v2:from-a-newer-build")
            .unwrap();
        platform::set_raw_for_test(&damaged_sid, key, "toolport-chunked-v1:damaged").unwrap();
        let unreadable = [
            platform::plant_chunk_for_test(&format!("{future_sid}::{key}"), stale, 0).unwrap(),
            platform::plant_chunk_for_test(&format!("{damaged_sid}::{key}"), stale, 0).unwrap(),
        ];
        // With honest reads the replaced generation is provably orphaned; only it goes.
        assert_eq!(
            platform::sweep_orphaned_chunks_for_test(std::time::Duration::ZERO, &sid),
            1
        );
        assert!(!platform::raw_entry_exists_for_test(&replaced).unwrap());
        for account in live.iter().chain(&unreadable) {
            assert!(
                platform::raw_entry_exists_for_test(account).unwrap(),
                "the sweep removed {account}"
            );
        }
        assert_eq!(
            get_secret_result(&sid, key).unwrap().as_deref(),
            Some(value.as_str())
        );
        for account in &unreadable {
            platform::delete_raw_account_for_test(account).unwrap();
        }
    }

    /// SIDs are not secret, so another account can pre-create the lock name. A
    /// mutex this user does not own is refused rather than waited on.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_lock_refuses_a_mutex_another_account_owns() {
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::System::Threading::CreateMutexExW;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // A name nobody holds is created with this user as owner and accepted.
        assert_eq!(
            platform::open_lock_mutex_named_for_test(&format!(
                "Global\\toolport-test-fresh-lock-{unique}"
            )),
            Ok(())
        );
        // Stand-in squatter: owner Administrators instead of this user, which an
        // elevated test process may assign. Elsewhere this case cannot be set up.
        let squatted = format!("Global\\toolport-test-squatted-lock-{unique}");
        let name: Vec<u16> = squatted.encode_utf16().chain([0]).collect();
        let sddl: Vec<u16> = "O:BAD:(A;;GA;;;WD)".encode_utf16().chain([0]).collect();
        let mut descriptor = std::ptr::null_mut();
        assert!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } != 0
        );
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let squatter = unsafe { CreateMutexExW(&attributes, name.as_ptr(), 0, 0x001F_0001) };
        unsafe { LocalFree(descriptor.cast()) };
        if squatter.is_null() {
            eprintln!(
                "skipping: cannot create a mutex owned by Administrators here ({})",
                std::io::Error::last_os_error()
            );
            return;
        }
        let refused = platform::open_lock_mutex_named_for_test(&squatted);
        unsafe { CloseHandle(squatter) };
        let error = refused.expect_err("a mutex owned by another account must be refused");
        assert!(error.contains("another account"), "{error}");
    }

    /// A vault slow enough to list that one snapshot outlasts a whole batch must
    /// still be swept to completion in bounded time, and the lock stays free for
    /// other callers while the sweep lists the vault.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_sweep_terminates_when_snapshots_are_slow() {
        let _data = crate::registry::DataDirTestEnv::new(
            "windows_sweep_terminates_when_snapshots_are_slow",
        );
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sid = format!("toolport-windows-sweep-slow-{unique}");
        let key = "OAUTH_TOKEN";
        let value = format!("{}{}", "a".repeat(3_500), "🔒".repeat(300));
        let stale = "0123456789abcdef0123456789abcdef";
        let servers: Vec<String> = (0..4).map(|index| format!("{sid}-{index}")).collect();
        let _cleanup: Vec<_> = servers
            .iter()
            .map(|server| SecretCleanupGuard::new(server, key))
            .collect();
        let mut orphans = Vec::new();
        for server in &servers {
            set_secret(server, key, &value).unwrap();
            for index in 0..3 {
                orphans.push(
                    platform::plant_chunk_for_test(&format!("{server}::{key}"), stale, index)
                        .unwrap(),
                );
            }
        }
        // And chunks whose base is gone, which need two snapshots to condemn.
        orphans
            .push(platform::plant_chunk_for_test(&format!("{sid}-gone::{key}"), stale, 0).unwrap());

        let started = std::time::Instant::now();
        let removed =
            platform::with_slow_sweep_snapshots(std::time::Duration::from_millis(300), || {
                platform::sweep_orphaned_chunks_for_test(std::time::Duration::ZERO, &sid)
            });
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "the sweep ran for {elapsed:?}"
        );
        assert_eq!(removed, orphans.len());
        for account in &orphans {
            assert!(!platform::raw_entry_exists_for_test(account).unwrap());
        }

        // While another thread's sweep is in a slow listing, the lock stays free:
        // the listing is taken before the lock, so other credential calls do not
        // queue behind it.
        let sweeper = std::thread::spawn({
            let sid = sid.clone();
            move || {
                platform::with_slow_sweep_snapshots(std::time::Duration::from_secs(3), || {
                    platform::sweep_orphaned_chunks_for_test(std::time::Duration::ZERO, &sid)
                })
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        let waited = platform::credential_lock_wait_for_test().expect("credential lock");
        assert!(
            !sweeper.is_finished(),
            "the sweep's listing ended before the lock was tried"
        );
        assert!(
            waited < std::time::Duration::from_secs(1),
            "the lock waited {waited:?} behind the sweep's listing"
        );
        assert_eq!(sweeper.join().unwrap(), 0);
        for server in &servers {
            assert!(get_secret_result(server, key).unwrap().is_some());
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_production_sweep_covers_the_whole_vault_with_production_limits() {
        assert!(platform::production_sweep_is_whole_vault_with_production_limits());
    }

    /// Cross-platform: setting `CONDUIT_SECRET_KEY` activates the file backend.
    /// Runs on every OS (the env-var precedence is platform-independent).
    ///
    /// NOTE: mutates a process-global env var, so it's `#[serial]`-free but kept
    /// self-contained — it sets and then unsets the var, restoring prior state.
    #[test]
    fn file_active_when_env_key_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("CONDUIT_SECRET_KEY").ok();
        std::env::set_var("CONDUIT_SECRET_KEY", "unit-test-passphrase");
        assert!(
            file::active(),
            "file backend must be active when CONDUIT_SECRET_KEY is set"
        );
        // Restore prior state so other tests aren't affected.
        match prev {
            Some(v) => std::env::set_var("CONDUIT_SECRET_KEY", v),
            None => std::env::remove_var("CONDUIT_SECRET_KEY"),
        }
    }

    /// Process-env secrets win over the vault so a container can inject keys via
    /// an env file without writing `secrets.enc`. Prefixed form beats bare key;
    /// bare keys require CONDUIT_ALLOW_BARE_SECRET_ENV.
    #[test]
    fn env_secret_override_prefers_prefixed_then_bare() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let key = "TOOLPORT_ENV_SECRET_TEST";
        let prefixed = format!("CONDUIT_SECRET_{key}");
        let prev_bare = std::env::var(key).ok();
        let prev_pref = std::env::var(&prefixed).ok();
        let prev_allow = std::env::var("CONDUIT_ALLOW_BARE_SECRET_ENV").ok();
        std::env::remove_var(key);
        std::env::remove_var(&prefixed);
        std::env::remove_var("CONDUIT_ALLOW_BARE_SECRET_ENV");

        assert_eq!(env_secret_override(key), None);

        std::env::set_var(key, "from-bare");
        assert_eq!(
            env_secret_override(key),
            None,
            "bare key blocked without opt-in"
        );

        std::env::set_var("CONDUIT_ALLOW_BARE_SECRET_ENV", "1");
        assert_eq!(env_secret_override(key).as_deref(), Some("from-bare"));

        std::env::set_var(&prefixed, "from-prefixed");
        assert_eq!(env_secret_override(key).as_deref(), Some("from-prefixed"));

        // Preserve intentional whitespace; blank-only values don't count.
        std::env::set_var(&prefixed, "  spaced  ");
        assert_eq!(env_secret_override(key).as_deref(), Some("  spaced  "));
        std::env::set_var(&prefixed, "   ");
        std::env::set_var(key, "");
        assert_eq!(env_secret_override(key), None);

        match prev_bare {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        match prev_pref {
            Some(v) => std::env::set_var(&prefixed, v),
            None => std::env::remove_var(&prefixed),
        }
        match prev_allow {
            Some(v) => std::env::set_var("CONDUIT_ALLOW_BARE_SECRET_ENV", v),
            None => std::env::remove_var("CONDUIT_ALLOW_BARE_SECRET_ENV"),
        }
    }

    /// macOS: ensuring the master key returns 32 bytes and is idempotent — a
    /// second `ensure` returns the SAME key, and a plain `read` returns it too.
    /// This uses the real legacy Keychain and can prompt for ACL access, so it is
    /// opt-in on headless CI where no user can answer the prompt.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an interactive macOS keychain; unavailable in headless CI"]
    fn master_key_ensure_then_read_is_idempotent() {
        let first = platform::ensure_master_key().expect("ensure should succeed");
        assert_eq!(first.len(), 32);
        let second = platform::ensure_master_key().expect("ensure should be idempotent");
        assert_eq!(first, second, "ensure must return the same key each time");
        let read = platform::read_master_key()
            .expect("read should succeed")
            .expect("master key should exist after ensure");
        assert_eq!(first, read, "read must return the ensured key");
    }

    /// macOS: the file backend is headless-only. Without `CONDUIT_SECRET_KEY`,
    /// `active()` stays false even after a keychain master key exists, so desktop
    /// secrets route to the data-protection keychain, never to `secrets.enc`.
    #[cfg(target_os = "macos")]
    #[test]
    fn master_key_does_not_activate_file_backend() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("CONDUIT_SECRET_KEY").ok();
        std::env::remove_var("CONDUIT_SECRET_KEY");
        // Creating a master key must NOT flip the file backend on (best-effort:
        // the create may no-op on an unsigned build, which is fine for this test).
        let _ = platform::ensure_master_key();
        assert!(
            !file::active(),
            "file backend must stay inactive on desktop without CONDUIT_SECRET_KEY"
        );
        if let Some(v) = prev {
            std::env::set_var("CONDUIT_SECRET_KEY", v);
        }
    }

    #[test]
    fn file_backend_seal_open_round_trip() {
        let key = [7u8; 32];
        let sealed = file::seal(&key, b"s3cr3t value").unwrap();
        // A fresh nonce per seal means the same plaintext seals to different bytes.
        assert_ne!(sealed, file::seal(&key, b"s3cr3t value").unwrap());
        // The right key recovers the plaintext; a wrong key is rejected.
        assert_eq!(file::open(&key, &sealed).unwrap(), b"s3cr3t value");
        let mut wrong = key;
        wrong[0] ^= 0xff;
        assert!(file::open(&wrong, &sealed).is_err());
    }

    /// SOU-332: concurrent file-backend writers must not drop each other's keys.
    /// Without the load-modify-save lock, last atomic_write wins and loses peers.
    #[test]
    fn file_backend_concurrent_sets_preserve_all_keys() {
        let _data_dir = crate::registry::data_dir_test_lock();
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // This asserts no key is LOST, not that every writer wins the lock inside the
        // production budget. On a loaded runner the 5s default expires, a writer
        // correctly gives up, and this fails for the machine's timing (SBS-895).
        let _lock_budget = crate::registry::LockTimeoutOverride::generous();
        let dir = std::env::temp_dir().join(format!(
            "toolport-sou332-secrets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _dir_override = crate::registry::DataDirOverride::set(&dir);

        let prev_key = std::env::var("CONDUIT_SECRET_KEY").ok();
        let prev_toolport = std::env::var("TOOLPORT_SECRET_KEY").ok();
        std::env::remove_var("TOOLPORT_SECRET_KEY");
        std::env::set_var("CONDUIT_SECRET_KEY", "sou-332-unit-test-passphrase");
        assert!(file::active());

        const THREADS: usize = 8;
        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                std::thread::spawn(move || {
                    let key = format!("KEY_{i}");
                    set_secret("srv", &key, &format!("val-{i}")).expect("set_secret");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        for i in 0..THREADS {
            let key = format!("KEY_{i}");
            assert_eq!(
                get_secret("srv", &key).as_deref(),
                Some(format!("val-{i}").as_str()),
                "key {key} must survive concurrent writers"
            );
        }

        match prev_key {
            Some(v) => std::env::set_var("CONDUIT_SECRET_KEY", v),
            None => std::env::remove_var("CONDUIT_SECRET_KEY"),
        }
        match prev_toolport {
            Some(v) => std::env::set_var("TOOLPORT_SECRET_KEY", v),
            None => std::env::remove_var("TOOLPORT_SECRET_KEY"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The data-protection keychain round-trips a per-server secret: write via
    /// `platform::set_secret` (`dpk::set`), read it back via
    /// `platform::get_secret_result` (`dpk::get`), then delete it
    /// (`dpk::delete`). After delete, the read returns `None`.
    ///
    /// Signed-build-only: every call carries `kSecUseDataProtectionKeychain`, so
    /// an unsigned binary gets errSecMissingEntitlement (-34018). Run only on a
    /// signed build with the embedded provisioning profile (see Phase 7).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "data-protection keychain needs a signed build w/ provisioning profile (Phase 7)"]
    fn dpk_set_get_delete_round_trip() {
        let sid = "conduit-dpk-test";
        let key = "DPK_ROUND_TRIP_KEY";
        let original = "s3cr3t-value-in-data-protection-keychain";

        platform::set_secret(sid, key, original).unwrap();
        assert_eq!(
            platform::get_secret_result(sid, key).unwrap().as_deref(),
            Some(original),
            "value must round-trip through the data-protection keychain"
        );

        platform::delete_secret(sid, key).unwrap();
        assert_eq!(
            platform::get_secret_result(sid, key).unwrap(),
            None,
            "value must be gone after delete"
        );
    }

    /// The legacy -> data-protection migration preserves secret values: it reads
    /// the old legacy item, writes + verifies the new DP item, then deletes the
    /// legacy one. After migration the value reads back from the DP store.
    ///
    /// This exercises `platform::migrate_legacy_to_dpk`, whose write/verify steps
    /// hit the data-protection keychain (errSecMissingEntitlement -34018 on an
    /// unsigned binary). Signed-build-only: run only on a signed build with the
    /// embedded provisioning profile (see Phase 7). Seeding a *legacy* keychain
    /// item to migrate FROM also requires the keychain to be writable in that
    /// context, which this harness sets up on the signed build.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "data-protection keychain needs a signed build w/ provisioning profile (Phase 7)"]
    fn migrate_legacy_to_dpk_preserves_values() {
        let sid = "conduit-migrate-test";
        let key = "MIGRATE_PRESERVE_KEY";
        let original = "s3cr3t-value-to-preserve";

        // Seed the OLD legacy keychain item (the migration reads it via
        // `get_generic_password`). `add_with_shared_access` writes to the legacy
        // keychain under the same SERVICE/account the migration scans.
        platform::seed_legacy_for_test(sid, key, original).unwrap();

        let keys = vec![(sid.to_string(), key.to_string())];
        let report = platform::migrate_legacy_to_dpk(&keys).expect("migration should succeed");

        assert_eq!(report.migrated, 1, "one entry should have been migrated");
        assert_eq!(report.failed, 0, "no entries should have failed");
        assert_eq!(report.not_found, 0, "no entries should have been not-found");

        // The value must now read back from the DATA-PROTECTION store.
        assert_eq!(
            platform::get_secret_result(sid, key).unwrap().as_deref(),
            Some(original),
            "secret value must survive migration into the data-protection store"
        );

        // Clean up the DP item.
        platform::delete_secret(sid, key).unwrap();
    }

    /// The migration correctly reports `not_found` for keys that don't exist in
    /// the legacy keychain (e.g. `__oauth_state__` on a non-OAuth server). This
    /// should not be counted as a failure.
    ///
    /// The not-found path only reads the legacy keychain (`get_generic_password`)
    /// and never reaches the data-protection write, so this part is exercisable
    /// unsigned. Kept `#[ignore]` for consistency with the DP-migration suite
    /// (signed-build-only).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "part of the data-protection migration suite; run on a signed build (Phase 7)"]
    fn migrate_legacy_to_dpk_reports_not_found_for_missing_keys() {
        let sid = "conduit-missing-test";
        let key = "THIS_KEY_DOES_NOT_EXIST";

        // Ensure neither store has the entry.
        let _ = platform::delete_secret(sid, key);

        let keys = vec![(sid.to_string(), key.to_string())];
        let report = platform::migrate_legacy_to_dpk(&keys).expect("migration should succeed");

        assert_eq!(report.migrated, 0, "nothing to migrate");
        assert_eq!(report.failed, 0, "missing keys are not failures");
        assert_eq!(
            report.not_found, 1,
            "one key should be reported as not-found"
        );
    }
}
