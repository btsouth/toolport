//! Human-in-the-loop (HITL) tool-approval: the contract both sides share.
//!
//! Legacy clients and code-mode calls use the app's approval broker and block for the
//! decision. Modern direct calls use MCP multi-round-trip elicitation instead: the client
//! returns the human's answer on a fresh request, so no gateway request remains held.
//! Arguments never touch disk, and every path remains fail-closed.
//!
//! This module is the piece both the gateway-side client and the app-side broker share:
//! the wire types, the gating policy, and the on-disk endpoint descriptor. Keeping it in
//! the lib means there is exactly one definition of the protocol.

use std::io::{self, BufRead, Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

/// The broker descriptor's filename inside the Conduit data dir. The app writes it on
/// startup; every gateway process reads it. It holds ONLY the endpoint address and an auth
/// token, never any call payload.
pub const ENDPOINT_FILE: &str = "approval-endpoint.json";

/// Fail-closed timeout for a pending approval: if no human decides within this window, the
/// call is denied. (Configurable later; a sensible default for v1.)
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// The broker endpoint descriptor the app publishes and gateways read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointDescriptor {
    /// The loopback TCP address a gateway connects to (`127.0.0.1:PORT`). Always present,
    /// so a gateway from before [`Self::unix_endpoint`] existed - which reads only this field
    /// and dials only TCP - still finds the broker.
    pub endpoint: String,
    /// On Unix, the broker's socket file as well (`unix:/absolute/path`, see
    /// [`UNIX_ENDPOINT_PREFIX`]). A gateway that knows the field prefers it and falls back to
    /// `endpoint` if the connect fails. Additive and optional on purpose: long-lived
    /// client-spawned gateways outlive app updates, and one that predates this field must not
    /// be cut off from the broker by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_endpoint: Option<String>,
    /// A random token both sides hold. The gateway presents it in every request, and the
    /// broker proves it holds it before the gateway sends anything (see [`dial_broker`]).
    /// Defense-in-depth over the local-user filesystem trust boundary: only a process that
    /// can read the Conduit data dir (same as secrets) can obtain it.
    pub token: String,
}

/// Why a call was gated, surfaced in the approval UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReason {
    /// The tool is annotated `destructiveHint: true`.
    Destructive,
    /// The tool's server has untrusted provenance (a shared or public-registry import).
    UntrustedSource,
    /// Both of the above.
    DestructiveAndUntrusted,
    /// The call carries a pseudonym minted by a DIFFERENT server, so dispatching it would
    /// hand one server's data to another (SBS-696). Never produced by [`gate_reason`]: this
    /// gate is not about the tool at all, it is about a specific value's destination, and it
    /// fires even when HITL gating is off -- the alternative is not "the call runs", it is
    /// "the call fails", so the prompt can only turn a certain failure into a possible
    /// success.
    PiiCrossServer,
}

/// A request to release specific pseudonymized values to a server that did not produce them.
///
/// Carries REAL values. That is the point -- a person cannot judge "may this address go to
/// the mail server?" without seeing the address - and it is safe only because the broker is
/// loopback-only, in-memory, and already the one audience that sees real arguments before
/// dispatch. It must never be logged, persisted, or relayed to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiiReleaseRequest {
    /// The server that would receive the values, and that no value below came from.
    pub server: String,
    /// Every refused value in this call. Approval covers exactly this set, for exactly
    /// this destination.
    pub values: Vec<PiiReleaseValue>,
}

/// One value a [`PiiReleaseRequest`] would release.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiiReleaseValue {
    /// The pseudonym as the model sees it, e.g. `⟦EMAIL_1⟧`.
    pub token: String,
    /// The real value behind it. Local approver only.
    pub value: String,
    /// The servers that already hold it, so the approver can see where it came from --
    /// releasing to a server that already has the value is a different decision from
    /// releasing to one that has never seen it.
    pub origins: Vec<String>,
}

const CLIENT_LABEL_MAX_BYTES: usize = 200;
const CLIENT_LABEL_MAX_CHARS: usize = 120;

pub(crate) fn client_label_within_bounds(text: &str) -> bool {
    text.len() <= CLIENT_LABEL_MAX_BYTES
        && text.chars().take(CLIENT_LABEL_MAX_CHARS + 1).count() <= CLIENT_LABEL_MAX_CHARS
}

/// Client-reported text is display-only. Bound both its stored bytes and visible
/// characters, and remove formatting that can disguise the trusted requester.
pub fn sanitize_client_label(text: &str) -> Option<String> {
    static FORMAT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let format =
        FORMAT.get_or_init(|| regex::Regex::new(r"\p{Cf}").expect("Unicode format category"));
    let mut label = String::new();
    let mut chars = 0;
    let mut marks = 0;
    for mut c in text.chars() {
        if c.is_whitespace() {
            c = ' ';
        } else if c.is_control() || format.is_match(c.encode_utf8(&mut [0; 4])) {
            continue;
        }
        if c == ' ' && (label.is_empty() || label.ends_with(' ')) {
            continue;
        }
        if unicode_normalization::char::is_combining_mark(c) {
            marks += 1;
            if marks > 2 {
                continue;
            }
        } else {
            marks = 0;
        }
        if chars == CLIENT_LABEL_MAX_CHARS || label.len() + c.len_utf8() > CLIENT_LABEL_MAX_BYTES {
            break;
        }
        label.push(c);
        chars += 1;
    }
    let label = label.trim_end();
    (!label.is_empty()).then(|| label.to_string())
}

/// Truncate display text without splitting a Unicode character.
pub fn shorten_client_label(label: &str, limit: usize) -> String {
    if label.chars().count() <= limit {
        label.to_string()
    } else {
        format!(
            "{}…",
            label
                .chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub fn client_info_label(params: Option<&serde_json::Value>) -> Option<String> {
    let info = params?.get("clientInfo")?;
    let name = info.get("name")?.as_str()?;
    let version = info.get("version").and_then(serde_json::Value::as_str);
    // Cropping first can hide a path separator while retaining a private prefix.
    if !client_label_within_bounds(name)
        || version.is_some_and(|version| !client_label_within_bounds(version))
    {
        return Some("[private]".into());
    }
    let name = sanitize_client_label(name)?;
    let version = version.and_then(sanitize_client_label);
    let label = match version {
        Some(version) => format!("{name} {version}"),
        None => name,
    };
    if !client_label_within_bounds(&label) {
        return Some("[private]".into());
    }
    sanitize_client_label(&label)
}

fn deserialize_client_label<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let label = Option::<String>::deserialize(deserializer)?;
    Ok(label.as_deref().and_then(sanitize_client_label))
}

/// A request from a gateway to the broker: "a human should approve this call." The arguments
/// are included so the person can review them; they stay in memory on both ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    /// Auth token from the endpoint descriptor.
    pub token: String,
    /// Opaque per-call id; also the correlation key for the decision.
    pub id: String,
    /// Which client/agent triggered it (for display + attribution), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Gateway-observed display name, never an authorization principal.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_client_label"
    )]
    pub client_name: Option<String>,
    /// Untrusted initialize clientInfo label. Never an access principal.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_client_label"
    )]
    pub client_label: Option<String>,
    /// The downstream server the tool belongs to.
    pub server: String,
    /// The tool name.
    pub tool: String,
    /// Why it was gated, for the UI.
    pub reason: ApprovalReason,
    /// The exact arguments the human is approving.
    pub arguments: serde_json::Value,
    /// Fingerprint of the current tool definition, when the gateway can resolve it.
    /// Allowlist entries include this so a tool definition change re-requires approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_fingerprint: Option<String>,
    /// A URL-mode elicitation that the desktop broker should present when the MCP client
    /// cannot do so itself. Absent for ordinary tool approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_elicitation: Option<UrlElicitationRequest>,
    /// Pseudonymized values this call would send to a server that never produced them.
    /// Present only for [`ApprovalReason::PiiCrossServer`]; absent for ordinary approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pii_release: Option<PiiReleaseRequest>,
}

/// The already-screened browser interaction carried over the local broker. The gateway
/// validates the URL and derives `origin`; the desktop renders that origin separately so a
/// server-controlled message cannot disguise where the link goes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlElicitationRequest {
    pub url: String,
    pub origin: String,
    pub message: String,
}

/// The broker's answer to an [`ApprovalRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// A human approved; the call runs.
    Approved,
    /// A human denied; the call is refused.
    Denied,
    /// A human was asked but did not decide within the fail-closed window; treated as a deny.
    Timeout,
    /// The gateway could not reach a live approval broker: no endpoint descriptor, a dead
    /// endpoint, or the transport failed before the request was ever handed off. Distinct
    /// from [`Timeout`] (a human *was* asked and didn't answer) so the agent-facing message
    /// and any audit can tell "the approval service is down" apart from "you didn't approve
    /// in time". Still fail-closed - it never lets the call proceed. The broker never sends
    /// this; it is only ever produced gateway-side.
    Unreachable,
    /// A human approved a *specific* call, but the arguments about to execute no longer match
    /// the ones that were approved (their canonical [`crate::audit::args_hash`] differs). The
    /// stale approval is rejected rather than run on mutated content - approval binds to
    /// CONTENT, not just intent. Like [`Unreachable`], the broker never sends this; it is
    /// only ever produced gateway-side, at execute time, and is still fail-closed. This is
    /// the seam a decoupled approval (session re-use, or a code-mode script that approves
    /// then replays) must clear before its effect runs.
    StaleState,
    /// The caller withdrew before a human decision was received. Gateway-only.
    Withdrawn,
}

impl ApprovalDecision {
    /// The security-critical predicate: ONLY an explicit human approval lets the call
    /// proceed. Denied, Timeout, Unreachable, and (at the call site) any transport error
    /// all block.
    pub fn is_approved(self) -> bool {
        matches!(self, ApprovalDecision::Approved)
    }
}

/// HITL gating policy: given whether a tool is destructive and whether its server has
/// untrusted provenance, decide if the call needs a human. `enabled` is the registry's
/// `human_approval` master switch. Returns the reason when gated, `None` when the call may
/// run without approval.
///
/// v1 gates destructive tools AND anything from an untrusted-provenance server (the same
/// shared/registry signal the SSRF connect-guard uses), so it does not rely solely on
/// servers that bother to set `destructiveHint`.
pub fn gate_reason(
    enabled: bool,
    is_destructive: bool,
    untrusted_source: bool,
) -> Option<ApprovalReason> {
    if !enabled {
        return None;
    }
    match (is_destructive, untrusted_source) {
        (true, true) => Some(ApprovalReason::DestructiveAndUntrusted),
        (true, false) => Some(ApprovalReason::Destructive),
        (false, true) => Some(ApprovalReason::UntrustedSource),
        (false, false) => None,
    }
}

/// The stable key for the "allow this tool past approval" lists (per-session in the broker,
/// persistent in the registry). One definition so both sides agree. `server` is already the
/// sanitized prefix, so `server/tool` is unambiguous.
pub fn allow_key(server: &str, tool: &str) -> String {
    format!("{server}/{tool}")
}

/// Fingerprint-bound allow key. This is intentionally distinct from the legacy
/// `server/tool` key so old broad allows don't silently keep bypassing approval.
pub fn fingerprint_allow_key(server: &str, tool: &str, fingerprint: &str) -> String {
    format!("{}/{}/{}", server, tool, fingerprint)
}

// ---------------------------------------------------------------------------------------
// Broker transport and mutual authentication
// ---------------------------------------------------------------------------------------
//
// The endpoint descriptor is owner-only on disk, so holding the token proves a process can
// read the Toolport data dir. The token has always authenticated the GATEWAY to the broker.
// Nothing authenticated the broker to the gateway: a gateway dialed whatever the descriptor
// named and believed whatever came back, so any process that could bind the published port
// after the app had gone - a stale descriptor survives a crash or a force-kill - could
// answer `"approved"` to every gated call and, worse, RECEIVE the request first, including
// the real values behind a PII release (SBS-867).
//
// The fix is a challenge before anything else crosses the wire. The gateway sends a fresh
// random nonce; the broker answers with HMAC-SHA256(token, nonce); the gateway verifies that
// against the token in the descriptor it read, and only then sends the request. A peer that
// cannot read the descriptor cannot produce the proof, so it never sees the request and its
// decision is never read. On Unix the broker additionally listens on a socket file in a 0700
// directory, which a current gateway prefers, so such a peer cannot even connect on that
// path; the loopback listener stays for gateways that predate the field, and the challenge
// runs on both, so the guarantee does not depend on which transport was chosen.
//
// Out of scope, on purpose: a process running as the SAME user as Toolport. It can read the
// descriptor and registry.json alike, so it can answer the challenge - and it can switch
// human approval off directly. Nothing in a same-user trust model stops that; per-server
// sandboxing (SBS-185) is the answer there, not a stronger handshake.

/// Marks a Unix-domain-socket endpoint in [`EndpointDescriptor::endpoint`]:
/// `unix:/absolute/path/approval.sock`. Anything else is a `host:port` loopback address.
pub const UNIX_ENDPOINT_PREFIX: &str = "unix:";

/// How long each side waits on the other during the handshake. Short on purpose: a peer
/// that cannot answer the challenge promptly is not the app, and the gateway's caller is
/// holding a tool call open.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on the proof line a gateway reads before deciding the peer is not a broker.
/// A real proof is well under 200 bytes.
const MAX_PROOF_LINE_BYTES: u64 = 1024;

/// The first line a gateway writes after connecting. The key name doubles as the protocol
/// marker: a broker that sees it answers with a [`BrokerProof`] before reading the request;
/// a request or suggestion line never carries it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerChallenge {
    /// Hex-encoded random nonce, fresh per dial.
    pub toolport_approval_challenge: String,
}

/// The broker's answer to a [`BrokerChallenge`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerProof {
    /// Hex HMAC-SHA256 keyed by the broker token over the nonce string exactly as sent.
    pub toolport_approval_proof: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The proof a broker holding `token` gives for `nonce` (the hex string as sent, not the
/// decoded bytes, so neither side has to agree on a decoding).
pub fn challenge_proof(token: &str, nonce: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(token.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(nonce.as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// Constant-time comparison of a presented proof against the expected one.
pub fn proof_matches(expected: &str, presented: &str) -> bool {
    expected.len() == presented.len() && bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
}

/// Broker side: if `first_line` is a [`BrokerChallenge`], the [`BrokerProof`] line to write
/// back (without its trailing newline). `None` means the line is not a challenge - a gateway
/// from before the handshake existed, sending its request straight away - and the caller
/// should treat it as the request itself.
pub fn answer_challenge(first_line: &[u8], token: &str) -> Option<String> {
    let challenge: BrokerChallenge = serde_json::from_slice(first_line).ok()?;
    serde_json::to_string(&BrokerProof {
        toolport_approval_proof: challenge_proof(token, &challenge.toolport_approval_challenge),
    })
    .ok()
}

/// A connection between a gateway and the broker over whichever transport the descriptor
/// names: Read + Write plus the timeout and clone controls both sides use.
#[derive(Debug)]
pub enum BrokerStream {
    Tcp(std::net::TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl BrokerStream {
    /// Connect to `endpoint` as written in a descriptor. No handshake; see [`dial_broker`].
    pub fn connect(endpoint: &str) -> io::Result<Self> {
        match endpoint.strip_prefix(UNIX_ENDPOINT_PREFIX) {
            #[cfg(unix)]
            Some(path) => std::os::unix::net::UnixStream::connect(path).map(Self::Unix),
            #[cfg(not(unix))]
            Some(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unix socket endpoints are not available on this platform",
            )),
            None => std::net::TcpStream::connect(endpoint).map(Self::Tcp),
        }
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_read_timeout(dur),
            #[cfg(unix)]
            Self::Unix(s) => s.set_read_timeout(dur),
        }
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_write_timeout(dur),
            #[cfg(unix)]
            Self::Unix(s) => s.set_write_timeout(dur),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(s) => s.try_clone().map(Self::Tcp),
            #[cfg(unix)]
            Self::Unix(s) => s.try_clone().map(Self::Unix),
        }
    }
}

impl Read for BrokerStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
        }
    }
}

impl Write for BrokerStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
        }
    }
}

/// Gateway side: connect to the broker `desc` names and make it prove it holds `desc.token`
/// before anything else is sent. Every failure is an `Err`, which the caller treats as "no
/// broker was reached": the request was never written, so no human was asked and nothing
/// confidential left this process. On `Ok` the stream sits right after the proof line with
/// the handshake timeouts still set; the caller sets its own before the long wait.
pub fn dial_broker(desc: &EndpointDescriptor) -> io::Result<BrokerStream> {
    // An empty token means the app's CSPRNG failed at startup. The broker denies every
    // request in that state anyway; refusing here as well keeps an empty-key proof, which
    // anyone can compute, from ever counting as authentication.
    if desc.token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "broker descriptor carries no token",
        ));
    }
    let mut nonce = [0u8; 32];
    getrandom::getrandom(&mut nonce).map_err(|_| io::Error::other("CSPRNG unavailable"))?;
    let nonce = hex(&nonce);

    // Prefer the socket file when the broker published one; a connect failure there (stale
    // path, or a platform without it) falls back to the loopback address. The challenge below
    // protects both the same way, so the fallback changes nothing about the guarantee.
    let mut stream = match desc.unix_endpoint.as_deref() {
        Some(unix) => {
            BrokerStream::connect(unix).or_else(|_| BrokerStream::connect(&desc.endpoint))?
        }
        None => BrokerStream::connect(&desc.endpoint)?,
    };
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let challenge = serde_json::to_string(&BrokerChallenge {
        toolport_approval_challenge: nonce.clone(),
    })?;
    stream.write_all(challenge.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    // The broker writes nothing after the proof until it has our request, so a buffered
    // reader over a clone cannot swallow anything the caller will later want.
    let mut reply = String::new();
    let read = io::BufReader::new(stream.try_clone()?)
        .take(MAX_PROOF_LINE_BYTES)
        .read_line(&mut reply)?;
    if read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed before answering the challenge",
        ));
    }
    if !reply.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proof line exceeds the bound",
        ));
    }
    let proof: BrokerProof = serde_json::from_str(reply.trim()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "peer did not answer the challenge",
        )
    })?;
    if !proof_matches(
        &challenge_proof(&desc.token, &nonce),
        &proof.toolport_approval_proof,
    ) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "peer failed the token proof",
        ));
    }
    Ok(stream)
}

// ---------------------------------------------------------------------------
// Broker client: shared by the gateway's HITL gate and the agent guard (SBS-1059)
// ---------------------------------------------------------------------------

/// A fresh 128-bit correlation id for an approval request (same CSPRNG-or-die policy
/// as the confirm token: a randomness failure on a security gate is fatal, not papered).
pub fn new_correlation_id() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).expect("CSPRNG unavailable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read the approval-broker endpoint the Toolport app publishes into the data dir.
/// `None` when it is absent/unreadable (the app is not running) - a fail-closed signal.
pub fn read_endpoint_descriptor() -> Option<EndpointDescriptor> {
    let dir = crate::registry::conduit_dir()?;
    let raw = std::fs::read_to_string(dir.join(ENDPOINT_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The outcome of a single dial to the approval broker. Separating "we never reached a
/// live broker" from "a broker answered" lets the caller retry a *stale* endpoint (the app
/// just restarted and rebound to a new port) without ever re-prompting a human who was
/// already asked.
pub enum BrokerAttempt {
    /// A broker received the request and answered (Approved / Denied / Timeout).
    Decided(ApprovalDecision),
    /// We never handed the request to a live broker: no descriptor, connect refused, or the
    /// transport failed before the request went across. No human was asked, so a retry
    /// against a freshly-read descriptor is safe.
    Unreachable,
}

/// One dial to the broker described by `desc`. FAIL-CLOSED throughout: the arguments travel
/// over the socket and never touch disk. The dial itself ([`dial_broker`]) makes
/// the peer prove it holds the descriptor's token before a byte of the request is written,
/// so a process that merely binds the published endpoint after the app has gone gets
/// neither the arguments nor a say in the decision (SBS-867).
///
/// The key invariant: `Unreachable` is returned ONLY when the request never reached a
/// broker (so no human saw it). Once the request is written, any later failure - including
/// the read timeout that means "the human didn't answer" - is a `Decided(Timeout)`, so we
/// never retry in a way that could double-prompt.
pub fn try_decide_once(
    desc: Option<EndpointDescriptor>,
    req: &mut ApprovalRequest,
) -> BrokerAttempt {
    try_decide_once_with_cancel(desc, req, None)
}

/// Preserve broker outcome and retry semantics while withdrawing a cancelled call.
pub fn try_decide_once_with_cancel(
    desc: Option<EndpointDescriptor>,
    req: &mut ApprovalRequest,
    cancel: Option<&crate::downstream::CancelContext>,
) -> BrokerAttempt {
    if cancel.is_some_and(crate::downstream::CancelContext::is_cancelled) {
        return BrokerAttempt::Decided(ApprovalDecision::Withdrawn);
    }
    let Some(desc) = desc else {
        return BrokerAttempt::Unreachable;
    };
    req.token = desc.token.clone();
    // Connect refused, no answer to the challenge, or a wrong proof: in every case the
    // request was never written, so no human was asked and a re-dial is safe.
    let Ok(mut stream) = dial_broker(&desc) else {
        return BrokerAttempt::Unreachable;
    };
    let withdrawal = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return BrokerAttempt::Unreachable,
    };
    let _withdraw = cancel.map(|cancel| {
        cancel.on_cancel(std::sync::Arc::new(move |_| match &withdrawal {
            BrokerStream::Tcp(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            #[cfg(unix)]
            BrokerStream::Unix(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }))
    });
    if cancel.is_some_and(crate::downstream::CancelContext::is_cancelled) {
        return BrokerAttempt::Decided(ApprovalDecision::Withdrawn);
    }
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    // Windows shutdown can expose EOF to the broker without waking our blocked
    // reader. Poll cancellation within the unchanged overall approval deadline.
    let read_timeout = if cancel.is_some() {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(DEFAULT_TIMEOUT_SECS)
    };
    let _ = stream.set_read_timeout(Some(read_timeout));
    let decision = if cancel.is_some() {
        exchange_approval_request_with_cancel(req, stream, cancel)
    } else {
        exchange_approval_request(req, stream)
    };
    broker_attempt_after_cancel(
        decision,
        cancel.is_some_and(crate::downstream::CancelContext::is_cancelled),
    )
}

fn broker_attempt_after_cancel(decision: BrokerAttempt, cancelled: bool) -> BrokerAttempt {
    // A parsed human answer wins even if the client disconnected immediately
    // after it arrived. Only a transport failure without an answer is withdrawal.
    if matches!(
        decision,
        BrokerAttempt::Decided(ApprovalDecision::Approved | ApprovalDecision::Denied)
    ) || !cancelled
    {
        decision
    } else {
        BrokerAttempt::Decided(ApprovalDecision::Withdrawn)
    }
}

fn exchange_approval_request(
    req: &ApprovalRequest,
    stream: impl io::Read + io::Write,
) -> BrokerAttempt {
    exchange_approval_request_with_cancel(req, stream, None)
}

fn exchange_approval_request_with_cancel(
    req: &ApprovalRequest,
    mut stream: impl io::Read + io::Write,
    cancel: Option<&crate::downstream::CancelContext>,
) -> BrokerAttempt {
    use std::io::{BufRead, BufReader, Read};
    let Ok(line) = serde_json::to_string(req) else {
        // We connected but can't serialize our own request: not a reachability problem, so
        // don't spin on retry. Fail closed.
        return BrokerAttempt::Decided(ApprovalDecision::Timeout);
    };
    if stream.write_all(line.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
        // The request never made it across, so no human was asked: safe to re-dial.
        return BrokerAttempt::Unreachable;
    }
    let _ = stream.flush();
    let mut resp = String::new();
    let mut reader = BufReader::new(stream).take(4096);
    let deadline = std::time::Instant::now() + Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    loop {
        if cancel.is_some_and(crate::downstream::CancelContext::is_cancelled) {
            return BrokerAttempt::Decided(ApprovalDecision::Withdrawn);
        }
        if std::time::Instant::now() >= deadline {
            return BrokerAttempt::Decided(ApprovalDecision::Timeout);
        }
        match reader.read_line(&mut resp) {
            // Connected and the peer closed with no answer: not a healthy broker. No human was
            // shown a prompt (the broker's pre-prompt reject paths close silently), so re-dial.
            Ok(0) => return BrokerAttempt::Unreachable,
            Ok(_) => {
                let t = resp.trim();
                if t.is_empty() {
                    return BrokerAttempt::Unreachable;
                } else {
                    // A parseable decision is authoritative; an unparseable line is fail-closed
                    // as a Timeout (a real broker answered, so this is not a retry case).
                    return BrokerAttempt::Decided(
                        serde_json::from_str::<ApprovalDecision>(t)
                            .unwrap_or(ApprovalDecision::Timeout),
                    );
                }
            }
            // A read error AFTER we sent the request is the "human didn't answer in time" path
            // (read timeout) or a mid-wait drop. Either way the broker had our request, so this
            // is a genuine no-decision Timeout - never retry (that would re-prompt).
            Err(error)
                if cancel.is_some()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
            Err(_) => return BrokerAttempt::Decided(ApprovalDecision::Timeout),
        }
    }
}

/// Ask the app broker for a human decision on `req`, reading the endpoint descriptor once.
/// Collapses an unreachable broker to the `Unreachable` decision (still fail-closed). Kept
/// as a thin, dependency-free entry point for unit tests; `request_human_decision` is the
/// production path with the self-healing retry.
pub fn decide_via_broker(
    desc: Option<EndpointDescriptor>,
    req: &mut ApprovalRequest,
) -> ApprovalDecision {
    match try_decide_once(desc, req) {
        BrokerAttempt::Decided(d) => d,
        BrokerAttempt::Unreachable => ApprovalDecision::Unreachable,
    }
}

/// Hold a gated tool call until a human decides via the Toolport app (or it fails closed).
///
/// If the first dial can't reach a live broker, re-read the descriptor and retry once: the
/// app may have just restarted and rebound to a new port, leaving the descriptor we first
/// read stale. This self-heals that race without ever failing open - two unreachable dials
/// return `Unreachable`, which is still a deny.
pub fn request_human_decision(mut req: ApprovalRequest) -> ApprovalDecision {
    match try_decide_once(read_endpoint_descriptor(), &mut req) {
        BrokerAttempt::Decided(d) => d,
        BrokerAttempt::Unreachable => match try_decide_once(read_endpoint_descriptor(), &mut req) {
            BrokerAttempt::Decided(d) => d,
            BrokerAttempt::Unreachable => {
                crate::gatewaylog::append(
                    "approval broker unreachable after retry; failing closed (Unreachable)",
                );
                ApprovalDecision::Unreachable
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p08_cancel_aware_broker_keeps_write_failure_unreachable_and_read_timeout_no_response() {
        struct FailingStream {
            write_fails: bool,
        }
        impl io::Write for FailingStream {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.write_fails {
                    Err(io::ErrorKind::BrokenPipe.into())
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl io::Read for FailingStream {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
        }
        let req = ApprovalRequest {
            token: "tok".into(),
            id: "p08-outcomes".into(),
            client: None,
            client_name: crate::session_observability::current().client_name,
            client_label: None,
            server: "s".into(),
            tool: "t".into(),
            reason: ApprovalReason::Destructive,
            arguments: serde_json::json!({}),
            tool_fingerprint: None,
            url_elicitation: None,
            pii_release: None,
        };
        assert!(matches!(
            exchange_approval_request(&req, FailingStream { write_fails: true }),
            BrokerAttempt::Unreachable
        ));
        assert!(matches!(
            exchange_approval_request(&req, FailingStream { write_fails: false }),
            BrokerAttempt::Decided(ApprovalDecision::Timeout)
        ));
    }

    #[test]
    fn gate_is_off_when_disabled() {
        assert_eq!(gate_reason(false, true, true), None);
        assert_eq!(gate_reason(false, true, false), None);
    }

    #[test]
    fn gate_covers_destructive_and_untrusted() {
        assert_eq!(
            gate_reason(true, true, false),
            Some(ApprovalReason::Destructive)
        );
        assert_eq!(
            gate_reason(true, false, true),
            Some(ApprovalReason::UntrustedSource)
        );
        assert_eq!(
            gate_reason(true, true, true),
            Some(ApprovalReason::DestructiveAndUntrusted)
        );
        // A read-only tool from a trusted server is never gated, even with HITL on.
        assert_eq!(gate_reason(true, false, false), None);
    }

    #[test]
    fn only_explicit_approval_proceeds() {
        assert!(ApprovalDecision::Approved.is_approved());
        assert!(!ApprovalDecision::Denied.is_approved());
        assert!(!ApprovalDecision::Timeout.is_approved());
        // Unreachable is fail-closed exactly like the other non-approvals.
        assert!(!ApprovalDecision::Unreachable.is_approved());
        // StaleState (approved-then-mutated) is fail-closed too: content no longer matches.
        assert!(!ApprovalDecision::StaleState.is_approved());
    }

    #[test]
    fn stale_state_is_a_distinct_serde_variant() {
        // Round-trips to snake_case and is distinct from the other non-approvals so a
        // caller can tell "the approved content changed" apart from a deny/timeout.
        let s = serde_json::to_string(&ApprovalDecision::StaleState).unwrap();
        assert_eq!(s, "\"stale_state\"");
        let back: ApprovalDecision = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ApprovalDecision::StaleState);
        assert_ne!(ApprovalDecision::StaleState, ApprovalDecision::Denied);
        assert_ne!(ApprovalDecision::StaleState, ApprovalDecision::Unreachable);
    }

    #[test]
    fn unreachable_is_a_distinct_serde_variant() {
        // The variant must round-trip (it flows through the same decision type), and be
        // distinct from Timeout so callers can tell the two failure modes apart.
        let s = serde_json::to_string(&ApprovalDecision::Unreachable).unwrap();
        assert_eq!(s, "\"unreachable\"");
        let back: ApprovalDecision = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ApprovalDecision::Unreachable);
        assert_ne!(ApprovalDecision::Unreachable, ApprovalDecision::Timeout);
    }

    #[test]
    fn wire_types_round_trip() {
        let req = ApprovalRequest {
            token: "tok".into(),
            id: "abc".into(),
            client: Some("cursor".into()),
            client_name: crate::session_observability::current().client_name,
            client_label: None,
            server: "db".into(),
            tool: "drop_table".into(),
            reason: ApprovalReason::Destructive,
            arguments: serde_json::json!({ "table": "users" }),
            tool_fingerprint: Some("v2:abc".into()),
            url_elicitation: None,
            pii_release: None,
        };
        let round: ApprovalRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(round.tool, "drop_table");
        assert_eq!(round.reason, ApprovalReason::Destructive);
        assert_eq!(round.arguments["table"], "users");
        assert_eq!(round.tool_fingerprint.as_deref(), Some("v2:abc"));

        let dec: ApprovalDecision =
            serde_json::from_str(&serde_json::to_string(&ApprovalDecision::Approved).unwrap())
                .unwrap();
        assert!(dec.is_approved());
    }

    #[test]
    fn fingerprint_allow_key_binds_definition() {
        assert_eq!(
            fingerprint_allow_key("db", "drop_table", "v2:abc"),
            "db/drop_table/v2:abc"
        );
        assert_ne!(
            fingerprint_allow_key("db", "drop_table", "v2:abc"),
            allow_key("db", "drop_table")
        );
    }

    #[test]
    fn endpoint_descriptor_round_trips() {
        let d = EndpointDescriptor {
            endpoint: "127.0.0.1:8790".into(),
            token: "s3cret".into(),
            unix_endpoint: None,
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            !json.contains("unixEndpoint"),
            "an absent socket file is not written, so the shape an older gateway parses is byte-identical to before: {json}"
        );
        let round: EndpointDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(round.endpoint, "127.0.0.1:8790");
        assert_eq!(round.token, "s3cret");
        assert_eq!(round.unix_endpoint, None);

        // A descriptor an older app wrote (no socket field) still parses, and one with the
        // field round-trips.
        let legacy: EndpointDescriptor =
            serde_json::from_str(r#"{"endpoint":"127.0.0.1:1","token":"t"}"#).unwrap();
        assert_eq!(legacy.unix_endpoint, None);
        let with_unix = EndpointDescriptor {
            endpoint: "127.0.0.1:8790".into(),
            token: "s3cret".into(),
            unix_endpoint: Some("unix:/tmp/x/approval.sock".into()),
        };
        let round: EndpointDescriptor =
            serde_json::from_str(&serde_json::to_string(&with_unix).unwrap()).unwrap();
        assert_eq!(
            round.unix_endpoint.as_deref(),
            Some("unix:/tmp/x/approval.sock")
        );
    }

    #[test]
    fn challenge_proof_is_hmac_sha256_over_the_nonce() {
        // RFC 4231 test case 2, so the proof is exactly what a reviewer expects and not a
        // home-grown construction.
        assert_eq!(
            challenge_proof("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_ne!(
            challenge_proof("tok", "nonce-a"),
            challenge_proof("tok", "nonce-b")
        );
        assert_ne!(
            challenge_proof("tok-a", "nonce"),
            challenge_proof("tok-b", "nonce")
        );
        assert!(proof_matches("abc", "abc"));
        assert!(!proof_matches("abc", "abd"));
        assert!(!proof_matches("abc", "abcd"));
        assert!(!proof_matches("", "a"));
    }

    #[test]
    fn broker_answers_a_challenge_and_passes_anything_else_through() {
        let line = serde_json::to_vec(&BrokerChallenge {
            toolport_approval_challenge: "00ff".into(),
        })
        .unwrap();
        let answer = answer_challenge(&line, "tok").expect("a challenge is answered");
        let proof: BrokerProof = serde_json::from_str(&answer).unwrap();
        assert_eq!(
            proof.toolport_approval_proof,
            challenge_proof("tok", "00ff")
        );

        // A pre-handshake gateway's request is not a challenge: the caller treats it as
        // the request itself, authenticated by its own token field exactly as before.
        let req = serde_json::to_vec(&ApprovalRequest {
            token: "tok".into(),
            id: "abc".into(),
            client: None,
            client_name: crate::session_observability::current().client_name,
            client_label: None,
            server: "db".into(),
            tool: "drop_table".into(),
            reason: ApprovalReason::Destructive,
            arguments: serde_json::json!({}),
            tool_fingerprint: None,
            url_elicitation: None,
            pii_release: None,
        })
        .unwrap();
        assert!(answer_challenge(&req, "tok").is_none());
        assert!(answer_challenge(b"not json", "tok").is_none());
    }

    /// A loopback peer that answers every line it receives with `reply(line)` and reports
    /// each line it saw. Stands in for the real broker, and for an impostor.
    fn scripted_peer(
        reply: impl Fn(&str) -> String + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                let seen = line.trim().to_string();
                line.clear();
                let answer = reply(&seen);
                let _ = tx.send(seen);
                if stream
                    .write_all(answer.as_bytes())
                    .and_then(|_| stream.write_all(b"\n"))
                    .is_err()
                {
                    break;
                }
            }
        });
        (endpoint, rx)
    }

    fn desc(endpoint: String, token: &str) -> EndpointDescriptor {
        EndpointDescriptor {
            endpoint,
            token: token.into(),
            unix_endpoint: None,
        }
    }

    #[test]
    fn dial_succeeds_against_a_peer_that_proves_the_token() {
        let (endpoint, seen) = scripted_peer(|line| {
            answer_challenge(line.as_bytes(), "tok").unwrap_or_else(|| "\"approved\"".into())
        });
        let mut stream = dial_broker(&desc(endpoint, "tok")).expect("an honest broker passes");
        let first = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.contains("toolportApprovalChallenge"), "{first}");
        assert!(
            !first.contains("tok"),
            "the token never travels in the clear: {first}"
        );

        // The stream is usable for the request/decision exchange afterwards.
        stream.write_all(b"{\"request\":1}\n").unwrap();
        let second = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(second, "{\"request\":1}");
        let mut decision = String::new();
        io::BufReader::new(stream).read_line(&mut decision).unwrap();
        assert_eq!(decision.trim(), "\"approved\"");
    }

    /// SBS-867: a peer that merely holds the published port answers the first line with
    /// `"approved"` - exactly what a pre-handshake gateway would have believed. It must be
    /// refused, and it must never be sent the request.
    #[test]
    fn dial_refuses_a_peer_that_answers_without_the_proof() {
        let (endpoint, seen) = scripted_peer(|_| "\"approved\"".into());
        let err = dial_broker(&desc(endpoint, "tok")).expect_err("an impostor is refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        let first = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.contains("toolportApprovalChallenge"), "{first}");
        assert!(
            seen.recv_timeout(Duration::from_millis(500)).is_err(),
            "nothing follows a failed challenge"
        );
    }

    #[test]
    fn dial_refuses_a_proof_for_the_wrong_token() {
        let (endpoint, _seen) = scripted_peer(|line| {
            answer_challenge(line.as_bytes(), "not-the-token").unwrap_or_default()
        });
        let err = dial_broker(&desc(endpoint, "tok")).expect_err("a wrong proof is refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    }

    #[test]
    fn dial_refuses_an_empty_token_before_connecting() {
        // Port 1 would be ConnectionRefused if a connect were attempted; InvalidInput shows
        // the refusal happened first.
        let err = dial_broker(&desc("127.0.0.1:1".into(), "")).expect_err("no token, no dial");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    }

    #[test]
    fn dial_reports_a_peer_that_hangs_up_as_unreachable_not_decided() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let _ = listener.accept(); // accept, then drop immediately
        });
        let err = dial_broker(&desc(endpoint, "tok")).expect_err("a hang-up is an error");
        // Whether the OS reports the early close as a clean EOF or as a reset depends on
        // whether our challenge write had landed when the peer dropped; either way the
        // caller sees an error, and the caller maps every error to Unreachable.
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            ),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dial_works_over_a_unix_socket_endpoint() {
        let dir = std::env::temp_dir().join(format!(
            "toolport-broker-uds-{}-{}",
            std::process::id(),
            challenge_proof("salt", "dial_works_over_a_unix_socket_endpoint")
                .get(..8)
                .unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("approval.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let proof = answer_challenge(line.as_bytes(), "tok").unwrap();
            stream.write_all(proof.as_bytes()).unwrap();
            stream.write_all(b"\n").unwrap();
        });
        // The socket file is preferred over the loopback address when both are published;
        // port 1 would refuse, so reaching Ok proves the unix path was taken.
        let mut d = desc("127.0.0.1:1".into(), "tok");
        d.unix_endpoint = Some(format!("{UNIX_ENDPOINT_PREFIX}{}", path.display()));
        let stream = dial_broker(&d).expect("uds broker passes");
        assert!(matches!(stream, BrokerStream::Unix(_)));
        drop(stream);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn dial_falls_back_to_loopback_when_the_socket_file_is_gone() {
        let (endpoint, _seen) =
            scripted_peer(|line| answer_challenge(line.as_bytes(), "tok").unwrap_or_default());
        let mut d = desc(endpoint, "tok");
        d.unix_endpoint = Some(format!(
            "{UNIX_ENDPOINT_PREFIX}/nonexistent/toolport-{}/approval.sock",
            std::process::id()
        ));
        let stream = dial_broker(&d).expect("falls back to the loopback listener");
        assert!(matches!(stream, BrokerStream::Tcp(_)));
    }
}

#[cfg(test)]
mod client_label_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn p08b_labels_are_bounded_and_strip_controls_and_bidi() {
        assert_eq!(
            client_info_label(Some(
                &json!({"clientInfo":{"name":"  Claude\n\r\t\u{202e} Code\u{2066}","version":"1\0"}})
            )),
            Some("Claude Code 1".into())
        );
        let long = "界".repeat(1000);
        let label =
            client_info_label(Some(&json!({"clientInfo":{"name":long,"version":"evil"}}))).unwrap();
        assert!(label.chars().count() <= 120);
        assert!(label.len() <= 200);
        assert_eq!(
            client_info_label(Some(
                &json!({"clientInfo":{"name":"\u{202e}\0","version":"1"}})
            )),
            None
        );
        assert_eq!(
            client_info_label(Some(&json!({"clientInfo":{"name":false}}))),
            None
        );
    }

    #[test]
    fn p08b_request_label_is_sanitized_and_json_escaped() {
        let req: ApprovalRequest = serde_json::from_value(json!({"token":"t","id":"1","client":"client:real","clientLabel":"<b>Fake</b>\n\u{202e} \"1\"","server":"s","tool":"t","reason":"destructive","arguments":{}})).unwrap();
        assert_eq!(req.client.as_deref(), Some("client:real"));
        assert_eq!(req.client_label.as_deref(), Some("<b>Fake</b> \"1\""));
        let wire = serde_json::to_string(&req).unwrap();
        assert!(wire.contains("\\\"1\\\""));
        assert!(!wire.contains('\u{202e}'));
    }
}

#[cfg(test)]
mod p08b_revision_tests {
    use super::*;

    #[test]
    fn p08b_r1_all_whitespace_collapses_and_format_characters_disappear() {
        for c in [
            '\t', '\n', '\r', '\u{85}', '\u{a0}', '\u{2028}', '\u{2029}', '\u{3000}',
        ] {
            assert_eq!(
                sanitize_client_label(&format!("Claude{c}{c}Code")),
                Some("Claude Code".into())
            );
        }
        for c in [
            '\u{ad}',
            '\u{180e}',
            '\u{fff9}',
            '\u{fffa}',
            '\u{fffb}',
            '\u{e0001}',
            '\u{e0020}',
            '\u{e007f}',
            '\u{202e}',
            '\u{200b}',
        ] {
            assert_eq!(
                sanitize_client_label(&format!("Claude{c} Code")),
                Some("Claude Code".into())
            );
        }
    }

    #[test]
    fn p08b_r1_combining_marks_and_utf8_bytes_are_bounded() {
        assert_eq!(
            sanitize_client_label("a\u{301}\u{302}\u{303}b\u{301}\u{302}\u{303}"),
            Some("a\u{301}\u{302}b\u{301}\u{302}".into())
        );
        for c in ['a', '界', '🦀'] {
            let label = sanitize_client_label(&c.to_string().repeat(300)).unwrap();
            assert!(label.len() <= 200);
            assert!(label.chars().count() <= 120);
            assert_eq!(label.chars().count(), (200 / c.len_utf8()).min(120));
        }
    }

    #[test]
    fn p08b_r1_registered_clients_use_registry_labels() {
        let _env = crate::registry::DataDirTestEnv::new("p08b-r1-registry-label");
        let mut registry = crate::registry::Registry::default();
        registry.http_clients.push(crate::registry::HttpClient { id: "real".into(), label: "My assistant".into(), token_sha256: "unused".into(), profile: String::new(), unknown_fields: Default::default() });
        crate::registry::save(&registry).unwrap();
        assert_eq!(crate::clients::trusted_client_name(Some("client:real"), None), "My assistant");
        assert_eq!(crate::clients::trusted_client_name(Some("client:unknown"), None), "Unknown client");
    }

    #[test]
    fn p08b_r1_adapter_names_come_from_client_definitions() {
        assert_eq!(
            crate::clients::trusted_client_name(Some("adapter:claude-code"), None),
            "Claude Code"
        );
        assert_eq!(
            crate::clients::trusted_client_name(Some("adapter:unknown"), None),
            "unknown"
        );
        assert_eq!(
            crate::clients::trusted_client_name(Some("adapter:claude-code"), Some("Recorded name")),
            "Recorded name"
        );
    }
}

#[cfg(test)]
mod p10c_decision_tests {
    use super::*;

    #[test]
    fn p10c_r1_cancelled_read_poll_retires_without_a_reply_and_keeps_received_answers() {
        struct PollStream {
            registry: crate::downstream::CancelRegistry,
            reply: Option<ApprovalDecision>,
            reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl io::Write for PollStream {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl io::Read for PollStream {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if read == 0 {
                    if self.reply.is_none() {
                        self.registry.cancel("poll", None);
                    }
                    // Model a blocked Windows read waking on its short timeout,
                    // with no reply from the broker to unblock cancellation.
                    return Err(io::ErrorKind::TimedOut.into());
                }
                let reply = self.reply.expect("a cancelled poll must not read again");
                let line = format!("{}\n", serde_json::to_string(&reply).unwrap());
                bytes[..line.len()].copy_from_slice(line.as_bytes());
                // The received answer must survive an immediate disconnect.
                self.registry.cancel("poll", None);
                Ok(line.len())
            }
        }
        let req = ApprovalRequest {
            token: "tok".into(),
            id: "poll".into(),
            client: None,
            client_name: crate::session_observability::current().client_name,
            client_label: None,
            server: "s".into(),
            tool: "t".into(),
            reason: ApprovalReason::Destructive,
            arguments: serde_json::json!({}),
            tool_fingerprint: None,
            url_elicitation: None,
            pii_release: None,
        };
        for reply in [
            None,
            Some(ApprovalDecision::Approved),
            Some(ApprovalDecision::Denied),
        ] {
            let registry = crate::downstream::CancelRegistry::new();
            assert!(registry.begin_client_request("poll".into()));
            let cancel = registry.context("poll".into());
            let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let result = exchange_approval_request_with_cancel(
                &req,
                PollStream {
                    registry,
                    reply,
                    reads: reads.clone(),
                },
                Some(&cancel),
            );
            let expected = reply.unwrap_or(ApprovalDecision::Withdrawn);
            assert!(matches!(result, BrokerAttempt::Decided(actual) if actual == expected));
            assert_eq!(
                reads.load(std::sync::atomic::Ordering::SeqCst),
                if reply.is_some() { 2 } else { 1 }
            );
        }
    }

    #[test]
    fn p10c_r1_human_answer_wins_over_immediate_disconnect() {
        for decision in [ApprovalDecision::Approved, ApprovalDecision::Denied] {
            assert!(
                matches!(broker_attempt_after_cancel(BrokerAttempt::Decided(decision), true), BrokerAttempt::Decided(actual) if actual == decision)
            );
        }
        assert!(matches!(
            broker_attempt_after_cancel(BrokerAttempt::Unreachable, true),
            BrokerAttempt::Decided(ApprovalDecision::Withdrawn)
        ));
    }
}
