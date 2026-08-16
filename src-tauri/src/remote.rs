//! Remote (http) server connection with automatic OAuth token refresh.
//!
//! When a connection fails with an auth error and we have a stored refresh
//! token, we transparently refresh the access token and retry once. The OAuth
//! state (token endpoint, client id, refresh token) is vaulted alongside the
//! access token.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
/// Best-effort by design: with no resolvable data dir there is nowhere to put a lock, and
/// refusing to refresh at all would be worse than the race it prevents. Contention waits long
/// enough to cover the OAuth client's 30-second request timeout and metadata refresh before
/// failing open visibly rather than silently dropping back to the original race (SBS-705).
const OAUTH_REFRESH_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(65);

fn lock_oauth_refresh_for(
    server_id: &str,
    timeout: std::time::Duration,
) -> Result<Option<crate::registry::FileLock>, String> {
    let Some(dir) = crate::registry::conduit_dir() else {
        return Ok(None);
    };
    let leaf = format!(
        "oauth-refresh-{}.lock",
        crate::router::sanitize_segment(server_id)
    );
    crate::registry::lock_at_for(&dir.join(leaf), timeout).map(Some)
}

fn lock_oauth_refresh(server_id: &str) -> Option<crate::registry::FileLock> {
    match lock_oauth_refresh_for(server_id, OAUTH_REFRESH_LOCK_TIMEOUT) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!(
                "toolport: OAuth refresh for {server_id:?} is proceeding without the cross-process lock after waiting {}s: {error}",
                OAUTH_REFRESH_LOCK_TIMEOUT.as_secs()
            );
            None
        }
    }
}

/// After winning the refresh lock, decide whether another process already did the work.
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

fn refresh_token_with_expiry(server_id: &str) -> Result<RefreshedToken, String> {
    // Snapshot before locking: the comparison after we win is what tells us whether a
    // racing process rotated the credential while we waited.
    // A vault read failure is not "no token" (SBS-840).
    let before_access = match secrets::get_secret_result(server_id, secrets::HTTP_AUTH_KEY) {
        Ok(v) => v,
        Err(e) => return Err(format!("could not read the vaulted access token: {e}")),
    };
    // Held for the whole function, including the client-credentials branch, so two
    // processes cannot mint two tokens for the same server.
    let _refresh_lock = lock_oauth_refresh(server_id);
    if let Some(winner) = refreshed_while_waiting(server_id, before_access.as_deref())? {
        return Ok(winner);
    }
    // Client-credentials servers have no refresh token by construction, so they
    // reacquire instead. Checked first because this is the seam BOTH the proactive
    // pre-expiry path and the reactive 401/403 retry go through; branching here
    // means neither has to know which flow a server uses.
    // A failed CC-state read must not fall through to interactive refresh (SBS-840).
    if load_cc_state(server_id)?.is_some() {
        return reacquire_client_credentials(server_id);
    }
    let state = match load_state(server_id) {
        Ok(Some(s)) => s,
        Ok(None) => return Err("no stored OAuth state to refresh".to_string()),
        Err(e) => return Err(e),
    };
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
            let endpoints = oauth::discover(resource).map_err(|e| {
                format!("could not verify the stored OAuth issuer; needs authentication: {e}")
            })?;
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
    secrets::set_secret(server_id, STATE_KEY, &json)?;
    secrets::set_secret(server_id, secrets::HTTP_AUTH_KEY, &tokens.access_token)?;
    Ok(RefreshedToken {
        access_token: tokens.access_token,
        expires_at: tokens.expires_at,
    })
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

pub fn refresh_token(server_id: &str) -> Result<String, String> {
    refresh_token_with_expiry(server_id).map(|token| token.access_token)
}

/// Refresh before the known expiry. A legacy/provider state with no expiry is a
/// no-op and continues to use the 401/403 fallback. If the deadline is close but
/// no refresh token exists, return an auth-classified error so the existing
/// per-server "Needs sign-in" UI appears before a failed tool call.
fn refresh_token_if_needed(server_id: &str) -> Result<Option<String>, String> {
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
            return Ok(refresh_token(server_id).ok());
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
        // The token may still be valid throughout the safety window. A transient
        // refresh failure falls back to it; a real 401/403 forces another refresh.
        RefreshDecision::Refresh => Ok(refresh_token(server_id).ok()),
        RefreshDecision::Reauthenticate => Err(
            "OAuth access token expires soon and no refresh token is available; needs authentication"
                .to_string(),
        ),
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
) -> Result<HttpTransport, String> {
    if token.is_some() {
        require_secure_for_auth(url)?;
    }
    // Shared by ordinary refresh and scope step-up so a newly-authorized token's
    // expiry replaces the previous token's proactive deadline immediately.
    // A failed state read must not silently disable proactive refresh (SBS-840).
    let oauth_state = load_state(server_id)?;
    let refresh_at = oauth_state
        .as_ref()
        .and_then(|state| state.expires_at)
        .map(|expires_at| expires_at.saturating_sub(PROACTIVE_REFRESH_SKEW_SECS));
    let next_refresh_at = Arc::new(Mutex::new(refresh_at));
    // The request path and the background subscription listener can refresh or
    // step up concurrently. Serialize credential-changing flows so an older
    // refresh result cannot overwrite a newer interactive authorization state.
    let credential_update = Arc::new(Mutex::new(()));
    let refresh: Option<RefreshFn> = if token.is_some() {
        let sid = server_id.to_string();
        // Keep the proactive deadline in memory. This avoids a keychain read on
        // every tool call while still updating the deadline after each refresh.
        let next_refresh_at = Arc::clone(&next_refresh_at);
        let credential_update = Arc::clone(&credential_update);
        Some(Box::new(move |force| {
            let _update = credential_update
                .lock()
                .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
            if !force {
                let deadline = *next_refresh_at
                    .lock()
                    .map_err(|_| "OAuth refresh deadline lock poisoned".to_string())?;
                match deadline {
                    Some(refresh_at) if now_epoch_seconds() >= refresh_at => {}
                    _ => return Ok(None),
                }
            }

            let refreshed = match refresh_token_with_expiry(&sid) {
                Ok(refreshed) => refreshed,
                Err(e) => {
                    if !force {
                        *next_refresh_at
                            .lock()
                            .map_err(|_| "OAuth refresh deadline lock poisoned".to_string())? =
                            Some(now_epoch_seconds().saturating_add(PROACTIVE_REFRESH_RETRY_SECS));
                    }
                    // A locked keychain is not "please sign in again" (SBS-840).
                    if e.contains("could not read the vaulted")
                        || e.contains("could not parse the vaulted")
                    {
                        return Err(e);
                    }
                    return Err(format!(
                        "OAuth token refresh failed; needs authentication: {e}"
                    ));
                }
            };
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
            let _update = credential_update
                .lock()
                .map_err(|_| "OAuth credential-update lock poisoned".to_string())?;
            let token = reauthorize_for_scope(&sid, &resource, scope)?;
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
    let mut transport = HttpTransport::guarded(url, token, refresh, block_private);
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
    Ok(transport)
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
        if e.secret && e.value.is_none() {
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
    guard_connect_target(server)?;
    let url = server.url.as_deref().unwrap_or("");
    let server_id = &server.id;
    // Untrusted-provenance servers also get private/loopback refused at the resolver,
    // matching `guard_connect_target`'s pre-check but closing the DNS-rebind TOCTOU.
    let block_private = is_untrusted_source(server.source.as_deref());
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
    if client_credentials_state_is_stale(server, server_id, url)? {
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
    // A vault read failure is not "no token" (SBS-789): connecting anonymous on a
    // locked keychain would surface as a bogus 401/"needs sign-in" and can hand an
    // unauthenticated session to a server the user believes is authenticated.
    let stored_auth = match secrets::get_secret_result(server_id, secrets::HTTP_AUTH_KEY) {
        Ok(Some(v)) => Some(v),
        Ok(None) => first_vaulted_secret(server)
            .map_err(|e| format!("could not read the vaulted auth token: {e}"))?,
        Err(e) => return Err(format!("could not read the vaulted auth token: {e}")),
    };
    let auth = match refresh_token_if_needed(server_id)? {
        Some(fresh) => Some(fresh),
        None => stored_auth,
    };
    // Remember exactly what we hand the transport. The transport force-refreshes
    // internally on a 401/403 and vaults the result, so if the vaulted token
    // differs from this afterwards, an exchange already happened during this
    // connect (SOU-474).
    let sent_auth = auth.clone();
    let mut transport = authed_transport(url, auth, server_id, block_private)?;
    if let Some(ref handler) = server_handler {
        transport.set_server_request_handler(handler.clone());
    }
    transport.set_resource_updated_sink(resource_updated.clone());
    transport.set_progress_sink(progress.clone());
    transport.set_change_sink(change_dirty.clone());
    match DownstreamServer::connect(server_id.to_string(), Box::new(transport)) {
        Ok(ds) => Ok(ds),
        Err(e) if is_auth_error(&e) => {
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
            let already_refreshed =
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
                };
            if already_refreshed {
                return Err(e);
            }
            match refresh_token(server_id) {
                Ok(fresh) => {
                    let mut transport =
                        authed_transport(url, Some(fresh), server_id, block_private)?;
                    if let Some(handler) = server_handler.clone() {
                        transport.set_server_request_handler(handler);
                    }
                    transport.set_resource_updated_sink(resource_updated);
                    transport.set_progress_sink(progress);
                    transport.set_change_sink(change_dirty);
                    DownstreamServer::connect(server_id.to_string(), Box::new(transport))
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
        let contention = match lock_oauth_refresh_for(
            "server-a",
            std::time::Duration::from_millis(40),
        ) {
            Err(error) => error,
            Ok(_) => panic!("contention must remain distinguishable from a missing data dir"),
        };
        assert!(contention.contains("locked by another Toolport process"));
        assert!(
            OAUTH_REFRESH_LOCK_TIMEOUT >= std::time::Duration::from_secs(30),
            "the production wait must cover the token client's request timeout"
        );
        drop((a, b));

        assert!(
            lock_oauth_refresh("server-a").is_some(),
            "the lock must be reacquirable once released"
        );

        let _ = std::fs::remove_dir_all(&dir);
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
            unknown_fields: serde_json::Map::new(),
        }
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
        assert!(resource_binding_changed(vaulted, "https://MCP.example.com/MCP"));
        assert!(resource_binding_changed(vaulted, "https://mcp.example.com/MCP/v2"));
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
        let err = refresh_token(RESERVED_VAULT_NS)
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
