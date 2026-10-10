//! Remote (http) server connection with automatic OAuth token refresh.
//!
//! When a connection fails with an auth error and we have a stored refresh
//! token, we transparently refresh the access token and retry once. The OAuth
//! state (token endpoint, client id, refresh token) is vaulted alongside the
//! access token.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::downstream::{
    DownstreamServer, HttpTransport, ProgressSink, RefreshFn, ResourceUpdatedSink,
    ScopeReauthorizeFn, ServerRequestHandler, Transport,
};
use crate::registry::ServerEntry;
use crate::{oauth, secrets};

const STATE_KEY: &str = "__oauth_state__";
pub const OAUTH_STATE_KEY: &str = STATE_KEY;
/// Refresh before the exact deadline so the token cannot expire while an MCP
/// request is in flight.
const PROACTIVE_REFRESH_SKEW_SECS: u64 = 60;
/// Avoid hammering a temporarily unavailable OAuth endpoint on every tool call
/// while still retrying within the pre-expiry safety window.
const PROACTIVE_REFRESH_RETRY_SECS: u64 = 15;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn request_timeout(server: &ServerEntry) -> Result<Duration, String> {
    match server.request_timeout_ms {
        Some(milliseconds) => {
            crate::registry::validate_request_timeout_ms(milliseconds).map(Duration::from_millis)
        }
        None => Ok(DEFAULT_REQUEST_TIMEOUT),
    }
}

#[derive(Serialize, Deserialize)]
struct OAuthState {
    /// Validated authorization-server issuer that owns the client credentials.
    /// Optional for states vaulted before Toolport recorded issuer binding.
    #[serde(default)]
    issuer: Option<String>,
    token_endpoint: String,
    client_id: String,
    refresh_token: Option<String>,
    /// The RFC 8707 resource indicator (the MCP server URL) the token is bound
    /// to. Optional for back-compat with states vaulted before this existed.
    #[serde(default)]
    resource: Option<String>,
    /// Scope set requested for the current authorization. Optional for vaulted
    /// states written before Toolport supported runtime scope step-up.
    #[serde(default)]
    scope: Option<String>,
    /// Unix timestamp when Toolport received the latest token response.
    /// Optional for states vaulted by older Toolport versions.
    #[serde(default)]
    issued_at: Option<u64>,
    /// Unix access-token expiry derived from the provider's `expires_in`.
    /// Optional because OAuth providers are allowed to omit the lifetime.
    #[serde(default)]
    expires_at: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshDecision {
    NotNeeded,
    Refresh,
    Reauthenticate,
}

#[derive(Clone)]
struct RefreshedToken {
    access_token: String,
    expires_at: Option<u64>,
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn refresh_decision(state: &OAuthState, now: u64) -> RefreshDecision {
    let Some(expires_at) = state.expires_at else {
        // Backward-compatible and provider-compatible: without a known expiry,
        // retain the existing reactive refresh on 401/403.
        return RefreshDecision::NotNeeded;
    };
    if now.saturating_add(PROACTIVE_REFRESH_SKEW_SECS) < expires_at {
        RefreshDecision::NotNeeded
    } else if state.refresh_token.is_some() {
        RefreshDecision::Refresh
    } else {
        RefreshDecision::Reauthenticate
    }
}

/// Persist what's needed to refresh this server's token later.
pub fn store_oauth_state(
    server_id: &str,
    issuer: Option<String>,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: Option<String>,
    resource: Option<String>,
    scope: Option<String>,
    issued_at: u64,
    expires_at: Option<u64>,
) -> Result<(), String> {
    let state = OAuthState {
        issuer,
        token_endpoint: token_endpoint.to_string(),
        client_id: client_id.to_string(),
        refresh_token,
        resource,
        scope,
        issued_at: Some(issued_at),
        expires_at,
    };
    let json = serde_json::to_string(&state).map_err(|e| e.to_string())?;
    secrets::set_secret(server_id, STATE_KEY, &json)
}

/// Decode a vaulted JSON blob, distinguishing confirmed-missing (`Ok(None)`)
/// from a failed read or an unreadable stored value (`Err`).
///
/// A locked keychain must not look like "never saved" (SBS-840), and a stored
/// blob that does not parse is not treated as missing — something is there,
/// just unreadable. The blob is left in place.
fn decode_vaulted_json<T: DeserializeOwned>(
    blob: Result<Option<String>, String>,
    what: &str,
) -> Result<Option<T>, String> {
    match blob {
        Ok(None) => Ok(None),
        Ok(Some(s)) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| format!("could not parse the vaulted {what}: {e}")),
        Err(e) => Err(format!("could not read the vaulted {what}: {e}")),
    }
}

/// A failed vault read is NOT "missing" (SBS-840): a locked keychain must
/// not look like the user never authenticated.
fn load_state(server_id: &str) -> Result<Option<OAuthState>, String> {
    decode_vaulted_json(
        secrets::get_secret_result(server_id, STATE_KEY),
        "OAuth state",
    )
}

/// Same fail-closed mapping as [`load_state`] for the headless flow (SBS-840).
fn load_cc_state(server_id: &str) -> Result<Option<ClientCredentialsState>, String> {
    decode_vaulted_json(
        secrets::get_secret_result(server_id, CC_STATE_KEY),
        "client-credentials state",
    )
}

fn issuer_bound_token_endpoint<'a>(
    expected_issuer: &str,
    endpoints: &'a oauth::Endpoints,
) -> Result<&'a str, String> {
    if endpoints.issuer == expected_issuer {
        Ok(&endpoints.token_endpoint)
    } else {
        Err(
            "the server's OAuth issuer changed; needs authentication before credentials can be reused"
                .to_string(),
        )
    }
}

/// Remove refresh metadata when the user clears OAuth or replaces it with a
/// manually pasted bearer token. Otherwise stale vaulted state could silently
/// recreate a credential the user explicitly removed.
pub fn clear_oauth_state(server_id: &str) -> Result<(), String> {
    // Attempt both, then surface the first failure. Swallowing the
    // client-credentials delete would leave state that silently reacquires with
    // the long-lived secret after the user believed they had cleared auth; only
    // attempting the second on success would leave the other key behind.
    let headless = secrets::delete_secret(server_id, CC_STATE_KEY);
    let interactive = secrets::delete_secret(server_id, STATE_KEY);
    headless.and(interactive)
}

// ── Client-credentials flow (SBS-524) ──────────────────────────────────────

const CC_STATE_KEY: &str = "__oauth_cc_state__";

/// What a later reacquisition needs, resolved once at connect time.
///
/// The reacquire seam (`refresh_token_with_expiry`) is reached from the request
/// path with only a server id, so everything needed to mint another token is
/// captured here rather than looked up from the registry again. That also means a
/// reacquisition uses the same issuer and method the first one did, instead of
/// silently following a metadata document that changed underneath it.
///
/// Holds no secret: the client secret stays in the vault under its own key.
#[derive(Serialize, Deserialize)]
struct ClientCredentialsState {
    issuer: String,
    token_endpoint: String,
    client_id: String,
    /// The negotiated `token_endpoint_auth_method` identifier.
    method: String,
    #[serde(default)]
    scope: Option<String>,
    /// RFC 8707 resource indicator (the MCP server URL).
    resource: String,
    #[serde(default)]
    expires_at: Option<u64>,
}

/// Discover, negotiate an auth method, and mint an access token for a headless
/// server. Vaults the token and the state a later reacquisition needs.
///
/// Fails closed rather than falling back to the interactive flow: a server that
/// silently opened a browser would be unusable in the environment this exists for.
fn acquire_client_credentials(
    server_id: &str,
    resource: &str,
    config: &crate::registry::ClientCredentials,
) -> Result<RefreshedToken, String> {
    // A vault read failure is not "no client secret" (SBS-840): a locked
    // keychain must not look like the secret was never saved.
    let secret = match secrets::get_secret_result(server_id, secrets::CLIENT_SECRET_KEY) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err(
                "no client secret is vaulted for this server; add one before connecting \
                 (client-credentials auth never falls back to a browser sign-in)"
                    .to_string(),
            )
        }
        Err(e) => return Err(format!("could not read the vaulted client secret: {e}")),
    };
    let configured = match config.token_endpoint_auth_method.as_deref() {
        Some(raw) => Some(oauth::ClientAuthMethod::parse(raw).ok_or_else(|| {
            format!("unknown token_endpoint_auth_method {raw:?} configured for this server")
        })?),
        None => None,
    };

    let endpoints = oauth::discover(resource)?;
    let method = oauth::select_client_auth_method(
        configured,
        endpoints.token_endpoint_auth_methods_supported.as_deref(),
    )?;
    // Prefer the user's explicit scopes; otherwise take what discovery advertises
    // for this protected resource, matching the interactive flow.
    let scope = config.scope.clone().or_else(|| endpoints.scope.clone());

    let block_private = oauth::host_of_url(&endpoints.token_endpoint)
        .map(|h| !oauth::host_is_definitely_private(&h))
        .unwrap_or(true);
    let tokens = oauth::client_credentials_token(
        &endpoints.token_endpoint,
        &config.client_id,
        &secret,
        method,
        scope.as_deref(),
        Some(resource),
        block_private,
    )?;

    // State first, then the access token: a failure between the two leaves the
    // next attempt able to reacquire, where the reverse order could strand a
    // token with no way to mint its successor. Same ordering as the refresh path.
    let state = ClientCredentialsState {
        issuer: endpoints.issuer,
        token_endpoint: endpoints.token_endpoint,
        client_id: config.client_id.clone(),
        method: method.as_str().to_string(),
        scope,
        resource: resource.to_string(),
        expires_at: tokens.expires_at,
    };
    let json = serde_json::to_string(&state).map_err(|e| e.to_string())?;
    secrets::set_secret(server_id, CC_STATE_KEY, &json)?;
    secrets::set_secret(server_id, secrets::HTTP_AUTH_KEY, &tokens.access_token)?;
    Ok(RefreshedToken {
        access_token: tokens.access_token,
        expires_at: tokens.expires_at,
    })
}

/// Mint a replacement token from vaulted client-credentials state.
///
/// There is no refresh token to redeem (RFC 6749 §4.4.3), so this re-runs the
/// grant. It reuses the recorded token endpoint and method rather than
/// rediscovering, and re-verifies the issuer when it does discover, so a resource
/// that changed authorization server fails closed instead of sending the secret
/// somewhere new.
fn reacquire_client_credentials(server_id: &str) -> Result<RefreshedToken, String> {
    // A failed read is not "no state" / "secret is gone" (SBS-840).
    let state = load_cc_state(server_id)?.ok_or("no client-credentials state to reacquire from")?;
    let secret = match secrets::get_secret_result(server_id, secrets::CLIENT_SECRET_KEY) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Err("the vaulted client secret is gone; re-add it for this server".to_string())
        }
        Err(e) => return Err(format!("could not read the vaulted client secret: {e}")),
    };
    let method = oauth::ClientAuthMethod::parse(&state.method)
        .ok_or_else(|| format!("vaulted auth method {:?} is not recognized", state.method))?;

    let endpoints = oauth::discover(&state.resource).map_err(|e| {
        format!("could not verify the stored OAuth issuer before reusing the client secret: {e}")
    })?;
    let token_endpoint = issuer_bound_token_endpoint(&state.issuer, &endpoints)?;

    let block_private = oauth::host_of_url(token_endpoint)
        .map(|h| !oauth::host_is_definitely_private(&h))
        .unwrap_or(true);
    let tokens = oauth::client_credentials_token(
        token_endpoint,
        &state.client_id,
        &secret,
        method,
        state.scope.as_deref(),
        Some(&state.resource),
        block_private,
    )?;

    let next = ClientCredentialsState {
        token_endpoint: token_endpoint.to_string(),
        expires_at: tokens.expires_at,
        ..state
    };
    let json = serde_json::to_string(&next).map_err(|e| e.to_string())?;
    secrets::set_secret(server_id, CC_STATE_KEY, &json)?;
    secrets::set_secret(server_id, secrets::HTTP_AUTH_KEY, &tokens.access_token)?;
    Ok(RefreshedToken {
        access_token: tokens.access_token,
        expires_at: tokens.expires_at,
    })
}

/// Drop vaulted client-credentials state so the next connect re-acquires.
///
/// Called whenever the configuration changes. The state records the issuer,
/// method and scopes resolved at acquisition time, so leaving it in place after
/// an edit would keep minting tokens against the OLD configuration and the user's
/// change would appear to do nothing.
pub fn reset_client_credentials(server_id: &str) -> Result<(), String> {
    // Errors propagate. A failed delete leaves state that would keep minting
    // tokens under the OLD configuration, so reporting success here would tell
    // the user their change had taken effect when it had not. Deleting a key that
    // is not there is already `Ok` in every backend, so this does not fail on a
    // server being configured for the first time.
    secrets::delete_secret(server_id, CC_STATE_KEY)?;
    // The access token was minted under the previous configuration too.
    secrets::delete_secret(server_id, secrets::HTTP_AUTH_KEY)
}

/// Does the vaulted state name a different MCP URL than the one being connected?
///
/// Compared EXACTLY on the trimmed string, not case-insensitively. A URL path and
/// query are case-sensitive, so `/MCP` and `/mcp` are different resources; folding
/// case would let an edit between them keep a token that RFC 8707 bound to the old
/// one. Erring the other way is harmless: a comparison that reports "changed" when
/// only the scheme or host case differs just re-acquires, which is cheap and
/// non-interactive by construction.
///
/// An unreadable state counts as unchanged: the caller only uses this to decide
/// whether to discard state, and discarding on a parse failure would loop a broken
/// vault into re-acquiring on every connect.
///
/// Deliberately still `get_secret`, not `get_secret_result` (SBS-840 sweep). The sole
/// caller, [`client_credentials_state_is_stale`], reads the same key through
/// `get_secret_result` first, so the common failure is caught one frame up. Note this
/// narrows the window rather than closing it: that is a SECOND round trip to the vault,
/// so a backend that dies between the two calls still collapses to `None` here and skips
/// the reset. Left as-is because the window is one round trip wide and also needs the
/// user to have changed this server's URL; converting it means threading a `Result`
/// through a `bool` helper for that. Re-evaluate if the vault gets flakier.
fn client_credentials_resource_changed(server_id: &str, url: &str) -> bool {
    let Some(state) = secrets::get_secret(server_id, CC_STATE_KEY)
        .and_then(|s| serde_json::from_str::<ClientCredentialsState>(&s).ok())
    else {
        return false;
    };
    resource_binding_changed(&state.resource, url)
}

/// The comparison [`client_credentials_resource_changed`] is built on, split out so
/// the case-sensitivity rule is checkable without a vault round trip.
///
/// Deliberately NOT `eq_ignore_ascii_case`: see the doc comment above.
fn resource_binding_changed(vaulted_resource: &str, url: &str) -> bool {
    vaulted_resource.trim() != url.trim()
}

/// Is the vaulted client-credentials state stale for the entry being connected?
///
/// Split out of [`connect_remote_with_handler`] so the decision is reachable
/// without a live connect. Two ways state goes stale, both reached by editing the
/// server outside `set_client_credentials`: the config was removed, or the URL
/// changed out from under an RFC 8707 resource binding.
///
/// Errs on a failed vault read (SBS-840). Collapsing that to "no state here" skips
/// the reset, and the connect then sends a token RFC 8707 bound to the OLD resource
/// to the new one, which is the exact thing the reset exists to prevent.
fn client_credentials_state_is_stale(
    server: &ServerEntry,
    server_id: &str,
    url: &str,
) -> Result<bool, String> {
    if secrets::get_secret_result(server_id, CC_STATE_KEY)
        .map_err(|e| format!("could not read the vaulted client-credentials state: {e}"))?
        .is_none()
    {
        return Ok(false);
    }
    Ok(!uses_client_credentials(server) || client_credentials_resource_changed(server_id, url))
}

/// Expiry of the vaulted client-credentials token, if this server uses that flow.
///
/// Propagates a vault read failure (SBS-840) so a locked keychain cannot look
/// like "this server has no client-credentials state".
fn client_credentials_expiry(server_id: &str) -> Result<Option<u64>, String> {
    // A server that reports no lifetime keeps the reactive 401/403 behaviour,
    // matching the interactive flow. Returning 0 here would reacquire on every
    // single connect.
    Ok(load_cc_state(server_id)?.and_then(|state| state.expires_at))
}

/// Is this server configured for the headless flow?
fn uses_client_credentials(server: &ServerEntry) -> bool {
    server
        .client_credentials
        .as_ref()
        .is_some_and(|c| !c.client_id.trim().is_empty())
}

/// Use the stored refresh token to mint a fresh access token, vault it, and
/// return it.
/// Cross-process lock serializing programmatic refresh for one server.
///
/// The desktop app's health probe and the gateway are separate processes sharing one
/// keychain. Both could read RT0, both POST `/token`, and a provider with refresh-token
/// reuse detection revokes the whole family — the exact failure the in-process guard was
/// added to prevent, reached by another route (SBS-479). The app's existing OAuth lock
/// covers only the interactive browser flow.
///
/// The bounded wait does not have to cover every metadata request: on timeout we
/// reread the winner's saved token, or fail without exchanging. The OS releases
/// this advisory lock on process death; never unlink its file to recover a holder.
const OAUTH_REFRESH_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(65);
const OAUTH_REFRESH_LOCK_ERROR: &str =
    "OAuth refresh is busy or its cross-process lock is unavailable; try again.";
const OAUTH_REFRESH_SAVE_ERROR: &str = "OAuth refresh succeeded but could not save";
struct PendingRefresh {
    state: String,
    token: RefreshedToken,
}

#[derive(Default)]
struct CredentialState {
    pending: Option<PendingRefresh>,
    token: Option<String>,
    vaulted_access: Option<String>,
    vaulted_state: Option<String>,
    loaded: bool,
}

impl CredentialState {
    fn read_vault(&mut self, server_id: &str) -> Result<bool, String> {
        let access = secrets::get_secret_result(server_id, secrets::HTTP_AUTH_KEY)
            .map_err(|e| format!("could not read the vaulted access token: {e}"))?;
        let state = secrets::get_secret_result(server_id, STATE_KEY)
            .map_err(|e| format!("could not read the vaulted OAuth state: {e}"))?;
        let changed = self.loaded && (access != self.vaulted_access || state != self.vaulted_state);
        // A timed-out save may have committed our metadata without its bearer.
        let own_save = self.pending.as_ref().is_some_and(|pending| {
            state.as_deref() == Some(pending.state.as_str())
                && (access == self.vaulted_access
                    || access.as_deref() == Some(pending.token.access_token.as_str()))
        });
        if own_save {
            let pending = self.pending.take().expect("pending save matched");
            self.token = Some(pending.token.access_token.clone());
        } else if !self.loaded || changed {
            self.pending = None;
            self.token = access.clone();
        }
        self.vaulted_access = access;
        self.vaulted_state = state;
        self.loaded = true;
        if own_save {
            if let Some(token) = self.token.clone() {
                self.save_access(server_id, &token);
            }
        }
        Ok(changed && !own_save)
    }

    fn valid_pending_token(&self) -> Option<RefreshedToken> {
        self.pending
            .as_ref()
            .filter(|pending| {
                pending
                    .token
                    .expires_at
                    .is_none_or(|expiry| expiry > now_epoch_seconds())
            })
            .map(|pending| pending.token.clone())
    }

    fn save_access(&mut self, server_id: &str, token: &str) {
        self.token = Some(token.to_string());
        if secrets::set_secret(server_id, secrets::HTTP_AUTH_KEY, token).is_ok() {
            self.vaulted_access = Some(token.to_string());
        } else {
            eprintln!("OAuth refreshed access token could not be saved; using it in memory.");
        }
    }
}

type CredentialUpdate = Arc<Mutex<CredentialState>>;
type CredentialUpdates = HashMap<(Option<std::path::PathBuf>, String), CredentialUpdate>;

// Share pending rotations across connect, request and subscription refreshes.
// Only each server's mutex is held across I/O; unrelated servers stay independent.
fn credential_update(server_id: &str) -> CredentialUpdate {
    static UPDATES: OnceLock<Mutex<CredentialUpdates>> = OnceLock::new();
    UPDATES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry((crate::registry::conduit_dir(), server_id.to_string()))
        .or_default()
        .clone()
}

/// Credential to send: an unsaved in-memory token wins while the vault is
/// unchanged; a changed bearer or OAuth state replaces it and clears pending
/// rotation. Otherwise retain the last known token, including failed access saves.
/// Reads both bearer and OAuth metadata from the vault on every call (keychain I/O).
/// A locked/unreadable keychain returns `Err`, even with an in-memory token. Callers
/// must propagate that storage error, not interpret it as missing auth or force an exchange.
pub fn current_credential(server_id: &str) -> Result<Option<String>, String> {
    let update = credential_update(server_id);
    let mut state = update
        .lock()
        .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
    state.read_vault(server_id)?;
    Ok(state.token.clone())
}

/// A different current credential permits one retry after 401 without an exchange.
/// `None` means no replacement is available, including when auth was cleared.
/// Has the vault cost/error behavior of [`current_credential`]. Pass the rejected
/// token to [`refresh_token`] too: this lookup alone cannot serialize a later exchange.
pub fn newer_credential(server_id: &str, rejected: &str) -> Result<Option<String>, String> {
    Ok(current_credential(server_id)?.filter(|token| token != rejected))
}

fn lock_oauth_refresh_for(
    server_id: &str,
    timeout: std::time::Duration,
) -> Result<crate::registry::FileLock, String> {
    let dir = crate::registry::conduit_dir().ok_or(OAUTH_REFRESH_LOCK_ERROR)?;
    let leaf = format!(
        "oauth-refresh-{}",
        crate::router::sanitize_segment(&crate::local_auth::owner(server_id)?)
    );
    crate::registry::lock_at_for(&dir.join(leaf), timeout)
}

fn lock_oauth_refresh(server_id: &str) -> Result<crate::registry::FileLock, String> {
    lock_oauth_refresh_for(server_id, OAUTH_REFRESH_LOCK_TIMEOUT)
}

/// After waiting for the refresh lock, decide whether another process already did the work.
///
/// Compares the vaulted access token against the snapshot taken BEFORE the lock. Unchanged
/// means the refresh is still ours to do. Changed means someone rotated it while we were
/// parked, so we use theirs rather than spending a second exchange on a refresh token they
/// have already invalidated.
///
/// A rotated-but-already-expired token is not reusable, so that falls through and refreshes
/// normally.
fn refreshed_while_waiting(
    server_id: &str,
    before: Option<&str>,
) -> Result<Option<RefreshedToken>, String> {
    // A vault read failure is not "no token" / "no state" (SBS-840): pretending
    // there is nothing stored would fall through into a refresh that also lies.
    let current = match secrets::get_secret_result(server_id, secrets::HTTP_AUTH_KEY) {
        Ok(v) => v,
        Err(e) => return Err(format!("could not read the vaulted access token: {e}")),
    };
    let now = now_epoch_seconds();
    // Client-credentials servers keep their expiry under their own key and have no
    // OAuthState, so they need their own read. Without this a CC waiter would win the
    // lock and mint a second grant it did not need — serialized, so not a race, but a
    // redundant round trip to the token endpoint on every contended connect.
    if let Some(expires_at) = client_credentials_expiry(server_id)? {
        return Ok(reuse_racing_client_credentials(
            before, current, expires_at, now,
        ));
    }
    let state = load_state(server_id)?;
    Ok(reuse_racing_refresh(before, current, state.as_ref(), now))
}

/// [`reuse_racing_refresh`] for the client-credentials flow.
///
/// Same rule, different source of truth for expiry, and deliberately the same skew the
/// proactive CC path uses to decide a token is too close to its deadline — otherwise a
/// waiter could accept a token the very next connect would immediately replace.
fn reuse_racing_client_credentials(
    before: Option<&str>,
    current: Option<String>,
    expires_at: u64,
    now: u64,
) -> Option<RefreshedToken> {
    let current = current?;
    if Some(current.as_str()) == before {
        return None;
    }
    if now.saturating_add(PROACTIVE_REFRESH_SKEW_SECS) >= expires_at {
        return None;
    }
    Some(RefreshedToken {
        access_token: current,
        expires_at: Some(expires_at),
    })
}

/// The decision half of [`refreshed_while_waiting`], with the vault reads lifted out so
/// it is testable without writing to the developer's keychain.
///
/// Reuses the racing process's token only when it is both *different* from what we saw
/// before the lock and *usable* — the same `refresh_decision` the proactive path uses, so
/// the two cannot disagree about what "still good" means.
fn reuse_racing_refresh(
    before: Option<&str>,
    current: Option<String>,
    state: Option<&OAuthState>,
    now: u64,
) -> Option<RefreshedToken> {
    let current = current?;
    if Some(current.as_str()) == before {
        return None;
    }
    let state = state?;
    if refresh_decision(state, now) != RefreshDecision::NotNeeded {
        return None;
    }
    Some(RefreshedToken {
        access_token: current,
        expires_at: state.expires_at,
    })
}

fn refresh_token_with_expiry(
    server_id: &str,
    rejected: Option<&str>,
) -> Result<RefreshedToken, String> {
    refresh_token_with_lock(server_id, rejected, || lock_oauth_refresh(server_id))
}

fn refresh_token_with_lock(
    server_id: &str,
    rejected: Option<&str>,
    lock: impl FnOnce() -> Result<crate::registry::FileLock, String>,
) -> Result<RefreshedToken, String> {
    let update = credential_update(server_id);
    let mut pending = update
        .lock()
        .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
    refresh_token_with_pending(server_id, lock, &mut pending, true, rejected)
}

fn refresh_token_with_pending(
    server_id: &str,
    lock: impl FnOnce() -> Result<crate::registry::FileLock, String>,
    credentials: &mut CredentialState,
    force: bool,
    rejected: Option<&str>,
) -> Result<RefreshedToken, String> {
    let result = refresh_token_under_lock(server_id, lock, credentials, force, rejected);
    match result {
        Err(error) if !force && is_refresh_storage_or_lock_error(&error) => {
            credentials.valid_pending_token().ok_or(error)
        }
        result => result,
    }
}

fn refresh_token_under_lock(
    server_id: &str,
    lock: impl FnOnce() -> Result<crate::registry::FileLock, String>,
    credentials: &mut CredentialState,
    force: bool,
    rejected: Option<&str>,
) -> Result<RefreshedToken, String> {
    // Read before the OS lock so a peer's saved winner remains distinguishable.
    let had_pending = credentials.pending.is_some();
    let changed_before = credentials.read_vault(server_id)?;
    let before_access = credentials.vaulted_access.clone();
    // Held for the whole function, including the client-credentials branch, so two
    // processes cannot mint two tokens for the same server.
    let refresh_lock = lock();
    if let Some(winner) = refreshed_while_waiting(server_id, before_access.as_deref())?
        .filter(|winner| !force || rejected != Some(winner.access_token.as_str()))
    {
        credentials.read_vault(server_id)?;
        credentials.pending = None;
        credentials.token = Some(winner.access_token.clone());
        return Ok(winner);
    }
    // Even on timeout, a peer may have saved a usable winner just before our
    // reread. Otherwise no exchange is allowed without a held lock.
    let _refresh_lock = refresh_lock.map_err(|cause| {
        eprintln!("OAuth refresh lock unavailable: {cause}");
        OAUTH_REFRESH_LOCK_ERROR.to_string()
    })?;
    // Client-credentials servers have no refresh token by construction, so they
    // reacquire instead. Checked first because this is the seam BOTH the proactive
    // pre-expiry path and the reactive 401/403 retry go through; branching here
    // means neither has to know which flow a server uses.
    // A failed CC-state read must not fall through to interactive refresh (SBS-840).
    let changed_after = credentials.read_vault(server_id)?;
    let cc_state = load_cc_state(server_id)?;
    if force
        && rejected.is_some()
        && credentials
            .token
            .as_deref()
            .is_some_and(|token| Some(token) != rejected)
    {
        let expires_at = match cc_state.as_ref() {
            Some(state) => state.expires_at,
            None => decode_vaulted_json::<OAuthState>(
                Ok(credentials.vaulted_state.clone()),
                "OAuth state",
            )?
            .and_then(|state| state.expires_at),
        };
        return Ok(RefreshedToken {
            access_token: credentials.token.clone().expect("current token checked"),
            expires_at: credentials
                .pending
                .as_ref()
                .map(|pending| pending.token.expires_at)
                .unwrap_or(expires_at),
        });
    }
    if cc_state.is_some() {
        let token = reacquire_client_credentials(server_id)?;
        credentials.read_vault(server_id)?;
        credentials.token = Some(token.access_token.clone());
        return Ok(token);
    }
    let vaulted_state = credentials.vaulted_state.clone();
    if had_pending && (changed_before || changed_after || (!force && credentials.pending.is_none()))
    {
        // A peer's sign-in or our completed save supersedes the unsaved pair.
        if let Some(state) = load_state(server_id)? {
            if let Some(access_token) = credentials.token.clone() {
                if refresh_decision(&state, now_epoch_seconds()) == RefreshDecision::NotNeeded
                    && (!force || rejected != Some(access_token.as_str()))
                {
                    return Ok(RefreshedToken {
                        access_token,
                        expires_at: state.expires_at,
                    });
                }
            }
        }
    }
    let mut source_state = vaulted_state.clone();
    if let Some(p) = credentials.pending.as_ref() {
        source_state = Some(p.state.clone());
        let token = p.token.clone();
        let json = p.state.clone();
        if secrets::set_secret(server_id, STATE_KEY, &json).is_ok() {
            credentials.vaulted_state = Some(json);
            credentials.pending = None;
            credentials.save_access(server_id, &token.access_token);
        }
        // Retry saving even before the deadline; rejected/expired tokens still
        // exchange using the in-memory refresh token if storage remains unavailable.
        if !force
            && token
                .expires_at
                .is_none_or(|expiry| expiry > now_epoch_seconds())
        {
            return Ok(token);
        }
    }
    let state: OAuthState = decode_vaulted_json(Ok(source_state), "OAuth state")?
        .ok_or("no stored OAuth state to refresh")?;
    let rt = state
        .refresh_token
        .as_deref()
        .ok_or("no refresh token available")?;
    // Credentials minted under a known issuer may only be sent to endpoints from
    // that issuer's current validated metadata. If the MCP resource changes its
    // authorization server, fail closed so the UI asks the user to authenticate
    // and register a fresh client instead of reusing the old credentials.
    let refreshed_endpoints = match (state.issuer.as_deref(), state.resource.as_deref()) {
        (Some(expected_issuer), Some(resource)) => {
            let endpoints = oauth::discover(resource)
                .map_err(|e| format!("could not verify the stored OAuth issuer: {e}"))?;
            issuer_bound_token_endpoint(expected_issuer, &endpoints)?;
            Some(endpoints)
        }
        _ => None,
    };
    let token_endpoint = refreshed_endpoints
        .as_ref()
        .map(|e| e.token_endpoint.as_str())
        .unwrap_or(&state.token_endpoint);

    // Block a rebind to the internal network unless the token endpoint is itself a
    // local/LAN host (a self-hosted auth server). Fail closed (block) if the stored
    // endpoint host can't be parsed OR can't be positively confirmed local, so an
    // unresolvable stored endpoint stays screened rather than opening the guard (#422).
    let block_private = oauth::host_of_url(token_endpoint)
        .map(|h| !oauth::host_is_definitely_private(&h))
        .unwrap_or(true);
    let tokens = oauth::refresh(
        token_endpoint,
        &state.client_id,
        rt,
        state.resource.as_deref(),
        block_private,
    )?;
    // Persist rotated refresh metadata first. If replacing the access token then
    // fails, the next attempt still has the new refresh token and can recover;
    // the reverse order could strand a new access token with an invalidated old
    // refresh token after a second-write failure.
    let rotated = tokens
        .refresh_token
        .as_ref()
        .is_some_and(|rt| Some(rt) != state.refresh_token.as_ref());
    let new_state = OAuthState {
        issuer: state.issuer,
        token_endpoint: token_endpoint.to_string(),
        client_id: state.client_id,
        refresh_token: tokens.refresh_token.or(state.refresh_token),
        resource: state.resource,
        scope: state.scope,
        issued_at: Some(tokens.issued_at),
        expires_at: tokens.expires_at,
    };
    let json = serde_json::to_string(&new_state).map_err(|e| e.to_string())?;
    let token = RefreshedToken {
        access_token: tokens.access_token,
        expires_at: tokens.expires_at,
    };
    match secrets::set_secret(server_id, STATE_KEY, &json) {
        Ok(()) => {
            credentials.vaulted_state = Some(json.clone());
            credentials.pending = None;
            credentials.save_access(server_id, &token.access_token);
        }
        Err(_) => {
            eprintln!("OAuth refresh metadata could not be saved; will retry.");
            // An unchanged refresh token remains safe to retry from the vault.
            // Preserve a previously pending rotation even if this response omits it.
            if rotated || credentials.pending.is_some() {
                credentials.pending = Some(PendingRefresh {
                    state: json,
                    token: token.clone(),
                });
            } else {
                credentials.save_access(server_id, &token.access_token);
            }
        }
    }
    credentials.token = Some(token.access_token.clone());
    Ok(token)
}

/// Complete an interactive step-up flow for a runtime `insufficient_scope`
/// challenge. A fresh authorization (and client registration when needed) is
/// intentional here: refresh-token grants cannot obtain user consent for new
/// permissions. Persist the full new state before replacing the access token so
/// a partial keychain write cannot strand rotated credentials.
fn reauthorize_for_scope(
    server_id: &str,
    resource: &str,
    required_scope: &str,
) -> Result<RefreshedToken, String> {
    let previous =
        match load_state(server_id) {
            Ok(Some(s)) => s,
            Ok(None) => return Err(
                "saved OAuth state is unavailable; authenticate again to grant additional scope"
                    .to_string(),
            ),
            // A locked keychain is not "authenticate again" (SBS-840).
            Err(e) => return Err(e),
        };
    let requested = oauth::scope_union(previous.scope.as_deref(), Some(required_scope));
    let result = oauth::authenticate_with_scope(resource, requested.as_deref())?;
    store_oauth_state(
        server_id,
        Some(result.issuer),
        &result.token_endpoint,
        &result.client_id,
        result.refresh_token,
        Some(resource.to_string()),
        result.scope,
        result.issued_at,
        result.expires_at,
    )?;
    secrets::set_secret(server_id, secrets::HTTP_AUTH_KEY, &result.access_token)?;
    Ok(RefreshedToken {
        access_token: result.access_token,
        expires_at: result.expires_at,
    })
}

/// Force a refresh after rejection, or reuse a different current credential under
/// the credential-update and cross-process locks. `None` disables rejected-token
/// coalescing. Pass the bearer actually rejected by the server to coalesce
/// concurrent failures. Vault read errors propagate; callers
/// should report the storage failure rather than request sign-in or exchange again.
pub fn refresh_token(server_id: &str, rejected: Option<&str>) -> Result<String, String> {
    refresh_token_with_expiry(server_id, rejected).map(|token| token.access_token)
}

fn refresh_token_for_connect(server_id: &str) -> Result<Option<String>, String> {
    let update = credential_update(server_id);
    let mut pending = update
        .lock()
        .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
    let retry_pending = pending.valid_pending_token().is_some();
    connect_refresh_result(
        refresh_token_with_pending(
            server_id,
            || {
                if retry_pending {
                    lock_oauth_refresh_for(server_id, Duration::ZERO)
                } else {
                    lock_oauth_refresh(server_id)
                }
            },
            &mut pending,
            false,
            None,
        )
        .map(|token| token.access_token),
    )
}

/// Refresh before the known expiry. A legacy/provider state with no expiry is a
/// no-op and continues to use the 401/403 fallback. If the deadline is close but
/// no refresh token exists, return an auth-classified error so the existing
/// per-server "Needs sign-in" UI appears before a failed tool call.
fn refresh_token_if_needed(server_id: &str) -> Result<Option<String>, String> {
    if credential_update(server_id)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pending
        .is_some()
    {
        return refresh_token_for_connect(server_id);
    }
    // Same pre-expiry rule for the headless flow, minus the "no refresh token"
    // branch: reacquiring needs no user interaction, so a near-deadline token is
    // simply replaced rather than surfaced as "needs sign-in".
    if let Some(expires_at) = client_credentials_expiry(server_id)? {
        if now_epoch_seconds().saturating_add(PROACTIVE_REFRESH_SKEW_SECS) >= expires_at {
            // Through `refresh_token`, not `reacquire_client_credentials` directly: that
            // seam is where the cross-process lock lives, and calling the reacquire
            // straight from here left the proactive headless path as the one arm of the
            // call graph still able to mint concurrently (SBS-479). Matches the shape of
            // the refresh-token arm below.
            return refresh_token_for_connect(server_id);
        }
        return Ok(None);
    }
    // A vault read failure is not "no stored OAuth state" (SBS-840): skipping
    // refresh would treat a locked keychain as never-authenticated.
    let Some(state) = load_state(server_id)? else {
        return Ok(None);
    };
    match refresh_decision(&state, now_epoch_seconds()) {
        RefreshDecision::NotNeeded => Ok(None),
        // Report contention and persistence failures rather than silently using
        // an old credential and immediately attempting another exchange on 401.
        RefreshDecision::Refresh => refresh_token_for_connect(server_id),
        RefreshDecision::Reauthenticate => Err(
            "OAuth access token expires soon and no refresh token is available; needs authentication"
                .to_string(),
        ),
    }
}

fn connect_refresh_result(result: Result<String, String>) -> Result<Option<String>, String> {
    match result {
        Ok(token) => Ok(Some(token)),
        Err(e) if is_refresh_storage_or_lock_error(&e) || is_auth_error(&e) => Err(e),
        // Discovery and network failures need not prevent using a current token.
        Err(_) => Ok(None),
    }
}

/// True when `code` appears in `s` as a standalone number rather than as a run of
/// digits inside a longer one.
///
/// A bare substring test reads an auth failure out of an OS error number
/// (`os error 10401`), a port (`127.0.0.1:4013`), or a duration (`4030ms`).
fn mentions_status(s: &str, code: &str) -> bool {
    s.match_indices(code).any(|(i, _)| {
        let before = s[..i].chars().next_back();
        let after = s[i + code.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

pub(crate) fn is_refresh_storage_or_lock_error(e: &str) -> bool {
    is_refresh_lock_error(e)
        || e.starts_with(OAUTH_REFRESH_SAVE_ERROR)
        || e.contains("could not read the vaulted")
        || e.contains("could not parse the vaulted")
}

pub(crate) fn is_refresh_lock_error(e: &str) -> bool {
    e == OAUTH_REFRESH_LOCK_ERROR
}

pub fn is_auth_error(e: &str) -> bool {
    let lower = e.to_lowercase();
    mentions_status(e, "401")
        || mentions_status(e, "403")
        || lower.contains("unauthorized")
        || lower.contains("needs authentication")
}

/// A vaulted bearer token must not ride over cleartext to a public host. Allow
/// http only for loopback/private hosts (local dev on a trusted network); require
/// https for anything public, so the token can't be sniffed off the wire.
fn require_secure_for_auth(url: &str) -> Result<(), String> {
    if url.trim().to_ascii_lowercase().starts_with("https://") {
        return Ok(());
    }
    let host = oauth::host_of_url(url).unwrap_or_default();
    if oauth::host_is_definitely_private(&host) {
        return Ok(());
    }
    // Redact before interpolating: this message reaches the activity UI, client error
    // text, and any log that records the failure, and a URL of the form
    // `http://user:hunter2@host/mcp` would carry the password into all three.
    let shown = crate::registry::redact_url_userinfo(url);
    Err(format!(
        "refusing to send the saved auth token to a non-HTTPS URL ({shown}); \
         use https for an authenticated remote server"
    ))
}

/// Build an HTTP transport, refusing to attach a token to a cleartext public URL.
/// When authed, the transport gets a refresh callback: on a mid-session 401/403 it
/// mints a fresh access token from the stored refresh token and retries, so a
/// short-lived token expiring no longer breaks the session until reconnect.
fn authed_transport(
    url: &str,
    token: Option<String>,
    server_id: &str,
    block_private: bool,
    request_timeout: Duration,
) -> Result<(HttpTransport, Arc<AtomicBool>), String> {
    if token.is_some() {
        require_secure_for_auth(url)?;
    }
    // Shared by ordinary refresh and scope step-up so a newly-authorized token's
    // expiry replaces the previous token's proactive deadline immediately.
    // A failed state read must not silently disable proactive refresh (SBS-840).
    let oauth_state = load_state(server_id).or_else(|error| {
        let update = credential_update(server_id);
        let update = update
            .lock()
            .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
        if update.valid_pending_token().is_some() {
            decode_vaulted_json(
                Ok(update.pending.as_ref().map(|pending| pending.state.clone())),
                "OAuth state",
            )
        } else {
            Err(error)
        }
    })?;
    let refresh_at = oauth_state
        .as_ref()
        .and_then(|state| state.expires_at)
        .map(|expires_at| expires_at.saturating_sub(PROACTIVE_REFRESH_SKEW_SECS));
    let next_refresh_at = Arc::new(Mutex::new(refresh_at));
    // The request path and the background subscription listener can refresh or
    // step up concurrently. Serialize credential-changing flows so an older
    // refresh result cannot overwrite a newer interactive authorization state.
    let credential_update = credential_update(server_id);
    let refreshed_during_connect = Arc::new(AtomicBool::new(false));
    let refresh: Option<RefreshFn> = if token.is_some() {
        let sid = server_id.to_string();
        // Keep the proactive deadline in memory. This avoids a keychain read on
        // every tool call while still updating the deadline after each refresh.
        let next_refresh_at = Arc::clone(&next_refresh_at);
        let credential_update = Arc::clone(&credential_update);
        let refreshed_during_connect = Arc::clone(&refreshed_during_connect);
        Some(Box::new(move |force, rejected| {
            let deadline = *next_refresh_at
                .lock()
                .map_err(|_| "OAuth refresh deadline lock poisoned".to_string())?;
            let valid_token = deadline.is_none_or(|deadline| {
                deadline.saturating_add(PROACTIVE_REFRESH_SKEW_SECS) > now_epoch_seconds()
            });
            let mut update = if !force && valid_token {
                match credential_update.try_lock() {
                    Ok(update) => update,
                    Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
                    Err(_) => return Err("OAuth credential-update lock poisoned".into()),
                }
            } else {
                credential_update
                    .lock()
                    .map_err(|_| "OAuth credential-update lock poisoned".to_string())?
            };
            if !force && update.pending.is_none() {
                let deadline = *next_refresh_at
                    .lock()
                    .map_err(|_| "OAuth refresh deadline lock poisoned".to_string())?;
                match deadline {
                    Some(refresh_at) if now_epoch_seconds() >= refresh_at => {}
                    _ => return Ok(None),
                }
            }

            let retry_pending = !force && update.valid_pending_token().is_some();
            let refreshed = match refresh_token_with_pending(
                &sid,
                || {
                    if retry_pending {
                        lock_oauth_refresh_for(&sid, Duration::ZERO)
                    } else {
                        lock_oauth_refresh(&sid)
                    }
                },
                &mut update,
                force,
                rejected,
            ) {
                Ok(refreshed) => refreshed,
                Err(e) => {
                    if !force {
                        *next_refresh_at
                            .lock()
                            .map_err(|_| "OAuth refresh deadline lock poisoned".to_string())? =
                            Some(now_epoch_seconds().saturating_add(PROACTIVE_REFRESH_RETRY_SECS));
                    }
                    return Err(e);
                }
            };
            refreshed_during_connect.store(true, Ordering::SeqCst);
            let deadline = refreshed
                .expires_at
                .map(|expires_at| expires_at.saturating_sub(PROACTIVE_REFRESH_SKEW_SECS));
            *next_refresh_at
                .lock()
                .map_err(|_| "OAuth refresh deadline lock poisoned".to_string())? = deadline;
            Ok(Some(refreshed.access_token))
        }))
    } else {
        None
    };
    let scope_reauthorize: Option<ScopeReauthorizeFn> = if token.is_some() && oauth_state.is_some()
    {
        let sid = server_id.to_string();
        let resource = url.to_string();
        let next_refresh_at = Arc::clone(&next_refresh_at);
        let credential_update = Arc::clone(&credential_update);
        Some(Box::new(move |scope| {
            let mut update = credential_update
                .lock()
                .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
            let token = reauthorize_for_scope(&sid, &resource, scope)?;
            update.pending = None;
            update.read_vault(&sid)?;
            update.token = Some(token.access_token.clone());
            let deadline = token
                .expires_at
                .map(|expires_at| expires_at.saturating_sub(PROACTIVE_REFRESH_SKEW_SECS));
            *next_refresh_at
                .lock()
                .map_err(|_| "OAuth refresh deadline lock poisoned".to_string())? = deadline;
            Ok(token.access_token)
        }))
    } else {
        None
    };
    // The resolver enforces the SSRF policy at connect time (DNS-rebind safe); it
    // mirrors `guard_connect_target`: link-local/metadata blocked for all, private
    // blocked only for untrusted-provenance servers.
    let mut transport =
        HttpTransport::guarded_with_timeout(url, token, refresh, block_private, request_timeout);
    transport.set_scope_reauthorize(scope_reauthorize);
    // Declared per request only while the flow is actually in use, which is what
    // the extension requires. Keyed off vaulted state rather than registry config
    // so it is true of the credential actually being sent: a server configured for
    // the flow but not yet provisioned has nothing to declare.
    //
    // Deliberately still `get_secret`, not `get_secret_result`. A read failure here
    // only omits an informational extension declaration: no credential is minted,
    // sent, or overwritten, and the request itself is unaffected. Failing the whole
    // transport build over a missing declaration would be worse than the omission
    // (SBS-840 sweep).
    if secrets::get_secret(server_id, CC_STATE_KEY).is_some() {
        transport.declare_extension(
            crate::downstream::OAUTH_CLIENT_CREDENTIALS_EXTENSION,
            serde_json::json!({}),
        );
    }
    Ok((transport, refreshed_during_connect))
}

/// Provenance Toolport doesn't trust to point at the user's private network. Shared
/// imports (`"shared"`) and public-registry entries (`"registry"`) are
/// attacker-influenceable; user-added, client-imported, curated-catalog, and team
/// servers are not, so their local URLs (e.g. a localhost MCP server) still connect.
fn is_untrusted_source(source: Option<&str>) -> bool {
    matches!(source, Some("shared") | Some("registry"))
}

/// True if `host` is a link-local / cloud-metadata literal or a name resolving
/// to one. Covers IPv4 `169.254.x`, IPv6 `fe80::/10`, IPv4-mapped forms, and the
/// AWS IPv6 metadata address `fd00:ec2::254` (see `oauth::ip_is_link_local`).
/// `169.254.169.254` and its IPv6 peers are the classic SSRF target for stealing
/// cloud credentials.
fn host_is_link_local(host: &str) -> bool {
    use std::net::{IpAddr, ToSocketAddrs};
    let h = host.trim();
    if let Ok(ip) = h.parse::<IpAddr>() {
        return oauth::ip_is_link_local(&ip);
    }
    (h, 0u16)
        .to_socket_addrs()
        .map(|addrs| addrs.map(|a| a.ip()).any(|ip| oauth::ip_is_link_local(&ip)))
        .unwrap_or(false)
}

/// SSRF guard run before connecting to a remote server. Link-local / cloud-metadata
/// is refused for EVERY server (never a valid MCP target, and the classic way to
/// steal cloud credentials). Other private/loopback hosts are refused only for
/// untrusted-provenance servers, so the user's own localhost server still works.
fn guard_connect_target(server: &ServerEntry) -> Result<(), String> {
    let host = oauth::host_of_url(server.url.as_deref().unwrap_or("")).unwrap_or_default();
    if host_is_link_local(&host) {
        return Err(format!(
            "Toolport refused to connect to {host}: link-local / cloud-metadata addresses \
             (169.254.x) are never a valid MCP server and are a common SSRF target."
        ));
    }
    if is_untrusted_source(server.source.as_deref()) && oauth::host_is_private(&host) {
        return Err(format!(
            "Toolport refused to connect \"{}\" to the private address {host}: it came from \
             an untrusted source ({}). If you trust it, add the server yourself.",
            server.name,
            server.source.as_deref().unwrap_or("unknown")
        ));
    }
    Ok(())
}

/// The first custom secret env var that has a value vaulted in the keychain.
/// For HTTP servers that don't use OAuth (e.g. Magica with a `BEARER` API key),
/// this is the token we send as `Authorization: Bearer ***`.
/// Errs on a failed vault read (SBS-789) — this fallback is the ONLY token
/// source for such servers, so swallowing the error here would connect
/// anonymous exactly like the `HTTP_AUTH_KEY` path used to.
fn first_vaulted_secret(server: &ServerEntry) -> Result<Option<String>, String> {
    for e in &server.env {
        if e.secret && e.value.is_none() && e.key != secrets::IMPORTED_URL_KEY {
            if let Some(v) = secrets::get_secret_result(&server.id, &e.key)? {
                return Ok(Some(v));
            }
        }
    }
    Ok(None)
}

/// Did the transport spend its own forced refresh during this connect?
///
/// The transport force-refreshes internally on a 401/403 and vaults the result, so a
/// vaulted token that differs from the one we handed it means an exchange already
/// happened (SOU-474). `sent_auth` is what went out; `None` back means there is
/// nothing vaulted to compare, which is not evidence of a refresh.
///
/// Errs on a failed vault read (SBS-840) rather than answering "no refresh happened".
/// That answer sends the caller into a second exchange on a refresh token the
/// transport may have already spent, which is what a provider with reuse detection
/// revokes the whole family over.
fn transport_refreshed_during_connect(
    server_id: &str,
    sent_auth: Option<&str>,
) -> Result<bool, String> {
    let vaulted = secrets::get_secret_result(server_id, secrets::HTTP_AUTH_KEY)
        .map_err(|e| format!("could not read the vaulted access token: {e}"))?;
    Ok(vaulted.is_some_and(|vaulted| Some(vaulted.as_str()) != sent_auth))
}

/// Connect to a remote server, injecting any vaulted token. On an auth error,
/// refresh the token once and retry.
///
/// Token lookup order for HTTP servers:
/// 1. `__http_auth__` — the key used by the OAuth flow and the "paste token" UI.
/// 2. The first vaulted custom secret env var (e.g. `BEARER`) — for servers like
///    Magica that declare a manual API-key env var in the registry but don't use
///    OAuth. Without this fallback, "Manage secrets" tokens were silently ignored
///    for HTTP servers.
pub fn connect_remote(server: &ServerEntry) -> Result<DownstreamServer, String> {
    connect_remote_with_handler(server, None, None, None, None)
}

/// Like [`connect_remote`], but wires server-initiated JSON-RPC (sampling, roots, …)
/// through `handler` when the downstream server asks mid-call, and optionally fans
/// `notifications/resources/updated` from SSE response streams (SOU-394), and
/// routes `notifications/progress` back to the client that minted the token
/// (SOU-444).
pub fn connect_remote_with_handler(
    server: &ServerEntry,
    server_handler: Option<ServerRequestHandler>,
    resource_updated: Option<ResourceUpdatedSink>,
    progress: Option<ProgressSink>,
    change_dirty: Option<Arc<AtomicU8>>,
) -> Result<DownstreamServer, String> {
    let (resolved, header_values) =
        crate::secret_refs::resolve_connection(server).map_err(|e| e.to_string())?;
    let server = &resolved;
    let imported = crate::import_credentials::has_imported_url(server);
    if !imported {
        return connect_remote_inner(
            server,
            header_values,
            server_handler,
            resource_updated,
            progress,
            change_dirty,
        )
        .map_err(|error| {
            if is_auth_error(&error) {
                crate::secret_refs::invalidate_server(server);
            }
            safe_imported_error(server, error)
        });
    }
    let url = secrets::get_vault_secret_result(&server.id, secrets::IMPORTED_URL_KEY)
        .map_err(|_| "Keychain unavailable. Unlock it and retry.")?
        .ok_or("Missing imported endpoint. Import its native definition again.")?;
    if server.url.as_deref() != Some(&crate::import_credentials::shown_url(&url)) {
        return Err("The endpoint changed. Review and import its credentials again.".into());
    }
    let mut resolved = server.clone();
    resolved.url = Some(url);
    connect_remote_inner(
        &resolved,
        header_values,
        server_handler,
        resource_updated,
        progress,
        change_dirty,
    )
    .map_err(|error| safe_imported_error(&resolved, error))
}

/// Keep provider errors from echoing a credential-bearing endpoint after connect.
struct ImportedTransport(Box<dyn Transport>, Redaction, bool);
struct ImportedConcurrent(
    Arc<dyn crate::downstream::ConcurrentTransport>,
    Redaction,
    bool,
);
#[derive(Clone)]
struct Redaction(Vec<String>, Vec<String>);
impl Redaction {
    fn for_server(server: &ServerEntry) -> Self {
        let mut values = Vec::new();
        if let Some(url) = &server.url {
            values.push(url.clone());
            if let Ok(parsed) = url::Url::parse(url) {
                let decoded = |value: &str| {
                    url::form_urlencoded::parse(format!("v={value}").as_bytes())
                        .next()
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default()
                };
                let mut add = |value: &str| {
                    values.push(value.into());
                    values.push(decoded(value));
                };
                add(parsed.username());
                if let Some(password) = parsed.password() {
                    add(password);
                }
                for pair in parsed.query().unwrap_or("").split('&') {
                    if let Some((key, value)) = pair.split_once('=') {
                        if crate::import_credentials::secret_url_name(key)
                            || decoded(value).len() >= 4
                        {
                            add(value);
                        }
                    }
                }
                let mut after_secret_name = false;
                for segment in parsed.path_segments().into_iter().flatten() {
                    let value = decoded(segment);
                    if after_secret_name
                        || crate::registry::arg_looks_secret(&value)
                        || crate::import_credentials::secret_env("", Some(&value))
                    {
                        add(segment);
                    }
                    after_secret_name = crate::import_credentials::secret_url_name(&value);
                }
            }
        }
        for env in server.env.iter().filter(|e| e.secret) {
            if let Some(value) = env.value.clone().or_else(|| {
                secrets::get_secret_result(&server.id, &env.key)
                    .ok()
                    .flatten()
            }) {
                values.push(value);
            }
        }
        for input in server
            .launch
            .iter()
            .flat_map(|launch| &launch.inputs)
            .filter(|input| input.secret)
        {
            if let Some(value) = input.value.clone().or_else(|| {
                secrets::get_vault_secret_result(&server.id, &input.key)
                    .ok()
                    .flatten()
            }) {
                values.push(value);
            }
        }
        if crate::import_credentials::has_imported_url(server) {
            if let Ok(Some(value)) =
                secrets::get_vault_secret_result(&server.id, secrets::IMPORTED_URL_KEY)
            {
                values.push(value);
            }
        }
        values.retain(|value| !value.is_empty());
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        Self(values, crate::secret_refs::review_references(server))
    }
    fn text(&self, mut message: String) -> String {
        for value in &self.0 {
            if value.len() < 4 {
                let token = regex::Regex::new(&format!(r"\b{}\b", regex::escape(value))).unwrap();
                message = token.replace_all(&message, "<redacted>").into_owned();
            } else {
                message = message.replace(value, "<redacted>");
            }
        }
        message
    }
    fn value(&self, value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(value) => serde_json::Value::String(self.text(value)),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(|v| self.value(v)).collect())
            }
            serde_json::Value::Object(values) => serde_json::Value::Object(
                values
                    .into_iter()
                    .map(|(k, v)| (self.text(k), self.value(v)))
                    .collect(),
            ),
            value => value,
        }
    }
    fn connection_error(
        &self,
        error: crate::downstream::TransportError,
        reference: bool,
    ) -> crate::downstream::TransportError {
        if !reference {
            return self.error(error);
        }
        use crate::call_failure::CallFailureKind as K;
        use crate::downstream::TransportError as E;
        let kind = error.call_failure().kind;
        // An auth rejection invalidates this connection, so its supervisor reads the
        // reference again. Never replay a possibly completed write automatically.
        if matches!(kind, K::Auth { .. }) {
            for reference in &self.1 {
                crate::secret_refs::invalidate(reference);
            }
            return E::Classified(
                K::Unavailable { after_send: true },
                "Password manager credential rejected. Reconnecting will read the key again."
                    .into(),
            );
        }
        // Child tails can contain a fragment or an escaped credential. Use fixed
        // text for reference-backed errors while retaining their retry classification.
        let message = match error {
            E::Retry { retry_after, .. } => {
                return E::Retry {
                    retry_after,
                    message: "Reference-backed server asked to retry.".into(),
                }
            }
            E::RateLimited { retry_after, .. } => {
                return E::RateLimited {
                    retry_after,
                    message: "Reference-backed server rate limited the request.".into(),
                }
            }
            E::Fatal(_) => {
                return E::Unavailable("Reference-backed server connection closed.".into())
            }
            E::Rpc(ref value) => {
                return E::Rpc(
                    serde_json::json!({"code": value.get("code").and_then(serde_json::Value::as_i64).unwrap_or(-32603), "message": "Reference-backed server rejected the request."}),
                )
            }
            _ => "Reference-backed server request failed.",
        };
        E::Classified(kind, message.into())
    }
    fn error(&self, error: crate::downstream::TransportError) -> crate::downstream::TransportError {
        use crate::downstream::TransportError as E;
        match error {
            E::Classified(kind, message) => E::Classified(kind, self.text(message)),
            E::Fatal(message) => E::Fatal(self.text(message)),
            E::FrameRejected(message) => E::FrameRejected(self.text(message)),
            E::Unavailable(message) => E::Unavailable(self.text(message)),
            E::RateLimited {
                retry_after,
                message,
            } => E::RateLimited {
                retry_after,
                message: self.text(message),
            },
            E::Retry {
                retry_after,
                message,
            } => E::Retry {
                retry_after,
                message: self.text(message),
            },
            E::Cancelled(message) => E::Cancelled(self.text(message)),
            E::Busy(message) => E::Busy(self.text(message)),
            E::Rpc(value) => E::Rpc(self.value(value)),
        }
    }
}
impl crate::downstream::ConcurrentTransport for ImportedConcurrent {
    fn request_with_cancel_and_headers(
        &self,
        method: &str,
        params: serde_json::Value,
        cancel: Option<crate::downstream::CancelContext>,
        headers: &[(String, String)],
    ) -> Result<serde_json::Value, crate::downstream::TransportError> {
        self.0
            .request_with_cancel_and_headers(method, params, cancel, headers)
            .map(|value| if self.2 { self.1.value(value) } else { value })
            .map_err(|error| self.1.connection_error(error, self.2))
    }
    fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
    fn suspended_calls(&self) -> usize {
        self.0.suspended_calls()
    }
}
impl Transport for ImportedTransport {
    fn response_count(&self) -> u64 {
        self.0.response_count()
    }
    fn connection_reset_reason(&self) -> Option<String> {
        self.0.connection_reset_reason().map(|error| {
            if self.2 {
                "Reference-backed connection closed. Restart to read its key again.".into()
            } else {
                self.1.text(error)
            }
        })
    }

    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, crate::downstream::TransportError> {
        self.0
            .request(method, params)
            .map(|value| if self.2 { self.1.value(value) } else { value })
            .map_err(|error| self.1.connection_error(error, self.2))
    }
    fn notify(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), crate::downstream::TransportError> {
        self.0
            .notify(method, params)
            .map_err(|error| self.1.connection_error(error, self.2))
    }
    fn request_with_cancel(
        &mut self,
        method: &str,
        params: serde_json::Value,
        cancel: Option<crate::downstream::CancelContext>,
    ) -> Result<serde_json::Value, crate::downstream::TransportError> {
        self.0
            .request_with_cancel(method, params, cancel)
            .map(|value| if self.2 { self.1.value(value) } else { value })
            .map_err(|error| self.1.connection_error(error, self.2))
    }
    fn request_with_cancel_and_headers(
        &mut self,
        method: &str,
        params: serde_json::Value,
        cancel: Option<crate::downstream::CancelContext>,
        headers: &[(String, String)],
    ) -> Result<serde_json::Value, crate::downstream::TransportError> {
        self.0
            .request_with_cancel_and_headers(method, params, cancel, headers)
            .map(|value| if self.2 { self.1.value(value) } else { value })
            .map_err(|error| self.1.connection_error(error, self.2))
    }
    fn cancel_matching_pending_request(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cancel: &crate::downstream::CancelContext,
    ) -> bool {
        self.0
            .cancel_matching_pending_request(method, params, cancel)
    }
    fn set_protocol_meta(&mut self, meta: Option<serde_json::Value>) {
        self.0.set_protocol_meta(meta)
    }
    fn set_subscription_listener(
        &mut self,
        filter: crate::downstream::SubscriptionFilter,
    ) -> Result<(), crate::downstream::TransportError> {
        self.0
            .set_subscription_listener(filter)
            .map_err(|error| self.1.connection_error(error, self.2))
    }
    fn supports_request_headers(&self) -> bool {
        self.0.supports_request_headers()
    }
    fn set_read_timeout(&mut self, timeout: Duration) {
        self.0.set_read_timeout(timeout)
    }
    fn connect_timeout(&self) -> Duration {
        self.0.connect_timeout()
    }
    fn initialize_complete(&mut self) {
        self.0.initialize_complete()
    }
    fn arm_tools_watch(&mut self) {
        self.0.arm_tools_watch()
    }
    fn set_server_request_handler(&mut self, handler: ServerRequestHandler) {
        if self.2 {
            let redact = self.1.clone();
            self.0.set_server_request_handler(Arc::new(move |frame| {
                handler(&redact.value(frame.clone()))
            }));
        } else {
            self.0.set_server_request_handler(handler)
        }
    }
    fn set_server_id(&mut self, id: &str) {
        self.0.set_server_id(id)
    }
    fn concurrent(&self) -> Option<Arc<dyn crate::downstream::ConcurrentTransport>> {
        self.0.concurrent().map(|transport| {
            Arc::new(ImportedConcurrent(transport, self.1.clone(), self.2))
                as Arc<dyn crate::downstream::ConcurrentTransport>
        })
    }
    fn connection_closed(&self) -> Option<bool> {
        self.0.connection_closed()
    }
    fn suspended_calls(&self) -> usize {
        self.0.suspended_calls()
    }
}
fn reviewed_transport(server: &ServerEntry, transport: HttpTransport) -> Box<dyn Transport> {
    protect_transport(server, Box::new(transport))
}

/// Share the existing credential redaction with stdio reference-backed connections.
pub fn protect_transport(
    server: &ServerEntry,
    transport: Box<dyn Transport>,
) -> Box<dyn Transport> {
    if has_imported_credentials(server) || crate::secret_refs::has_references(server) {
        Box::new(ImportedTransport(
            transport,
            Redaction::for_server(server),
            crate::secret_refs::has_references(server),
        ))
    } else {
        transport
    }
}

fn safe_imported_error(server: &ServerEntry, error: String) -> String {
    if !has_imported_credentials(server) && !crate::secret_refs::has_references(server) {
        return error;
    }
    Redaction::for_server(server).text(error)
}

fn has_imported_credentials(server: &ServerEntry) -> bool {
    crate::import_credentials::has_secrets(server)
}

fn connect_remote_inner(
    server: &ServerEntry,
    mut header_values: Vec<(String, String)>,
    server_handler: Option<ServerRequestHandler>,
    resource_updated: Option<ResourceUpdatedSink>,
    progress: Option<ProgressSink>,
    change_dirty: Option<Arc<AtomicU8>>,
) -> Result<DownstreamServer, String> {
    guard_connect_target(server)?;
    let legacy_bearer = crate::secret_refs::take_legacy_bearer(server, &mut header_values);
    let mut server_with_headers = server.clone();
    for (key, value) in &header_values {
        server_with_headers.env.push(crate::registry::EnvVar {
            key: key.clone(),
            value: Some(value.clone()),
            secret: true,
            unknown_fields: Default::default(),
        });
    }
    let server = &server_with_headers;
    let url = server.url.as_deref().unwrap_or("");
    let server_id = &server.id;
    // Untrusted-provenance servers also get private/loopback refused at the resolver,
    // matching `guard_connect_target`'s pre-check but closing the DNS-rebind TOCTOU.
    let block_private = is_untrusted_source(server.source.as_deref());
    let request_timeout = request_timeout(server)?;
    let initialize_timeout = server.initialize_timeout()?.unwrap_or(request_timeout);
    // First connect for a headless server: mint a token now. Only this path has the
    // registry config (client id, method, scopes); every later reacquisition runs
    // from the state vaulted here, which is why it can go through the shared seam
    // with just a server id.
    // Vaulted state that no longer matches the entry. Two cases, both reached by
    // editing the server outside `set_client_credentials`:
    //
    //   * the config was removed (e.g. registry.json edited with the app closed),
    //     which would otherwise keep the headless flow alive for a server no
    //     longer configured for it;
    //   * the URL changed, which matters more. The vaulted state pins `resource`,
    //     and the token is bound to it via RFC 8707, so reusing it would present a
    //     credential minted for the OLD resource to the new one. Reset instead, so
    //     the next acquisition binds to the URL actually being contacted.
    //
    // Handled here because this is the only place that sees both the current entry
    // and the vault; the reacquire seam takes just a server id by design.
    if !crate::secret_refs::has_references(server) {
        let stale_cc =
            client_credentials_state_is_stale(server, server_id, url).or_else(|error| {
                let update = credential_update(server_id);
                let update = update
                    .lock()
                    .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
                if !uses_client_credentials(server) && update.valid_pending_token().is_some() {
                    Ok(false)
                } else {
                    Err(error)
                }
            })?;
        if stale_cc {
            // Not ignored: leaving stale state would silently keep using the wrong
            // flow, or the wrong resource binding, for the rest of the session.
            reset_client_credentials(server_id)?;
        }
        // A failed CC-state read is not "no state" (SBS-840): do not mint a second grant.
        if uses_client_credentials(server) && load_cc_state(server_id)?.is_none() {
            let config = server
                .client_credentials
                .as_ref()
                .expect("uses_client_credentials checked it");
            acquire_client_credentials(server_id, url, config)?;
        }
    }
    // A vault read failure is not "no token" (SBS-789): connecting anonymous on a
    // locked keychain would surface as a bogus 401/"needs sign-in" and can hand an
    // unauthenticated session to a server the user believes is authenticated.
    let reference_auth = server
        .env
        .iter()
        .find(|e| e.unknown_fields.contains_key("source") && e.secret)
        .and_then(|e| e.value.clone());
    let auth = if crate::secret_refs::has_references(server) && !header_values.is_empty() {
        None
    } else if reference_auth.is_some() {
        reference_auth.clone()
    } else {
        match refresh_token_if_needed(server_id)? {
            Some(fresh) => Some(fresh),
            None => match current_credential(server_id)? {
                Some(token) => Some(token),
                None => match legacy_bearer {
                    Some(token) => Some(token),
                    None => first_vaulted_secret(server)
                        .map_err(|e| format!("could not read the vaulted auth token: {e}"))?,
                },
            },
        }
        // Remember exactly what we hand the transport. The transport force-refreshes
        // internally on a 401/403 and vaults the result, so if the vaulted token
        // differs from this afterwards, an exchange already happened during this
        // connect (SOU-474).
    };
    let sent_auth = auth.clone();
    let (mut transport, refreshed_during_connect) = if reference_auth.is_some()
        || crate::secret_refs::has_references(server)
    {
        require_secure_for_auth(url)?;
        (
            HttpTransport::guarded_with_timeout(url, auth, None, block_private, request_timeout),
            Arc::new(AtomicBool::new(false)),
        )
    } else {
        authed_transport(url, auth, server_id, block_private, request_timeout)?
    };
    if !header_values.is_empty() {
        require_secure_for_auth(url)?;
    }
    transport.set_credential_headers(header_values.clone())?;
    transport.set_reference_credentials(crate::secret_refs::has_references(server));
    transport.set_connect_timeout(initialize_timeout);
    if let Some(ref handler) = server_handler {
        transport.set_server_request_handler(protect_server_requests(server, handler.clone()));
    }
    transport.set_resource_updated_sink(protect_resource_updates(server, resource_updated.clone()));
    transport.set_progress_sink(protect_progress(server, progress.clone()));
    transport.set_change_sink(change_dirty.clone());
    match DownstreamServer::connect(server_id.to_string(), reviewed_transport(server, transport))
        .map_err(|e| safe_imported_error(server, e))
    {
        Ok(mut ds) => {
            ds.set_call_timeout(request_timeout);
            Ok(ds)
        }
        Err(e) if is_auth_error(&e) && !crate::secret_refs::has_references(server) => {
            // The transport already gets one forced refresh per token on a 401/403.
            // If it spent one during this connect, the vault now holds a token that
            // has ALREADY been rejected, so minting yet another cannot help - and
            // against a provider that rotates the refresh token on use, each needless
            // exchange consumes a further link of the chain. Retry only when the
            // transport had no refresh of its own to spend (SOU-474).
            //
            // The two auth-error arms are one arm so that check can use `?`: a match
            // guard cannot, so it could only answer "no refresh happened" on a failed
            // vault read and go on to refresh anyway (SBS-840). The rejection is then
            // described in words rather than quoted, because its text would make
            // `is_auth_error` classify a keychain fault as needs-sign-in and push the
            // user into a sign-in the same vault could not store.
            // A successful refresh can live only in memory after a vault write
            // fails. Count it directly instead of relying solely on saved tokens.
            let already_refreshed = if refreshed_during_connect.load(Ordering::SeqCst) {
                true
            } else {
                match transport_refreshed_during_connect(server_id, sent_auth.as_deref()) {
                    Ok(refreshed) => refreshed,
                    Err(vault_error) => {
                        // Keep the downstream rejection out of the returned string but
                        // not out of the record of what happened.
                        eprintln!(
                            "toolport: could not tell whether the transport refreshed during the \
                             failed connect to {server_id:?}; the server rejected the credential \
                             with: {e}"
                        );
                        return Err(format!(
                            "{vault_error} (the server rejected the credential Toolport sent, and \
                             without the vault there is no way to tell whether it had already been \
                             renewed, so no further token exchange was attempted)"
                        ));
                    }
                }
            };
            if already_refreshed {
                return Err(e);
            }
            match refresh_token(server_id, sent_auth.as_deref()) {
                Ok(fresh) => {
                    let (mut transport, _) = authed_transport(
                        url,
                        Some(fresh),
                        server_id,
                        block_private,
                        request_timeout,
                    )?;
                    transport.set_credential_headers(header_values.clone())?;
                    transport.set_connect_timeout(initialize_timeout);
                    if let Some(handler) = server_handler.clone() {
                        transport.set_server_request_handler(handler);
                    }
                    transport.set_resource_updated_sink(resource_updated);
                    transport.set_progress_sink(progress);
                    transport.set_change_sink(change_dirty);
                    DownstreamServer::connect(
                        server_id.to_string(),
                        reviewed_transport(server, transport),
                    )
                    .map(|mut ds| {
                        ds.set_call_timeout(request_timeout);
                        ds
                    })
                }
                Err(refresh_error) if is_refresh_storage_or_lock_error(&refresh_error) => {
                    Err(refresh_error)
                }
                Err(_) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_authorization_sends_bearer_or_nothing_without_oauth() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            let mut server = remote_server(&endpoint.url, None);
            server.unknown_fields.insert("headerKeys".into(), serde_json::json!([{"key":"Authorization","env":"AUTH"}]));
            for (saved, expected) in [(None,""),(Some("bare-token"),"Bearer bare-token"),(Some("Bearer x"),"Bearer x")] {
                if let Some(value)=saved {secrets::set_secret(&server.id,"AUTH",value).unwrap();}
                let mut connection=connect_remote(&server).unwrap();
                assert_eq!(connection.call("fixture",serde_json::json!({})).unwrap()["authorization"],expected);
            }
            assert_eq!(endpoint.count(),0);
        });
    }
    #[test]
    fn legacy_authorization_keeps_oauth_and_allows_oauth_only_members() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            let mut state = load_state("rotation").unwrap().unwrap();
            state.expires_at = Some(now_epoch_seconds() + 3600);
            secrets::set_secret(
                "rotation",
                STATE_KEY,
                &serde_json::to_string(&state).unwrap(),
            )
            .unwrap();
            let mut server = remote_server(&endpoint.url, None);
            server.id = "rotation".into();
            server.unknown_fields.insert(
                "headerKeys".into(),
                serde_json::json!([{"key":"Authorization","env":"AUTH"}]),
            );
            for saved in [None, Some("bare-token"), Some("Bearer x")] {
                if let Some(value) = saved {
                    secrets::set_secret("rotation", "AUTH", value).unwrap();
                }
                let mut connection = connect_remote(&server).unwrap();
                assert_eq!(
                    connection.call("fixture", serde_json::json!({})).unwrap()["authorization"],
                    "Bearer token-0"
                );
            }
            assert_eq!(endpoint.count(), 0);
        });
    }
    #[test]
    fn legacy_team_headers_keep_oauth_bearer_and_refresh_callback() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            let mut state = load_state("rotation").unwrap().unwrap();
            state.expires_at = Some(now_epoch_seconds() + 3600);
            secrets::set_secret(
                "rotation",
                STATE_KEY,
                &serde_json::to_string(&state).unwrap(),
            )
            .unwrap();
            let mut server = remote_server(&endpoint.url, None);
            server.id = "rotation".into();
            server.unknown_fields.insert(
                "headerKeys".into(),
                serde_json::json!([{"key":"X-Api-Key","env":"API_HEADER"}]),
            );
            secrets::set_secret("rotation", "API_HEADER", "header-fixture").unwrap();
            let mut connection = connect_remote(&server).unwrap();
            let result = connection.call("fixture", serde_json::json!({})).unwrap();
            assert_eq!(result["authorization"], "Bearer token-0");
            assert_eq!(endpoint.count(), 0);
        });
    }
    #[test]
    fn legacy_headers_do_not_skip_client_credentials_state_validation() {
        secrets::tests::with_isolated_vault(|| {
            let mut server = remote_server("https://example.com/mcp", None);
            server.client_credentials = Some(crate::registry::ClientCredentials {
                client_id: "client".into(),
                ..Default::default()
            });
            server.unknown_fields.insert(
                "headerKeys".into(),
                serde_json::json!([{"key":"X-Api-Key","env":"API_HEADER"}]),
            );
            secrets::set_secret(&server.id, "API_HEADER", "header-fixture").unwrap();
            let result = secrets::tests::with_failed_read(CC_STATE_KEY, || connect_remote(&server));
            assert!(result.err().unwrap().contains("client-credentials state"));
        });
    }
    #[test]
    fn inline_secret_env_does_not_replace_existing_bearer_keychain_lookup() {
        let server: ServerEntry = serde_json::from_value(serde_json::json!({"id":"old","name":"Old","transport":"http","url":"https://example.invalid/mcp","env":[{"key":"TOKEN","secret":true,"value":"inline-never-bearer"}]})).unwrap();
        assert_eq!(first_vaulted_secret(&server).unwrap(), None);
    }
    #[test]
    fn reviewed_url_only_vault_is_recognized_as_owned_credentials() {
        secrets::tests::with_isolated_vault(|| {
            let mut server = remote_server("https://example.invalid/mcp", None);
            server.unknown_fields.insert(
                "importedUrlKey".into(),
                serde_json::json!(secrets::IMPORTED_URL_KEY),
            );
            secrets::set_secret(
                &server.id,
                secrets::IMPORTED_URL_KEY,
                "https://example.invalid/mcp?token=private",
            )
            .unwrap();
            assert!(secrets::has_own_credentials(&server).unwrap());
            assert_eq!(first_vaulted_secret(&server).unwrap(), None);
        });
    }

    #[test]
    fn reviewed_bearer_env_errors_are_redacted() {
        secrets::tests::with_isolated_vault(|| {
            let mut server = remote_server("https://example.invalid/mcp", None);
            server.env.push(crate::registry::EnvVar {
                key: "PAT".into(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            });
            secrets::set_secret(&server.id, "PAT", "synthetic-codex-token").unwrap();
            assert_eq!(
                safe_imported_error(&server, "HTTP 500: synthetic-codex-token rejected".into()),
                "HTTP 500: <redacted> rejected"
            );
        });
    }

    #[test]
    fn reviewed_url_redaction_preserves_status_numbers() {
        let server = remote_server("https://private-user:private-password@example.invalid/sk-private-path?token=private%2Fquery&v=1", None);
        let redaction = Redaction::for_server(&server);
        let result = redaction.text("HTTP 401: private-user private-password sk-private-path private%2Fquery private/query; attempt 1".into());
        assert_eq!(
            result,
            "HTTP 401: <redacted> <redacted> <redacted> <redacted> <redacted>; attempt 1"
        );
    }

    #[test]
    fn imported_bearer_connect_error_does_not_echo_provider_credentials() {
        secrets::tests::with_isolated_vault(|| {
            let listener = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let mut server = remote_server(&format!("http://{}/mcp", listener.server_addr()), None);
            server.env.push(crate::registry::EnvVar {
                key: secrets::HTTP_AUTH_KEY.into(),
                value: None,
                secret: true,
                unknown_fields: Default::default(),
            });
            secrets::set_secret(&server.id, secrets::HTTP_AUTH_KEY, "synthetic-imported-pat")
                .unwrap();
            let worker = std::thread::spawn(move || {
                listener
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
                    .respond(
                        tiny_http::Response::from_string("synthetic-imported-pat")
                            .with_status_code(500),
                    )
                    .unwrap();
            });
            let error = connect_remote(&server).err().unwrap();
            worker.join().unwrap();
            assert!(!error.contains("synthetic-imported-pat"));
            assert!(error.contains("500"));
        });
    }

    #[test]
    fn reviewed_transport_preserves_rpc_errors_and_redacts_only_private_values() {
        let redact = Redaction(
            vec![
                "synthetic-pat".into(),
                "https://example.invalid/mcp?token=secret".into(),
            ],
            vec![],
        );
        let frame = redact.error(crate::downstream::TransportError::FrameRejected(
            "oversized frame near synthetic-pat".into(),
        ));
        assert!(
            matches!(frame, crate::downstream::TransportError::FrameRejected(ref message)
            if message == "oversized frame near <redacted>")
        );
        let error = redact.error(crate::downstream::TransportError::Rpc(serde_json::json!({"code":-32602,"message":"Invalid argument near synthetic-pat","data":{"endpoint":"https://example.invalid/mcp?token=secret","field":"limit"}})));
        if let crate::downstream::TransportError::Rpc(value) = error {
            assert_eq!(value["code"], -32602);
            assert_eq!(value["message"], "Invalid argument near <redacted>");
            assert_eq!(value["data"]["field"], "limit");
            assert_eq!(value["data"]["endpoint"], "<redacted>");
        } else {
            panic!("RPC error category must survive");
        }
        assert_eq!(
            redact.text("HTTP 429: rate limit exceeded".into()),
            "HTTP 429: rate limit exceeded"
        );
    }

    #[test]
    fn classifies_auth_errors() {
        assert!(is_auth_error("HTTP 401 (needs authentication): ..."));
        assert!(is_auth_error("got 403 Forbidden"));
        assert!(!is_auth_error("HTTP 500: server error"));
        assert!(!is_auth_error("connection refused"));
    }

    #[test]
    fn a_status_code_buried_in_a_longer_number_is_not_an_auth_error() {
        // Misreading these as auth failures shows the user a "Needs sign-in"
        // prompt for a network fault and burns an OAuth refresh exchange on it.
        assert!(!is_auth_error("connection refused (os error 10401)"));
        assert!(!is_auth_error("dial tcp 127.0.0.1:4013: refused"));
        assert!(!is_auth_error("read timed out after 4030ms"));
        assert!(!is_auth_error("HTTP 500: upstream returned 14012 bytes"));
        // Still caught at a boundary, wherever it sits in the message.
        assert!(is_auth_error("HTTP 401"));
        assert!(is_auth_error("server said 403."));
        assert!(is_auth_error("(403)"));
    }

    fn racing_state(expires_at: Option<u64>) -> OAuthState {
        OAuthState {
            issuer: Some("https://auth.example.com".into()),
            token_endpoint: "https://auth.example.com/token".into(),
            client_id: "client".into(),
            refresh_token: Some("rt-1".into()),
            resource: Some("https://mcp.example.com".into()),
            scope: None,
            issued_at: Some(1_000),
            expires_at,
        }
    }

    #[test]
    fn an_unchanged_vaulted_token_leaves_the_refresh_to_us() {
        // Nobody rotated it while we waited for the lock, so the caller must go on and
        // do the exchange rather than handing back a token it already knows is stale.
        let state = racing_state(Some(10_000));
        assert!(
            reuse_racing_refresh(Some("token-0"), Some("token-0".into()), Some(&state), 1_000)
                .is_none(),
            "an unchanged token is not somebody else's win"
        );
    }

    #[test]
    fn a_token_rotated_while_waiting_is_reused_instead_of_refreshed_again() {
        // The SBS-479 race: we parked on the lock and the other process refreshed.
        // Spending our own exchange now burns a refresh token it already invalidated,
        // which is what trips a provider's reuse detection.
        let state = racing_state(Some(10_000));
        let winner =
            reuse_racing_refresh(Some("token-0"), Some("token-1".into()), Some(&state), 1_000)
                .expect("a rotated, still-valid token must be reused");
        assert_eq!(winner.access_token, "token-1");
        assert_eq!(winner.expires_at, Some(10_000));
    }

    #[test]
    fn a_rotated_but_expired_token_still_triggers_a_refresh() {
        // Reusing it would hand the caller a credential that is already dead. Uses the
        // same refresh_decision as the proactive path, so the skew window agrees.
        let state = racing_state(Some(1_030));
        assert!(
            reuse_racing_refresh(Some("token-0"), Some("token-1".into()), Some(&state), 1_000)
                .is_none(),
            "inside the pre-expiry skew window this is not a usable win"
        );
    }

    #[test]
    fn a_rotation_without_vaulted_state_is_not_reused() {
        // No state means no expiry to judge it by; refreshing is the safe read.
        assert!(
            reuse_racing_refresh(Some("token-0"), Some("token-1".into()), None, 1_000).is_none()
        );
        // And an empty vault is not a win either.
        let state = racing_state(Some(10_000));
        assert!(reuse_racing_refresh(Some("token-0"), None, Some(&state), 1_000).is_none());
    }

    #[test]
    fn a_client_credentials_token_minted_while_waiting_is_reused() {
        // A CC waiter that wins the lock after the other process already minted must not
        // spend a second grant. Serialization alone would prevent the race but not the
        // redundant round trip.
        let winner =
            reuse_racing_client_credentials(Some("token-0"), Some("token-1".into()), 10_000, 1_000)
                .expect("a freshly minted, still-valid CC token must be reused");
        assert_eq!(winner.access_token, "token-1");
        assert_eq!(winner.expires_at, Some(10_000));
    }

    #[test]
    fn client_credentials_reuse_honours_the_same_skew_as_the_proactive_path() {
        // Inside the pre-expiry window the proactive path would replace this token on the
        // very next connect, so accepting it here just defers the work by one call.
        assert!(reuse_racing_client_credentials(
            Some("token-0"),
            Some("token-1".into()),
            1_030,
            1_000
        )
        .is_none());
        // And an unchanged token still means the mint is ours to do.
        assert!(reuse_racing_client_credentials(
            Some("token-0"),
            Some("token-0".into()),
            10_000,
            1_000
        )
        .is_none());
    }

    #[test]
    fn the_refresh_lock_is_per_server_and_released_on_drop() {
        // Two servers must not serialize against each other, or one slow provider
        // stalls refresh for every other server in the registry.
        let _guard = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-oauth-refresh-lock-{}-{}",
            std::process::id(),
            now_epoch_seconds()
        ));
        std::fs::create_dir_all(&dir).expect("scratch data dir");
        let _override = crate::registry::DataDirOverride::set(&dir);

        let a = lock_oauth_refresh("server-a").expect("a data dir is set, so a lock exists");
        let b = lock_oauth_refresh("server-b").expect("a different server must not block");
        let contention =
            match lock_oauth_refresh_for("server-a", std::time::Duration::from_millis(40)) {
                Err(error) => error,
                Ok(_) => panic!("contention must remain distinguishable from a missing data dir"),
            };
        assert!(contention.contains("locked by another Toolport process"));
        assert!(
            OAUTH_REFRESH_LOCK_TIMEOUT >= std::time::Duration::from_secs(30),
            "the production wait must cover the token client's request timeout"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names
            .iter()
            .any(|name| name.starts_with("oauth-refresh-") && name.ends_with(".lock")));
        assert!(!names.iter().any(|name| name.ends_with(".lock.lock")));
        drop((a, b));

        assert!(
            lock_oauth_refresh("server-a").is_ok(),
            "the lock must be reacquirable once released"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    struct RotatingEndpoint {
        url: String,
        exchanges: Arc<std::sync::atomic::AtomicUsize>,
        reject_access: Arc<AtomicBool>,
        started: std::sync::mpsc::Receiver<()>,
        release: std::sync::mpsc::SyncSender<()>,
        server: Arc<tiny_http::Server>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl RotatingEndpoint {
        fn new() -> Self {
            Self::with_rotation(true)
        }

        fn with_rotation(rotates: bool) -> Self {
            let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
            let url = format!("http://{}/token", server.server_addr());
            let exchanges = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let reject_access = Arc::new(AtomicBool::new(false));
            let reject = Arc::clone(&reject_access);
            let (started_tx, started) = std::sync::mpsc::channel();
            let (release, release_rx) = std::sync::mpsc::sync_channel(1);
            let endpoint = Arc::clone(&server);
            let count = Arc::clone(&exchanges);
            let worker = std::thread::spawn(move || {
                let mut release_rx = Some(release_rx);
                let mut handlers = Vec::new();
                while let Ok(mut request) = endpoint.recv() {
                    let mut body = String::new();
                    request.as_reader().read_to_string(&mut body).unwrap();
                    if !body.contains("grant_type=refresh_token") {
                        if reject.load(Ordering::SeqCst) {
                            request.respond(tiny_http::Response::empty(401)).unwrap();
                            continue;
                        }
                        let message: serde_json::Value = serde_json::from_str(&body).unwrap();
                        if message.get("id").is_none() {
                            request.respond(tiny_http::Response::empty(202)).unwrap();
                            continue;
                        }
                        let auth = request
                            .headers()
                            .iter()
                            .find(|h| h.field.equiv("Authorization"))
                            .map(|h| h.value.as_str())
                            .unwrap_or("");
                        let response = serde_json::json!({"jsonrpc":"2.0", "id":message["id"],
                            "result":{"authorization":auth, "tools":[]}})
                        .to_string();
                        request
                            .respond(
                                tiny_http::Response::from_string(response).with_header(
                                    tiny_http::Header::from_bytes(
                                        "Content-Type",
                                        "application/json",
                                    )
                                    .unwrap(),
                                ),
                            )
                            .unwrap();
                        continue;
                    }
                    let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let expected = if rotates { index } else { 0 };
                    if !body.contains(&format!("refresh_token=rt-{expected}")) {
                        request
                            .respond(
                                tiny_http::Response::from_string(r#"{"error":"invalid_grant"}"#)
                                    .with_status_code(400),
                            )
                            .unwrap();
                        continue;
                    }
                    let mut response = serde_json::json!({"access_token":format!("token-{}", index+1),
                        "expires_in":3600,"token_type":"Bearer"});
                    if rotates {
                        response["refresh_token"] = format!("rt-{}", index + 1).into();
                    }
                    if index == 0 {
                        let release_rx = release_rx.take().unwrap();
                        let started_tx = started_tx.clone();
                        handlers.push(std::thread::spawn(move || {
                            started_tx.send(()).unwrap();
                            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                            request
                                .respond(tiny_http::Response::from_string(response.to_string()))
                                .unwrap();
                        }));
                    } else {
                        request
                            .respond(tiny_http::Response::from_string(response.to_string()))
                            .unwrap();
                    }
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            });
            Self {
                url,
                exchanges,
                reject_access,
                started,
                release,
                server,
                worker: Some(worker),
            }
        }

        fn seed(&self) {
            let state = OAuthState {
                issuer: None,
                token_endpoint: self.url.clone(),
                client_id: "client".into(),
                refresh_token: Some("rt-0".into()),
                resource: None,
                scope: None,
                issued_at: Some(1),
                expires_at: Some(2),
            };
            secrets::set_secret(
                "rotation",
                STATE_KEY,
                &serde_json::to_string(&state).unwrap(),
            )
            .unwrap();
            secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "token-0").unwrap();
        }

        fn count(&self) -> usize {
            self.exchanges.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for RotatingEndpoint {
        fn drop(&mut self) {
            let _ = self.release.try_send(());
            self.server.unblock();
            self.worker.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn oauth_refresh_long_holder_never_allows_an_unlocked_exchange() {
        secrets::tests::with_isolated_vault(|| {
            std::thread::scope(|scope| {
                let endpoint = RotatingEndpoint::new();
                endpoint.seed();
                // Separate pending memory models a holder in another process.
                let holder = scope.spawn(|| {
                    refresh_token_with_pending(
                        "rotation",
                        || lock_oauth_refresh("rotation"),
                        &mut CredentialState::default(),
                        true,
                        None,
                    )
                    .map(|token| token.access_token)
                });
                endpoint
                    .started
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                // Inject the wait deadline while the first exchange is still blocked.
                // This is the old >65s race without a 65s wall-clock sleep.
                let waited = refresh_token_with_lock("rotation", None, || {
                    lock_oauth_refresh_for("rotation", Duration::ZERO)
                });
                endpoint.release.send(()).unwrap();
                assert_eq!(holder.join().unwrap().unwrap(), "token-1");
                let error = waited.err().unwrap();
                assert_eq!(error, OAUTH_REFRESH_LOCK_ERROR);
                assert!(!is_auth_error(&error));
                assert_eq!(endpoint.count(), 1);
                assert_eq!(
                    load_state("rotation")
                        .unwrap()
                        .unwrap()
                        .refresh_token
                        .as_deref(),
                    Some("rt-1")
                );
            })
        });
    }

    #[test]
    fn oauth_refresh_independent_waiter_uses_the_saved_winner() {
        for timeout_after_save in [false, true] {
            secrets::tests::with_isolated_vault(|| {
                std::thread::scope(|scope| {
                    let endpoint = RotatingEndpoint::new();
                    endpoint.seed();
                    let holder = scope.spawn(|| {
                        refresh_token_with_pending(
                            "rotation",
                            || lock_oauth_refresh("rotation"),
                            &mut CredentialState::default(),
                            true,
                            None,
                        )
                        .map(|token| token.access_token)
                    });
                    endpoint
                        .started
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    let winner = refresh_token_with_lock("rotation", None, || {
                        // The waiter has taken its pre-lock snapshot. Keep the holder
                        // blocked until contention is proved, then join its save before
                        // rechecking the lock. Separate pending memory models a peer
                        // process without a timed channel racing its vault writes.
                        let contended = lock_oauth_refresh_for("rotation", Duration::ZERO);
                        assert!(contended.is_err());
                        endpoint.release.send(()).unwrap();
                        assert_eq!(holder.join().unwrap().unwrap(), "token-1");
                        if timeout_after_save {
                            contended
                        } else {
                            lock_oauth_refresh_for("rotation", Duration::ZERO)
                        }
                    })
                    .unwrap();
                    assert_eq!(winner.access_token, "token-1");
                    assert!(winner.expires_at.unwrap() > now_epoch_seconds());
                    assert_eq!(endpoint.count(), 1);
                })
            });
        }
    }

    #[test]
    fn oauth_refresh_access_save_failure_returns_token_after_saving_rotation() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            let token = secrets::tests::with_failed_write(secrets::HTTP_AUTH_KEY, || {
                refresh_token_if_needed("rotation").unwrap().unwrap()
            });
            assert_eq!(token, "token-1");
            assert_eq!(
                load_state("rotation")
                    .unwrap()
                    .unwrap()
                    .refresh_token
                    .as_deref(),
                Some("rt-1")
            );
            assert_eq!(
                secrets::get_secret("rotation", secrets::HTTP_AUTH_KEY).as_deref(),
                Some("token-0")
            );
            assert_eq!(refresh_token("rotation", None).unwrap(), "token-2");
        });
    }

    #[test]
    fn oauth_connect_does_not_refresh_twice_after_a_failed_token_save() {
        for key in [STATE_KEY, secrets::HTTP_AUTH_KEY] {
            secrets::tests::with_isolated_vault(|| {
                let endpoint = RotatingEndpoint::new();
                endpoint.seed();
                let mut state = load_state("rotation").unwrap().unwrap();
                state.expires_at = Some(now_epoch_seconds() + 3600);
                secrets::set_secret(
                    "rotation",
                    STATE_KEY,
                    &serde_json::to_string(&state).unwrap(),
                )
                .unwrap();
                endpoint.reject_access.store(true, Ordering::SeqCst);
                endpoint.release.send(()).unwrap();
                let mut server = remote_server(&endpoint.url, None);
                server.id = "rotation".into();
                let error = secrets::tests::with_failed_write(key, || {
                    connect_remote(&server).err().unwrap()
                });
                assert!(is_auth_error(&error), "{error}");
                assert_eq!(
                    endpoint.count(),
                    1,
                    "connect must not perform another recovery exchange"
                );
            });
        }
    }

    #[test]
    fn oauth_refresh_unavailable_lock_is_a_retriable_failure() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            let error =
                refresh_token_with_lock("rotation", None, || Err("cannot create lock".into()))
                    .err()
                    .unwrap();
            assert_eq!(error, OAUTH_REFRESH_LOCK_ERROR);
            assert!(!is_auth_error(&error));
            assert_eq!(endpoint.count(), 0);
        });
    }

    fn rotation_transport(endpoint: &RotatingEndpoint) -> HttpTransport {
        authed_transport(
            &endpoint.url,
            Some("token-0".into()),
            "rotation",
            false,
            Duration::from_secs(5),
        )
        .unwrap()
        .0
    }

    fn request_auth(transport: &mut HttpTransport) -> String {
        transport
            .request("tools/list", serde_json::json!({}))
            .unwrap()["authorization"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn oauth_refresh_nonrotating_save_failure_recovers() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::with_rotation(false);
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            let mut transport = rotation_transport(&endpoint);
            secrets::tests::with_failed_write(STATE_KEY, || {
                assert_eq!(request_auth(&mut transport), "Bearer token-1");
            });
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_none());
            assert_eq!(refresh_token("rotation", None).unwrap(), "token-2");
            assert_eq!(
                load_state("rotation")
                    .unwrap()
                    .unwrap()
                    .refresh_token
                    .as_deref(),
                Some("rt-0")
            );
            assert_eq!(endpoint.count(), 2);
        });
    }

    #[test]
    fn oauth_refresh_rotating_save_failure_uses_memory_and_retries_persistence() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            let mut transport = rotation_transport(&endpoint);
            secrets::tests::with_failed_write(STATE_KEY, || {
                assert_eq!(request_auth(&mut transport), "Bearer token-1");
                // This forced exchange must use RT1, although the vault still has RT0.
                assert_eq!(refresh_token("rotation", None).unwrap(), "token-2");
                assert_eq!(request_auth(&mut transport), "Bearer token-2");
            });
            assert_eq!(
                load_state("rotation")
                    .unwrap()
                    .unwrap()
                    .refresh_token
                    .as_deref(),
                Some("rt-0")
            );
            assert_eq!(
                refresh_token_if_needed("rotation").unwrap().as_deref(),
                Some("token-2")
            );
            assert_eq!(request_auth(&mut transport), "Bearer token-2");
            assert_eq!(
                load_state("rotation")
                    .unwrap()
                    .unwrap()
                    .refresh_token
                    .as_deref(),
                Some("rt-2")
            );
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_none());
            assert_eq!(endpoint.count(), 2);
        });
    }

    #[test]
    fn oauth_refresh_vault_change_discards_pending_rotation() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            let mut transport = rotation_transport(&endpoint);
            secrets::tests::with_failed_write(STATE_KEY, || {
                assert_eq!(request_auth(&mut transport), "Bearer token-1");
            });
            let mut peer = load_state("rotation").unwrap().unwrap();
            peer.refresh_token = Some("peer-rt".into());
            peer.expires_at = Some(now_epoch_seconds() + 3600);
            let peer_json = serde_json::to_string(&peer).unwrap();
            secrets::set_secret("rotation", STATE_KEY, &peer_json).unwrap();
            secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "peer-token").unwrap();
            assert_eq!(request_auth(&mut transport), "Bearer peer-token");
            assert_eq!(
                secrets::get_secret("rotation", STATE_KEY).unwrap(),
                peer_json
            );
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_none());
            assert_eq!(endpoint.count(), 1);
        });
    }

    fn pending_rotation(endpoint: &RotatingEndpoint) -> HttpTransport {
        endpoint.seed();
        endpoint.release.send(()).unwrap();
        let mut transport = rotation_transport(endpoint);
        secrets::tests::with_failed_write(STATE_KEY, || {
            assert_eq!(request_auth(&mut transport), "Bearer token-1");
        });
        transport
    }

    #[test]
    fn oauth_pending_read_failure_uses_valid_token_for_requests_and_connect() {
        for key in [secrets::HTTP_AUTH_KEY, STATE_KEY, CC_STATE_KEY] {
            secrets::tests::with_isolated_vault(|| {
                let endpoint = RotatingEndpoint::new();
                let mut transport = pending_rotation(&endpoint);
                secrets::tests::with_failed_read(key, || {
                    assert_eq!(request_auth(&mut transport), "Bearer token-1");
                    let mut server = remote_server(&endpoint.url, None);
                    server.id = "rotation".into();
                    assert!(connect_remote(&server).is_ok());
                    assert!(credential_update("rotation")
                        .lock()
                        .unwrap()
                        .pending
                        .is_some());
                    assert!(refresh_token("rotation", Some("token-1")).is_err());
                    if key != CC_STATE_KEY {
                        assert!(current_credential("rotation").is_err());
                        assert!(newer_credential("rotation", "token-0").is_err());
                    }
                });
                assert_eq!(request_auth(&mut transport), "Bearer token-1");
                assert!(credential_update("rotation")
                    .lock()
                    .unwrap()
                    .pending
                    .is_none());
                assert_eq!(
                    load_state("rotation")
                        .unwrap()
                        .unwrap()
                        .refresh_token
                        .as_deref(),
                    Some("rt-1")
                );
                assert_eq!(endpoint.count(), 1);
            });
        }
    }

    #[test]
    fn oauth_read_failure_without_valid_pending_token_fails_closed() {
        for key in [secrets::HTTP_AUTH_KEY, STATE_KEY, CC_STATE_KEY] {
            secrets::tests::with_isolated_vault(|| {
                let endpoint = RotatingEndpoint::new();
                endpoint.seed();
                secrets::tests::with_failed_read(key, || {
                    assert!(refresh_token_for_connect("rotation").is_err());
                    let mut server = remote_server(&endpoint.url, None);
                    server.id = "rotation".into();
                    assert!(connect_remote(&server).is_err());
                });
                let _transport = pending_rotation(&endpoint);
                credential_update("rotation")
                    .lock()
                    .unwrap()
                    .pending
                    .as_mut()
                    .unwrap()
                    .token
                    .expires_at = Some(0);
                secrets::tests::with_failed_read(key, || {
                    assert!(refresh_token_for_connect("rotation").is_err());
                    let mut server = remote_server(&endpoint.url, None);
                    server.id = "rotation".into();
                    assert!(connect_remote(&server).is_err());
                });
                assert_eq!(endpoint.count(), 1);
            });
        }
    }

    #[test]
    fn oauth_pending_save_that_landed_keeps_its_matching_access_token() {
        for lookup_first in [false, true] {
            secrets::tests::with_isolated_vault(|| {
                let endpoint = RotatingEndpoint::new();
                let mut transport = pending_rotation(&endpoint);
                let json = credential_update("rotation")
                    .lock()
                    .unwrap()
                    .pending
                    .as_ref()
                    .unwrap()
                    .state
                    .clone();
                // Emulate a D-Bus write committing after its caller reported a timeout.
                secrets::set_secret("rotation", STATE_KEY, &json).unwrap();
                assert_eq!(
                    secrets::get_secret("rotation", secrets::HTTP_AUTH_KEY).as_deref(),
                    Some("token-0")
                );
                if lookup_first {
                    secrets::tests::with_failed_write(secrets::HTTP_AUTH_KEY, || {
                        assert_eq!(
                            current_credential("rotation").unwrap().as_deref(),
                            Some("token-1")
                        );
                    });
                }
                assert_eq!(request_auth(&mut transport), "Bearer token-1");
                assert_eq!(
                    current_credential("rotation").unwrap().as_deref(),
                    Some("token-1")
                );
                assert!(credential_update("rotation")
                    .lock()
                    .unwrap()
                    .pending
                    .is_none());
                if !lookup_first {
                    assert_eq!(
                        secrets::get_secret("rotation", secrets::HTTP_AUTH_KEY).as_deref(),
                        Some("token-1")
                    );
                }
                assert_eq!(endpoint.count(), 1);
            });
        }
    }

    #[test]
    fn oauth_forced_refresh_rechecks_rejected_token_after_lookup() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            assert_eq!(newer_credential("rotation", "token-0").unwrap(), None);
            endpoint.release.send(()).unwrap();
            assert_eq!(refresh_token("rotation", None).unwrap(), "token-1");
            assert_eq!(
                refresh_token("rotation", Some("token-0")).unwrap(),
                "token-1"
            );
            assert_eq!(endpoint.count(), 1);
            assert_eq!(
                refresh_token("rotation", Some("token-1")).unwrap(),
                "token-2"
            );
            assert_eq!(endpoint.count(), 2);
        });
    }

    #[test]
    fn oauth_forced_refresh_does_not_reuse_rejected_lock_winner() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            let token = refresh_token_with_lock("rotation", Some("rejected-peer"), || {
                let lock = lock_oauth_refresh("rotation")?;
                let mut peer = load_state("rotation")?.unwrap();
                peer.expires_at = Some(now_epoch_seconds() + 3600);
                secrets::set_secret(
                    "rotation",
                    STATE_KEY,
                    &serde_json::to_string(&peer).unwrap(),
                )?;
                secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "rejected-peer")?;
                Ok(lock)
            })
            .unwrap();
            assert_eq!(token.access_token, "token-1");
            assert_eq!(endpoint.count(), 1);
        });
    }

    #[test]
    fn oauth_concurrent_rejections_exchange_once_even_when_save_fails() {
        for failed_key in [STATE_KEY, secrets::HTTP_AUTH_KEY] {
            secrets::tests::with_isolated_vault(|| {
                let endpoint = RotatingEndpoint::new();
                endpoint.seed();
                let barrier = std::sync::Barrier::new(3);
                std::thread::scope(|scope| {
                    let callers: Vec<_> = (0..2)
                        .map(|_| {
                            let barrier = &barrier;
                            scope.spawn(move || {
                                barrier.wait();
                                secrets::tests::with_failed_write(failed_key, || {
                                    refresh_token("rotation", Some("token-0")).unwrap()
                                })
                            })
                        })
                        .collect();
                    barrier.wait();
                    endpoint
                        .started
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    endpoint.release.send(()).unwrap();
                    for caller in callers {
                        assert_eq!(caller.join().unwrap(), "token-1");
                    }
                });
                assert_eq!(endpoint.count(), 1);
            });
        }
    }

    #[test]
    fn oauth_pending_busy_lock_keeps_requests_and_connect_usable() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            let mut transport = pending_rotation(&endpoint);
            let holder = lock_oauth_refresh("rotation").unwrap();
            assert_eq!(request_auth(&mut transport), "Bearer token-1");
            let mut server = remote_server(&endpoint.url, None);
            server.id = "rotation".into();
            assert!(connect_remote(&server).is_ok());
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_some());
            // The same unavailable-lock result remains an error on the forced path.
            let error = refresh_token_with_lock("rotation", Some("token-1"), || {
                lock_oauth_refresh_for("rotation", Duration::ZERO)
            })
            .err()
            .unwrap();
            assert_eq!(error, OAUTH_REFRESH_LOCK_ERROR);
            drop(holder);
            assert_eq!(request_auth(&mut transport), "Bearer token-1");
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_none());
            assert_eq!(endpoint.count(), 1);
        });
    }

    #[test]
    fn oauth_pending_busy_update_mutex_keeps_valid_request_token() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            let mut transport = pending_rotation(&endpoint);
            let update = credential_update("rotation");
            let holder = update.lock().unwrap();
            // A blocking mutex acquisition would deadlock this deterministic test.
            assert_eq!(request_auth(&mut transport), "Bearer token-1");
            drop(holder);
            assert_eq!(request_auth(&mut transport), "Bearer token-1");
            assert_eq!(endpoint.count(), 1);
        });
    }

    #[test]
    fn current_credential_memory_after_vault_wins_until_vault_changes() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            // Observe another process's token before this process rotates locally.
            secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "peer-before").unwrap();
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("peer-before")
            );
            endpoint.release.send(()).unwrap();
            secrets::tests::with_failed_write(STATE_KEY, || {
                assert_eq!(refresh_token("rotation", None).unwrap(), "token-1");
            });
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("token-1")
            );
            assert_eq!(
                newer_credential("rotation", "peer-before")
                    .unwrap()
                    .as_deref(),
                Some("token-1")
            );
            assert_eq!(newer_credential("rotation", "token-1").unwrap(), None);
            // A bearer-only peer update must supersede the unsaved local pair too.
            secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "peer-after").unwrap();
            assert_eq!(
                newer_credential("rotation", "token-1").unwrap().as_deref(),
                Some("peer-after")
            );
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_none());
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("peer-after")
            );
            assert_eq!(endpoint.count(), 1, "credential lookup never exchanges");
        });
    }

    #[test]
    fn http_rejection_adopts_unsaved_credentials_without_an_exchange() {
        use crate::downstream::Transport;
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            secrets::tests::with_failed_write(STATE_KEY, || {
                assert_eq!(refresh_token("rotation", None).unwrap(), "token-1");
            });
            for (concurrent, pending) in
                [(false, true), (true, true), (false, false), (true, false)]
            {
                let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
                let mut transport = HttpTransport::with_auth_refresh(
                    &format!("http://{}/", server.server_addr()),
                    Some("token-0".into()),
                    Some(Box::new(|force, rejected| {
                        if force {
                            assert_eq!(rejected, Some("token-0"));
                            refresh_token("rotation", rejected).map(Some)
                        } else {
                            Ok(None)
                        }
                    })),
                );
                if !pending {
                    // Exercise authed_transport's actual callback with a peer token
                    // saved after construction, while proactive refresh is not due.
                    let mut state = load_state("rotation").unwrap().unwrap();
                    state.expires_at = Some(now_epoch_seconds() + 3600);
                    secrets::set_secret(
                        "rotation",
                        STATE_KEY,
                        &serde_json::to_string(&state).unwrap(),
                    )
                    .unwrap();
                    secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "token-0").unwrap();
                    assert_eq!(
                        current_credential("rotation").unwrap().as_deref(),
                        Some("token-0")
                    );
                    transport = authed_transport(
                        &format!("http://{}/", server.server_addr()),
                        Some("token-0".into()),
                        "rotation",
                        false,
                        Duration::from_secs(5),
                    )
                    .unwrap()
                    .0;
                    secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "token-1").unwrap();
                }
                transport.set_server_id("rotation");
                let wire = std::thread::spawn(move || {
                    let mut auths = Vec::new();
                    for _ in 0..2 {
                        let mut request = server
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap()
                            .unwrap();
                        let auth = request
                            .headers()
                            .iter()
                            .find(|h| h.field.equiv("Authorization"))
                            .unwrap()
                            .value
                            .as_str()
                            .to_string();
                        let mut text = String::new();
                        request.as_reader().read_to_string(&mut text).unwrap();
                        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
                        let response = if auth == "Bearer token-1" {
                            tiny_http::Response::from_string(serde_json::json!({"jsonrpc":"2.0","id":body["id"],"result":{"ok":true}}).to_string())
                        } else {
                            tiny_http::Response::from_string("revoked").with_status_code(401)
                        };
                        auths.push(auth);
                        request.respond(response).unwrap();
                    }
                    auths
                });
                let result = if concurrent {
                    transport.concurrent().unwrap().request_with_cancel(
                        "echo",
                        serde_json::json!({}),
                        None,
                    )
                } else {
                    transport.request("echo", serde_json::json!({}))
                };
                assert_eq!(result.unwrap(), serde_json::json!({"ok":true}));
                assert_eq!(wire.join().unwrap(), ["Bearer token-0", "Bearer token-1"]);
            }
            assert_eq!(endpoint.count(), 1);
        });
    }

    #[test]
    fn current_credential_vault_state_after_memory_wins_and_clear_removes_auth() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("token-0")
            );
            endpoint.release.send(()).unwrap();
            secrets::tests::with_failed_write(STATE_KEY, || {
                assert_eq!(refresh_token("rotation", None).unwrap(), "token-1");
            });
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("token-1")
            );
            let mut peer = load_state("rotation").unwrap().unwrap();
            peer.refresh_token = Some("peer-rt".into());
            // State-only replacement counts, even if its bearer matches the old vault.
            secrets::set_secret(
                "rotation",
                STATE_KEY,
                &serde_json::to_string(&peer).unwrap(),
            )
            .unwrap();
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("token-0")
            );
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_none());
            secrets::delete_secret("rotation", STATE_KEY).unwrap();
            secrets::delete_secret("rotation", secrets::HTTP_AUTH_KEY).unwrap();
            assert_eq!(current_credential("rotation").unwrap(), None);
            assert_eq!(newer_credential("rotation", "token-0").unwrap(), None);
            assert_eq!(endpoint.count(), 1);
        });
    }

    #[test]
    fn current_credential_retains_last_token_after_failed_access_save() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            secrets::tests::with_failed_write(secrets::HTTP_AUTH_KEY, || {
                assert_eq!(refresh_token("rotation", None).unwrap(), "token-1");
            });
            assert!(credential_update("rotation")
                .lock()
                .unwrap()
                .pending
                .is_none());
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("token-1")
            );
            assert_eq!(
                newer_credential("rotation", "token-0").unwrap().as_deref(),
                Some("token-1")
            );
            assert_eq!(newer_credential("rotation", "token-1").unwrap(), None);
            secrets::set_secret("rotation", secrets::HTTP_AUTH_KEY, "peer-token").unwrap();
            assert_eq!(
                current_credential("rotation").unwrap().as_deref(),
                Some("peer-token")
            );
            assert!(current_credential(RESERVED_VAULT_NS).is_err());
        });
    }

    #[test]
    fn oauth_refresh_lost_rotation_needs_authentication() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            endpoint.release.send(()).unwrap();
            secrets::tests::with_failed_write(STATE_KEY, || {
                assert_eq!(refresh_token("rotation", None).unwrap(), "token-1");
            });
            // Simulate a daemon restart losing the unsaved RT1.
            *credential_update("rotation").lock().unwrap() = CredentialState::default();
            let error = refresh_token_if_needed("rotation").unwrap_err();
            assert!(is_auth_error(&error), "{error}");
            assert!(!error.contains("rt-0"));
        });
    }

    #[test]
    fn oauth_connect_refresh_keeps_current_token_on_transient_failure() {
        secrets::tests::with_isolated_vault(|| {
            let endpoint = RotatingEndpoint::new();
            endpoint.seed();
            let mut state = load_state("rotation").unwrap().unwrap();
            state.expires_at = Some(now_epoch_seconds() + 30);
            // Invalid metadata/discovery URL fails immediately without network timing.
            state.issuer = Some("https://issuer.example".into());
            state.resource = Some("invalid resource URL".into());
            secrets::set_secret(
                "rotation",
                STATE_KEY,
                &serde_json::to_string(&state).unwrap(),
            )
            .unwrap();
            assert_eq!(refresh_token_if_needed("rotation").unwrap(), None);
            let mut transport = rotation_transport(&endpoint);
            assert_eq!(request_auth(&mut transport), "Bearer token-0");
            assert_eq!(endpoint.count(), 0);
            assert_eq!(
                secrets::get_secret("rotation", secrets::HTTP_AUTH_KEY).as_deref(),
                Some("token-0")
            );
        });
        for error in [
            "network unavailable",
            "discovery unavailable",
            "OAuth token endpoint returned status code 500",
        ] {
            assert_eq!(connect_refresh_result(Err(error.into())).unwrap(), None);
        }
    }

    #[test]
    fn oauth_connect_refresh_propagates_lock_storage_and_auth_errors() {
        for error in [
            OAUTH_REFRESH_LOCK_ERROR,
            "could not read the vaulted OAuth state: locked",
            "could not parse the vaulted OAuth state: malformed",
            "OAuth refresh token was rejected; needs authentication",
        ] {
            assert_eq!(
                connect_refresh_result(Err(error.into())).unwrap_err(),
                error
            );
        }
    }

    #[test]
    fn oauth_refresh_token_endpoint_classifies_invalid_grant_and_401() {
        for (code, body, auth) in [
            (
                400,
                r#"{"error":"invalid_grant","description":"secret-token"}"#,
                true,
            ),
            (401, "secret-token", true),
            (400, r#"{"error":"temporarily_unavailable"}"#, false),
            (500, "secret-token", false),
        ] {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let url = format!("http://{}/token", server.server_addr());
            let worker = std::thread::spawn(move || {
                server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
                    .respond(tiny_http::Response::from_string(body).with_status_code(code))
                    .unwrap();
            });
            let error = oauth::refresh(&url, "client", "secret-token", None, false)
                .err()
                .unwrap();
            worker.join().unwrap();
            assert_eq!(is_auth_error(&error), auth, "{error}");
            assert!(!error.contains("secret-token"));
        }
    }

    #[test]
    fn oauth_refresh_lock_contention_uses_current_token_before_send() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", server.server_addr());
        let mut transport = HttpTransport::with_auth_refresh(
            &url,
            Some("old-token".into()),
            Some(Box::new(|_, _| Err(OAUTH_REFRESH_LOCK_ERROR.into()))),
        );
        let worker = std::thread::spawn(move || {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert!(request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization") && h.value.as_str() == "Bearer old-token"));
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let message: serde_json::Value = serde_json::from_str(&body).unwrap();
            request
                .respond(
                    tiny_http::Response::from_string(
                        serde_json::json!({"jsonrpc":"2.0",
                "id":message["id"],"result":{}})
                        .to_string(),
                    )
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                    ),
                )
                .unwrap();
        });
        transport
            .request("tools/list", serde_json::json!({}))
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn oauth_refresh_errors_surface_through_proactive_and_forced_http_calls() {
        for proactive in [true, false] {
            for error in [
                OAUTH_REFRESH_LOCK_ERROR.to_string(),
                format!("{OAUTH_REFRESH_SAVE_ERROR} the rotated refresh token"),
            ] {
                // Contention is deliberately ignored only before sending.
                if proactive && error == OAUTH_REFRESH_LOCK_ERROR {
                    continue;
                }
                let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
                let url = format!("http://{}/mcp", server.server_addr());
                let callback_error = error.clone();
                let mut transport = HttpTransport::with_auth_refresh(
                    &url,
                    Some("old-token".into()),
                    Some(Box::new(move |force, _| {
                        if proactive || force {
                            Err(callback_error.clone())
                        } else {
                            Ok(None)
                        }
                    })),
                );
                let worker = std::thread::spawn(move || {
                    if !proactive {
                        server
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap()
                            .unwrap()
                            .respond(tiny_http::Response::empty(401))
                            .unwrap();
                    }
                });
                let surfaced = transport
                    .request("tools/list", serde_json::json!({}))
                    .unwrap_err();
                worker.join().unwrap();
                assert_eq!(surfaced.to_string(), error);
                assert!(!is_auth_error(&surfaced.to_string()));
            }
        }
    }

    // Spawn this exact headless test as a lock holder; the parent kills it rather
    // than dropping the guard, proving OS recovery with the lock file retained.
    #[test]
    fn oauth_refresh_dead_holder_child() {
        let Some(dir) = std::env::var_os("TOOLPORT_REFRESH_LOCK_CHILD") else {
            return;
        };
        let _data_lock = crate::registry::data_dir_test_lock();
        let _override = crate::registry::DataDirOverride::set(std::path::PathBuf::from(dir));
        let _lock = lock_oauth_refresh("dead-holder").unwrap();
        use std::io::Write;
        println!("LOCKED");
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
    }

    #[test]
    fn oauth_refresh_recovers_the_lock_of_a_dead_process() {
        secrets::tests::with_isolated_vault(|| {
            use std::io::BufRead;
            struct Holder(std::process::Child);
            impl Drop for Holder {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let dir = crate::registry::conduit_dir().unwrap();
            let mut child = Holder(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "remote::tests::oauth_refresh_dead_holder_child",
                        "--nocapture",
                    ])
                    .env("TOOLPORT_REFRESH_LOCK_CHILD", &dir)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let stdout = child.0.stdout.take().unwrap();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    if line.unwrap() == "LOCKED" {
                        ready_tx.send(()).unwrap();
                        break;
                    }
                }
            });
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            reader.join().unwrap();
            assert!(lock_oauth_refresh_for("dead-holder", Duration::ZERO).is_err());
            let lock_file = dir.join("oauth-refresh-dead_holder.lock");
            assert!(lock_file.exists());
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            let _recovered = lock_oauth_refresh_for("dead-holder", Duration::ZERO).unwrap();
            assert!(lock_file.exists());
        });
    }

    #[test]
    fn refusing_cleartext_auth_does_not_echo_url_credentials() {
        // The refusal message reaches the activity UI, client error text, and logs. A
        // password in the URL would ride along into all three, which is the leak this
        // error was supposed to prevent in the first place.
        let err = require_secure_for_auth("http://user:hunter2@8.8.8.8/mcp")
            .expect_err("cleartext auth to a public host must be refused");

        assert!(
            !err.contains("hunter2"),
            "credentials leaked into the error: {err}"
        );
        assert!(
            err.contains("8.8.8.8"),
            "the host has to survive or the error is unactionable: {err}"
        );
    }

    #[test]
    fn a_cleartext_url_without_credentials_is_reported_as_written() {
        let err = require_secure_for_auth("http://8.8.8.8/mcp")
            .expect_err("cleartext auth to a public host must be refused");
        assert!(err.contains("http://8.8.8.8/mcp"), "got: {err}");
    }

    #[test]
    fn private_and_https_hosts_are_still_allowed() {
        // Redaction must not change which URLs are accepted.
        assert!(require_secure_for_auth("https://mcp.example.com/mcp").is_ok());
        assert!(require_secure_for_auth("http://127.0.0.1:4000/mcp").is_ok());
    }

    fn oauth_state(expires_at: Option<u64>, refresh_token: Option<&str>) -> OAuthState {
        OAuthState {
            issuer: Some("https://auth.example.com".into()),
            token_endpoint: "https://auth.example.com/token".into(),
            client_id: "client".into(),
            refresh_token: refresh_token.map(str::to_string),
            resource: Some("https://mcp.example.com".into()),
            scope: Some("files:read".into()),
            issued_at: Some(1_000),
            expires_at,
        }
    }

    #[test]
    fn refresh_decision_uses_expiry_safety_window() {
        assert_eq!(
            refresh_decision(&oauth_state(Some(1_061), Some("refresh")), 1_000),
            RefreshDecision::NotNeeded
        );
        assert_eq!(
            refresh_decision(&oauth_state(Some(1_060), Some("refresh")), 1_000),
            RefreshDecision::Refresh
        );
        assert_eq!(
            refresh_decision(&oauth_state(Some(999), Some("refresh")), 1_000),
            RefreshDecision::Refresh
        );
    }

    #[test]
    fn refresh_decision_requests_reauth_without_refresh_token() {
        assert_eq!(
            refresh_decision(&oauth_state(Some(1_060), None), 1_000),
            RefreshDecision::Reauthenticate
        );
        assert_eq!(
            refresh_decision(&oauth_state(None, None), 1_000),
            RefreshDecision::NotNeeded
        );
    }

    #[test]
    fn oauth_state_from_older_versions_keeps_unknown_expiry() {
        let state: OAuthState = serde_json::from_str(
            r#"{"token_endpoint":"https://auth.example.com/token","client_id":"client","refresh_token":"refresh","resource":"https://mcp.example.com"}"#,
        )
        .unwrap();

        assert_eq!(state.issued_at, None);
        assert_eq!(state.expires_at, None);
        assert_eq!(state.issuer, None);
        assert_eq!(state.scope, None);
        assert_eq!(refresh_decision(&state, 1_000), RefreshDecision::NotNeeded);
    }

    #[test]
    fn refresh_credentials_stay_bound_to_their_issuer() {
        let endpoints = |issuer: &str, token_endpoint: &str| oauth::Endpoints {
            issuer: issuer.into(),
            authorization_endpoint: "https://auth.example.com/authorize".into(),
            token_endpoint: token_endpoint.into(),
            registration_endpoint: None,
            scope: None,
            authorization_response_iss_parameter_supported: false,
            client_id_metadata_document_supported: false,
            token_endpoint_auth_methods_supported: None,
        };

        let rotated = endpoints(
            "https://auth.example.com",
            "https://auth.example.com/token-v2",
        );
        assert_eq!(
            issuer_bound_token_endpoint("https://auth.example.com", &rotated).unwrap(),
            "https://auth.example.com/token-v2"
        );

        let changed = endpoints(
            "https://other.example.com",
            "https://other.example.com/token",
        );
        assert!(issuer_bound_token_endpoint("https://auth.example.com", &changed).is_err());
    }

    #[test]
    fn auth_requires_https_for_public_hosts() {
        // IP literals so the private-host check needs no DNS (hermetic test).
        // A token must not ride cleartext to a public host.
        assert!(require_secure_for_auth("http://8.8.8.8/mcp").is_err());
        // https to anywhere is fine.
        assert!(require_secure_for_auth("https://8.8.8.8/mcp").is_ok());
        // Loopback / private over http is acceptable (local dev).
        assert!(require_secure_for_auth("http://127.0.0.1:8080/mcp").is_ok());
        assert!(require_secure_for_auth("http://192.168.1.10/mcp").is_ok());
        // An unresolvable host is not positively local. The refusal-side
        // predicate treats this as private, but that must never grant permission
        // to put a saved token on a cleartext connection.
        assert!(require_secure_for_auth("http://no-such-host-633.invalid/mcp").is_err());
    }

    #[test]
    fn link_local_detection() {
        assert!(host_is_link_local("169.254.169.254")); // v4 cloud metadata
        assert!(host_is_link_local("169.254.0.1"));
        assert!(host_is_link_local("fe80::1")); // v6 link-local
        assert!(host_is_link_local("fd00:ec2::254")); // AWS v6 metadata (ULA)
        assert!(host_is_link_local("::ffff:169.254.169.254")); // IPv4-mapped metadata
        assert!(!host_is_link_local("127.0.0.1"));
        assert!(!host_is_link_local("::1")); // v6 loopback is not metadata
        assert!(!host_is_link_local("10.0.0.1"));
        assert!(!host_is_link_local("8.8.8.8"));
        assert!(!host_is_link_local("2606:4700:4700::1111")); // public v6
    }

    #[test]
    fn untrusted_sources() {
        assert!(is_untrusted_source(Some("shared")));
        assert!(is_untrusted_source(Some("registry")));
        assert!(!is_untrusted_source(Some("user")));
        assert!(!is_untrusted_source(Some("manual")));
        assert!(!is_untrusted_source(Some("curated")));
        assert!(!is_untrusted_source(Some("imported:cursor")));
        assert!(!is_untrusted_source(None));
    }

    fn remote_server(url: &str, source: Option<&str>) -> ServerEntry {
        ServerEntry {
            enabled: false,
            inherit_env: false,
            id: "t".into(),
            name: "Test".into(),
            transport: "http".into(),
            command: None,
            args: vec![],
            env: vec![],
            url: Some(url.into()),
            source: source.map(String::from),
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            request_timeout_ms: None,
            initialize_timeout_ms: None,
            launch: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    #[test]
    fn request_timeout_uses_the_legacy_default_or_the_server_override() {
        let mut server = remote_server("https://mcp.example.com/mcp", None);
        assert_eq!(request_timeout(&server).unwrap(), Duration::from_secs(30));

        server.request_timeout_ms = Some(65_000);
        assert_eq!(
            request_timeout(&server).unwrap(),
            Duration::from_millis(65_000)
        );

        server.request_timeout_ms = Some(0);
        assert_eq!(
            request_timeout(&server).unwrap_err(),
            "requestTimeoutMs must be greater than zero"
        );

        server.request_timeout_ms = Some(crate::registry::MAX_REQUEST_TIMEOUT_MS);
        assert_eq!(
            request_timeout(&server).unwrap(),
            Duration::from_millis(crate::registry::MAX_REQUEST_TIMEOUT_MS)
        );

        server.request_timeout_ms = Some(crate::registry::MAX_REQUEST_TIMEOUT_MS + 1);
        assert_eq!(
            request_timeout(&server).unwrap_err(),
            format!(
                "requestTimeoutMs must not exceed {} (24 hours)",
                crate::registry::MAX_REQUEST_TIMEOUT_MS
            )
        );
    }

    #[test]
    fn guard_blocks_metadata_even_for_user_added() {
        let s = remote_server("http://169.254.169.254/latest/meta-data/", Some("user"));
        assert!(guard_connect_target(&s).is_err());
    }

    #[test]
    fn guard_blocks_private_for_untrusted_source() {
        let s = remote_server("http://127.0.0.1:6379/", Some("shared"));
        assert!(guard_connect_target(&s).is_err());
    }

    #[test]
    fn guard_allows_localhost_for_user_added() {
        let s = remote_server("http://127.0.0.1:8080/mcp", Some("user"));
        assert!(guard_connect_target(&s).is_ok());
    }

    #[test]
    fn guard_allows_public_host_for_any_source() {
        let s = remote_server("https://8.8.8.8/mcp", Some("shared"));
        assert!(guard_connect_target(&s).is_ok());
    }

    // ----- SBS-524: client-credentials wiring ---------------------------------

    fn cc(client_id: &str) -> crate::registry::ClientCredentials {
        crate::registry::ClientCredentials {
            client_id: client_id.into(),
            ..Default::default()
        }
    }

    fn http_server(id: &str, cc: Option<crate::registry::ClientCredentials>) -> ServerEntry {
        let mut s = remote_server("https://mcp.example.com/mcp", None);
        s.id = id.into();
        s.client_credentials = cc;
        s
    }

    /// The registry file, its backups and its exports must never carry the client
    /// secret. Only the vault does. This asserts the shape rather than trusting
    /// that no one adds a `clientSecret` field later.
    #[test]
    fn client_credentials_config_serializes_without_any_secret() {
        let mut config = cc("client-abc");
        config.token_endpoint_auth_method = Some("client_secret_basic".into());
        config.scope = Some("mcp:read mcp:write".into());

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"clientId\":\"client-abc\""), "{json}");
        assert!(
            json.contains("\"tokenEndpointAuthMethod\":\"client_secret_basic\""),
            "{json}"
        );
        assert!(
            !json.to_ascii_lowercase().contains("secret\":"),
            "the registry must not carry a client secret: {json}"
        );

        let back: crate::registry::ClientCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(back, config);
    }

    /// A newer build's fields survive a round-trip through this one, same contract
    /// as the rest of the registry.
    #[test]
    fn client_credentials_config_preserves_unknown_fields() {
        let json = r#"{"clientId":"c","somethingNewer":{"a":1}}"#;
        let parsed: crate::registry::ClientCredentials = serde_json::from_str(json).unwrap();
        let out = serde_json::to_string(&parsed).unwrap();
        assert!(out.contains("somethingNewer"), "{out}");
    }

    /// The flow is selected by configuration, and a blank client id does not
    /// select it: an empty block would otherwise send every connect down the
    /// headless path and fail with "no client secret vaulted".
    #[test]
    fn client_credentials_flow_requires_a_non_empty_client_id() {
        assert!(uses_client_credentials(&http_server(
            "a",
            Some(cc("client-abc"))
        )));
        assert!(!uses_client_credentials(&http_server("b", Some(cc("   ")))));
        assert!(!uses_client_credentials(&http_server("c", Some(cc("")))));
        assert!(!uses_client_credentials(&http_server("d", None)));
    }

    #[test]
    fn client_credentials_state_round_trips_and_tolerates_older_vaulted_shapes() {
        let state = ClientCredentialsState {
            issuer: "https://auth.example.com".into(),
            token_endpoint: "https://auth.example.com/token".into(),
            client_id: "client-abc".into(),
            method: "client_secret_basic".into(),
            scope: Some("mcp:read".into()),
            resource: "https://mcp.example.com/mcp".into(),
            expires_at: Some(1_700_000_000),
        };
        let json = serde_json::to_string(&state).unwrap();
        // Assert the exact key set rather than grepping for "secret": the auth
        // METHOD is legitimately named `client_secret_basic`, so a substring check
        // both false-positives here and would miss a field named anything else.
        let keys: std::collections::BTreeSet<String> =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json)
                .unwrap()
                .keys()
                .cloned()
                .collect();
        assert_eq!(
            keys,
            [
                "issuer",
                "token_endpoint",
                "client_id",
                "method",
                "scope",
                "resource",
                "expires_at"
            ]
            .iter()
            .map(|k| k.to_string())
            .collect::<std::collections::BTreeSet<_>>(),
            "vaulted state grew a field; make sure it is not a credential: {json}"
        );
        let back: ClientCredentialsState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.issuer, state.issuer);
        assert_eq!(back.method, state.method);
        assert_eq!(back.expires_at, state.expires_at);

        // A provider that reports no lifetime keeps the reactive 401/403 path.
        let minimal: ClientCredentialsState = serde_json::from_str(
            r#"{"issuer":"https://a","tokenEndpoint":"https://a/t","clientId":"c",
                "method":"client_secret_post","resource":"https://r"}"#
                .replace("tokenEndpoint", "token_endpoint")
                .replace("clientId", "client_id")
                .as_str(),
        )
        .unwrap();
        assert_eq!(minimal.expires_at, None);
        assert_eq!(minimal.scope, None);
    }

    // ----- SBS-615: exact resource rebinding after a URL edit ------------------

    /// The comparison is EXACT, and that is the whole point: RFC 8707 binds the
    /// token to the resource string, and a URL path is case-sensitive, so
    /// `/MCP` and `/mcp` are different resources. Folding case here would keep a
    /// token minted for the old one and the user's edit would appear to do nothing.
    #[test]
    fn resource_rebinding_compares_the_url_exactly() {
        let vaulted = "https://mcp.example.com/MCP";

        assert!(
            !resource_binding_changed(vaulted, "https://mcp.example.com/MCP"),
            "the same URL must not force a pointless re-acquisition"
        );
        assert!(
            resource_binding_changed(vaulted, "https://mcp.example.com/mcp"),
            "a path differing only in case is a different resource"
        );
        // Case anywhere else counts as changed too. Over-reporting is the safe
        // direction: re-acquiring is cheap and non-interactive by construction.
        assert!(resource_binding_changed(
            vaulted,
            "https://MCP.example.com/MCP"
        ));
        assert!(resource_binding_changed(
            vaulted,
            "https://mcp.example.com/MCP/v2"
        ));
    }

    /// Surrounding whitespace is not a resource change: a URL pasted with a
    /// trailing newline would otherwise re-acquire on every single connect.
    #[test]
    fn resource_rebinding_ignores_surrounding_whitespace_only() {
        assert!(!resource_binding_changed(
            "  https://mcp.example.com/MCP\n",
            "https://mcp.example.com/MCP"
        ));
        assert!(!resource_binding_changed(
            "https://mcp.example.com/MCP",
            "\thttps://mcp.example.com/MCP  "
        ));
        // Trimming must not reach inside the URL and mask a real edit.
        assert!(resource_binding_changed(
            "  https://mcp.example.com/MCP  ",
            "  https://mcp.example.com/mcp  "
        ));
    }

    /// Points the vault at a scratch dir and the file backend at a known key, so a
    /// test can write real `ClientCredentialsState` and read it back.
    ///
    /// Holds `data_dir_test_lock` for the whole test: the data-dir override and the
    /// backend-selecting env var are both process-global.
    ///
    /// Field order IS drop order (unlike locals, struct fields drop in declaration
    /// order), so the override is declared before the guard that protects it. The
    /// other way round, teardown released the lock with the override still
    /// installed, and this drop could then clear the override the NEXT test had
    /// just installed, sending that test at the REAL data dir.
    struct VaultFixture {
        _override: crate::registry::DataDirOverride,
        _data_dir_lock: std::sync::MutexGuard<'static, ()>,
        dir: std::path::PathBuf,
        previous_key: Option<String>,
    }

    impl VaultFixture {
        fn new(name: &str) -> Self {
            let lock = crate::registry::data_dir_test_lock();
            let dir = std::env::temp_dir().join(format!(
                "toolport-sbs615-{name}-{}-{}",
                std::process::id(),
                now_epoch_seconds()
            ));
            std::fs::create_dir_all(&dir).expect("scratch data dir");
            let over = crate::registry::DataDirOverride::set(&dir);
            let previous_key = std::env::var("TOOLPORT_SECRET_KEY").ok();
            std::env::set_var("TOOLPORT_SECRET_KEY", "sbs-615-unit-test-passphrase");
            Self {
                _override: over,
                _data_dir_lock: lock,
                dir,
                previous_key,
            }
        }

        fn vault_state(&self, server_id: &str, resource: &str) {
            let state = ClientCredentialsState {
                issuer: "https://auth.example.com".into(),
                token_endpoint: "https://auth.example.com/token".into(),
                client_id: "client-abc".into(),
                method: "client_secret_basic".into(),
                scope: None,
                resource: resource.into(),
                expires_at: Some(9_999_999_999),
            };
            secrets::set_secret(
                server_id,
                CC_STATE_KEY,
                &serde_json::to_string(&state).expect("state serializes"),
            )
            .expect("scratch vault write");
        }
    }

    impl Drop for VaultFixture {
        fn drop(&mut self) {
            match self.previous_key.take() {
                Some(v) => std::env::set_var("TOOLPORT_SECRET_KEY", v),
                None => std::env::remove_var("TOOLPORT_SECRET_KEY"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// End to end over the real vault: state pinned to `/MCP`, entry edited to
    /// `/mcp`. The connect path must call this stale and reset, or the next
    /// acquisition would keep presenting a token bound to the old resource.
    #[test]
    fn a_url_edit_that_only_changes_path_case_rebinds_the_credential() {
        let vault = VaultFixture::new("rebind");
        let mut server = http_server("sbs615-rebind", Some(cc("client-abc")));
        server.url = Some("https://mcp.example.com/MCP".into());
        let server_id = server.id.clone();
        vault.vault_state(&server_id, "https://mcp.example.com/MCP");

        // Unchanged URL: nothing to reset, or every connect would re-acquire.
        assert!(!client_credentials_resource_changed(
            &server_id,
            "https://mcp.example.com/MCP"
        ));
        assert!(!client_credentials_state_is_stale(
            &server,
            &server_id,
            "https://mcp.example.com/MCP"
        )
        .expect("readable vault"));

        // The edit: same host, same everything but the path case.
        let edited = "https://mcp.example.com/mcp";
        server.url = Some(edited.into());
        assert!(client_credentials_resource_changed(&server_id, edited));
        assert!(
            client_credentials_state_is_stale(&server, &server_id, edited).expect("readable vault"),
            "the connect path must treat the vaulted state as stale"
        );

        // What the connect path then does. After it, nothing is left to reuse, so
        // the next acquisition binds to the URL actually being contacted.
        reset_client_credentials(&server_id).expect("reset");
        for key in [CC_STATE_KEY, secrets::HTTP_AUTH_KEY] {
            assert!(
                secrets::get_secret_result(&server_id, key)
                    .expect("readable vault")
                    .is_none(),
                "{key} must be gone after the reset"
            );
        }
    }

    /// Removing the config is the other way state goes stale, and it must not
    /// depend on the URL having changed.
    #[test]
    fn vaulted_state_without_a_configured_flow_is_stale_at_the_same_url() {
        let vault = VaultFixture::new("deconfigured");
        let url = "https://mcp.example.com/MCP";
        let server = http_server("sbs615-deconfigured", None);
        let server_id = server.id.clone();
        vault.vault_state(&server_id, url);

        assert!(!client_credentials_resource_changed(&server_id, url));
        assert!(
            client_credentials_state_is_stale(&server, &server_id, url).expect("readable vault"),
            "state for a server no longer configured for the flow must be discarded"
        );
    }

    /// No vaulted state means nothing to rebind: a server being configured for the
    /// first time must not report a change and must not attempt a reset.
    #[test]
    fn an_empty_vault_reports_no_resource_change() {
        let _vault = VaultFixture::new("empty");
        let server = http_server("sbs615-empty", Some(cc("client-abc")));
        let server_id = server.id.clone();

        assert!(!client_credentials_resource_changed(
            &server_id,
            "https://mcp.example.com/mcp"
        ));
        assert!(!client_credentials_state_is_stale(
            &server,
            &server_id,
            "https://mcp.example.com/mcp"
        )
        .expect("readable vault"));
    }

    // ----- SBS-840: a vault read failure is not missing OAuth/CC state ---------

    /// The reserved namespace makes `get_secret_result` return `Err` without
    /// touching a real keychain (same trick as SBS-841).
    const RESERVED_VAULT_NS: &str = "__toolport_internal__";

    #[test]
    fn decode_vaulted_json_distinguishes_missing_from_a_failed_read() {
        assert!(
            matches!(
                decode_vaulted_json::<OAuthState>(Ok(None), "OAuth state"),
                Ok(None)
            ),
            "confirmed-missing must stay Ok(None)"
        );

        let Err(err) =
            decode_vaulted_json::<OAuthState>(Err("keychain locked".into()), "OAuth state")
        else {
            panic!("a failed read must be Err, not missing");
        };
        assert!(
            err.contains("could not read the vaulted OAuth state"),
            "must describe a read failure: {err}"
        );
        assert!(
            err.contains("keychain locked"),
            "must keep the underlying vault error: {err}"
        );
        assert!(
            !err.contains("no stored OAuth state"),
            "a vault failure must not look like missing state: {err}"
        );

        let Err(parse_err) = decode_vaulted_json::<OAuthState>(Ok(Some("{".into())), "OAuth state")
        else {
            panic!("unreadable stored JSON is an error, not missing");
        };
        assert!(
            parse_err.contains("could not parse the vaulted OAuth state"),
            "must describe a parse failure: {parse_err}"
        );
        assert!(
            !parse_err.contains("no stored OAuth state"),
            "corrupt stored state is not missing: {parse_err}"
        );

        assert!(
            matches!(
                decode_vaulted_json::<ClientCredentialsState>(Ok(None), "client-credentials state"),
                Ok(None)
            ),
            "confirmed-missing CC state must stay Ok(None)"
        );
        let Err(cc_err) = decode_vaulted_json::<ClientCredentialsState>(
            Err("keychain locked".into()),
            "client-credentials state",
        ) else {
            panic!("a failed CC-state read must be Err");
        };
        assert!(
            cc_err.contains("could not read the vaulted client-credentials state"),
            "{cc_err}"
        );
        assert!(
            !cc_err.contains("no client-credentials state"),
            "a vault failure must not look like missing CC state: {cc_err}"
        );
    }

    #[test]
    fn load_state_on_reserved_namespace_is_a_read_failure_not_missing() {
        let Err(err) = load_state(RESERVED_VAULT_NS) else {
            panic!("reserved namespace must fail the vault read");
        };
        assert!(
            err.contains("could not read the vaulted OAuth state"),
            "must describe a read failure: {err}"
        );
        assert!(
            !err.contains("no stored OAuth state"),
            "a vault failure must not look like missing state: {err}"
        );
    }

    #[test]
    fn refresh_token_reports_a_vault_read_failure_not_missing_state() {
        let _data = crate::registry::DataDirTestEnv::new(
            "refresh_token_reports_a_vault_read_failure_not_missing_state",
        );
        let err = refresh_token(RESERVED_VAULT_NS, None)
            .expect_err("reserved namespace must fail the vault read");
        let lower = err.to_lowercase();
        assert!(
            lower.contains("could not read")
                && (lower.contains("vault") || lower.contains("state")),
            "must describe a vault/state read failure: {err}"
        );
        assert!(
            !err.contains("no stored OAuth state"),
            "a vault failure must not look like missing state: {err}"
        );
        assert!(
            !is_auth_error(&err),
            "a locked vault must not be classified as needs-authentication: {err}"
        );
    }

    #[test]
    fn refresh_token_if_needed_reports_a_vault_read_failure_not_ok_none() {
        let _data = crate::registry::DataDirTestEnv::new(
            "refresh_token_if_needed_reports_a_vault_read_failure_not_ok_none",
        );
        let result = refresh_token_if_needed(RESERVED_VAULT_NS);
        assert!(
            matches!(result, Err(_)),
            "a failed state read must not skip refresh as if there is no state: {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            !err.contains("no stored OAuth state"),
            "a vault failure must not look like missing state: {err}"
        );
    }

    #[test]
    fn acquire_client_credentials_reports_a_vault_read_failure_not_missing_secret() {
        let Err(err) = acquire_client_credentials(
            RESERVED_VAULT_NS,
            "https://mcp.example.com/mcp",
            &cc("client-abc"),
        ) else {
            panic!("reserved namespace must fail the client-secret read");
        };
        assert!(
            err.contains("could not read the vaulted client secret"),
            "must describe a read failure: {err}"
        );
        assert!(
            !err.contains("no client secret is vaulted"),
            "a vault failure must not look like a missing client secret: {err}"
        );
    }

    #[test]
    fn reacquire_client_credentials_reports_a_vault_read_failure_not_missing_state() {
        let Err(err) = reacquire_client_credentials(RESERVED_VAULT_NS) else {
            panic!("reserved namespace must fail the CC-state read");
        };
        assert!(
            err.contains("could not read the vaulted client-credentials state"),
            "must describe a read failure: {err}"
        );
        assert!(
            !err.contains("no client-credentials state"),
            "a vault failure must not look like missing CC state: {err}"
        );
        assert!(
            !err.contains("the vaulted client secret is gone"),
            "must fail on the state read, not claim the secret is gone: {err}"
        );
    }

    /// A failed CC-state read used to read as "there is no state here", which skips
    /// [`reset_client_credentials`] and lets the connect present a token RFC 8707
    /// bound to the OLD resource to the new one.
    #[test]
    fn client_credentials_staleness_reports_a_vault_read_failure_not_absent_state() {
        let server = http_server("sbs840-stale", Some(cc("client-abc")));
        let Err(err) = client_credentials_state_is_stale(
            &server,
            RESERVED_VAULT_NS,
            "https://mcp.example.com/mcp",
        ) else {
            panic!("reserved namespace must fail the CC-state read, not answer 'not stale'");
        };
        assert!(
            err.contains("could not read the vaulted client-credentials state"),
            "must describe a read failure: {err}"
        );
        assert!(
            !err.contains("no client-credentials state"),
            "a vault failure must not look like missing CC state: {err}"
        );
    }

    /// The SOU-474 guard. A failed read must not answer "the transport did not
    /// refresh", because the caller acts on that by spending another exchange on a
    /// refresh token the transport may already have consumed.
    #[test]
    fn a_failed_post_connect_token_read_is_not_a_missed_refresh() {
        let Err(err) = transport_refreshed_during_connect(RESERVED_VAULT_NS, Some("sent-token"))
        else {
            panic!("reserved namespace must fail the access-token read");
        };
        assert!(
            err.contains("could not read the vaulted access token"),
            "must describe a read failure: {err}"
        );
        assert!(
            !is_auth_error(&err),
            "a locked vault must not be classified as needs-authentication: {err}"
        );
    }

    /// The guard's answers over the real vault, so the fix above cannot regress into
    /// always reporting a refresh (which would stop the legitimate retry).
    #[test]
    fn a_rotated_vaulted_token_is_the_only_evidence_of_a_transport_refresh() {
        let _vault = VaultFixture::new("sou474");
        let server_id = "sbs840-guard";

        // Nothing vaulted is not evidence of anything: the retry must still be free
        // to run for a server whose token was cleared mid-connect.
        assert!(
            !transport_refreshed_during_connect(server_id, Some("sent-token"))
                .expect("readable vault"),
            "an empty vault is not a refresh"
        );

        secrets::set_secret(server_id, secrets::HTTP_AUTH_KEY, "sent-token")
            .expect("scratch vault write");
        assert!(
            !transport_refreshed_during_connect(server_id, Some("sent-token"))
                .expect("readable vault"),
            "the token we sent is still there, so the transport spent no refresh"
        );

        secrets::set_secret(server_id, secrets::HTTP_AUTH_KEY, "rotated-token")
            .expect("scratch vault write");
        assert!(
            transport_refreshed_during_connect(server_id, Some("sent-token"))
                .expect("readable vault"),
            "a different vaulted token means the transport already refreshed"
        );

        let _ = secrets::delete_secret(server_id, secrets::HTTP_AUTH_KEY);
    }
}

#[cfg(test)]
mod reference_redaction_tests {
    use super::*;
    #[test]
    fn handshake_requests_do_not_expose_resolved_credentials() {
        let server: ServerEntry = serde_json::from_value(serde_json::json!({"id":"refs", "name":"Refs", "transport":"stdio", "command":"mock", "env":[{"key":"KEY", "secret":true, "value":"synthetic-ref-value", "source":{"ref":"env:SYNTHETIC"}}]})).unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let handler = protect_server_requests(
            &server,
            Arc::new(move |frame| {
                assert!(!frame.to_string().contains("synthetic-ref-value"));
                observed.fetch_add(1, Ordering::Relaxed);
                None
            }),
        );
        handler(
            &serde_json::json!({"method":"elicitation/create", "params":{"message":"synthetic-ref-value"}}),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reference_errors_are_opaque_and_auth_triggers_reconnection() {
        let reference = "op://auth-test/item/key";
        assert_eq!(crate::secret_refs::test_cached_value(reference, "old"), "old");
        let redaction = Redaction(vec!["synthetic-ref-value".into()], vec![reference.into()]);
        let e = redaction.connection_error(
            crate::downstream::TransportError::Fatal("tail ends in ref-value".into()),
            true,
        );
        assert!(!e.to_string().contains("ref-value"));
        assert!(e.is_health_failure());
        let e = redaction.connection_error(
            crate::downstream::TransportError::Classified(
                crate::call_failure::CallFailureKind::Auth {
                    target: crate::call_failure::AuthTarget::Endpoint,
                },
                "HTTP 401 synthetic-ref-value".into(),
            ),
            true,
        );
        assert!(e.is_health_failure());
        assert_eq!(crate::secret_refs::test_cached_value(reference, "new"), "new");
        assert!(!e.to_string().contains("synthetic-ref-value"));
        let payload=redaction.value(serde_json::json!({"content":[{"text":"synthetic-ref-value"}],"synthetic-ref-value":"synthetic-ref-value"}));
        assert!(!payload.to_string().contains("synthetic-ref-value"));
    }
}

/// Prevent unsolicited progress and resource notifications from echoing injected keys.
pub fn protect_progress(server: &ServerEntry, sink: Option<ProgressSink>) -> Option<ProgressSink> {
    if !crate::secret_refs::has_references(server) {
        return sink;
    }
    let redact = Redaction::for_server(server);
    sink.map(|sink| Arc::new(move |value| sink(redact.value(value))) as ProgressSink)
}
pub fn protect_resource_updates(
    server: &ServerEntry,
    sink: Option<ResourceUpdatedSink>,
) -> Option<ResourceUpdatedSink> {
    if !crate::secret_refs::has_references(server) {
        return sink;
    }
    let redact = Redaction::for_server(server);
    sink.map(|sink| Arc::new(move |uri| sink(redact.text(uri))) as ResourceUpdatedSink)
}

/// Redact handshake-time requests before the protected transport is installed.
pub fn protect_server_requests(
    server: &ServerEntry,
    handler: ServerRequestHandler,
) -> ServerRequestHandler {
    if !crate::secret_refs::has_references(server) {
        return handler;
    }
    let redact = Redaction::for_server(server);
    Arc::new(move |frame| handler(&redact.value(frame.clone())))
}
