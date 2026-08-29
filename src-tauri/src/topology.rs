//! Gateway topology accounting: what role this process plays, and what it holds.
//!
//! Phase 0 of `docs/design/one-gateway-per-host.md`. Today every stdio client session
//! runs its own gateway, which builds its own router and its own copy of every enabled
//! downstream server. Measured on a real machine (SBS-838): three client sessions,
//! 74 processes, 2 GB resident, and the same servers duplicated per gateway.
//!
//! Nothing here changes that. It makes it *countable*, so the daemon/adapter split can
//! be held to a number instead of an impression, and so a regression after the split is
//! a failing assertion rather than a user noticing their fans.
//!
//! Two things are deliberately built now rather than with the topology change:
//!
//! * [`LaunchKey`] is the identity Phase 3 pools on. Defining it here means the pooling
//!   change is "reuse an existing launch for this key" rather than also having to invent
//!   what makes two launches interchangeable.
//! * [`CompatKey`] is what a future daemon rendezvous elects on. Two processes may only
//!   share a runtime if these match, so getting it wrong silently mixes data
//!   directories or protocol eras.
//!
//! Neither type carries secret material. `LaunchKey` records env var NAMES and a digest
//! of their values, never the values, because these snapshots go into the gateway log
//! and the diagnostics bundle a user pastes into a bug report.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;

use crate::registry::sha256_hex;

/// The role a gateway process is playing.
///
/// `Daemon` and `StdioAdapter` are the Phase 2 roles and are not constructed yet. They
/// are named here so the diagnostic's shape does not change when they land, and so the
/// exhaustive matches that report a role fail to compile rather than silently reporting
/// a new role as "standalone".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayRole {
    /// One client's stdio session, owning a full router. Today's default, and the role
    /// whose multiplication SBS-838 is about.
    Standalone,
    /// The desktop-supervised `--http` child serving many registered clients from one
    /// router. Already proves the shared-runtime model.
    HttpBridge,
    /// Phase 2: owns the router and downstream transports for the whole host.
    Daemon,
    /// Phase 2: owns exactly one client's stdin/stdout and forwards to the daemon.
    /// Holds no router and spawns no downstream servers.
    StdioAdapter,
}

impl GatewayRole {
    /// Whether this role builds a router and spawns downstream servers. The point of the
    /// topology change is to make this true for exactly one process per host.
    pub fn owns_downstream(self) -> bool {
        match self {
            GatewayRole::Standalone | GatewayRole::HttpBridge | GatewayRole::Daemon => true,
            GatewayRole::StdioAdapter => false,
        }
    }
}

impl fmt::Display for GatewayRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            GatewayRole::Standalone => "standalone",
            GatewayRole::HttpBridge => "http-bridge",
            GatewayRole::Daemon => "daemon",
            GatewayRole::StdioAdapter => "stdio-adapter",
        };
        f.write_str(s)
    }
}

/// What two processes must agree on before they may share one runtime.
///
/// A daemon rendezvous that ignores any of these would let a client attach to a gateway
/// serving a different data directory, or an incompatible build. Both are silent
/// correctness failures rather than crashes, which is why this is a type with a stable
/// string form rather than an ad-hoc comparison at the call site.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CompatKey {
    /// Gateway build. Two builds may not share a runtime: the wire contract between
    /// adapter and daemon is internal and free to change between releases.
    pub version: String,
    /// Resolved data directory. This is the registry, the pin store, the quarantine
    /// store and the audit log; sharing across data dirs is never correct.
    pub data_dir: String,
}

impl CompatKey {
    pub fn new(version: impl Into<String>, data_dir: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            data_dir: data_dir.into(),
        }
    }

    /// Stable, comparable form. Hashed so a rendezvous descriptor can carry it without
    /// publishing the user's home directory layout.
    pub fn fingerprint(&self) -> String {
        sha256_hex(&format!("{}\u{0}{}", self.version, self.data_dir))
    }
}

/// What makes two downstream launches interchangeable.
///
/// Phase 3 pools on this: two sessions asking for the same key may share one child
/// process, and two sessions with different keys may not. Everything that would change
/// the child's behaviour has to be in here, or pooling would hand a session a server
/// launched with someone else's environment or working directory.
///
/// `${ROOT}` is why `cwd` is part of the key rather than incidental: the same server
/// entry launched from two different project roots is two different servers, and
/// collapsing them would route a client's calls at another client's files.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct LaunchKey {
    /// Registry server id. Present unhashed because it is already user-facing.
    pub server: String,
    /// `stdio` or `remote`.
    pub kind: &'static str,
    /// Digest over the launch parameters: command, args, resolved cwd, the NAMES of the
    /// env vars supplied, and the secrets generation. Never the env values.
    pub digest: String,
}

impl LaunchKey {
    /// Key a stdio launch.
    ///
    /// `env_names` is deliberately just the names. Two launches that differ only in the
    /// VALUE of a secret must still be distinguishable, which is what `secrets_generation`
    /// is for: it changes whenever a vaulted value changes, so a rotated credential
    /// produces a new key without the value ever entering the digest.
    pub fn stdio(
        server: &str,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        env_names: &BTreeSet<String>,
        secrets_generation: u64,
    ) -> Self {
        let mut material = String::new();
        material.push_str(command);
        for a in args {
            material.push('\u{0}');
            material.push_str(a);
        }
        material.push('\u{1}');
        material.push_str(cwd.unwrap_or(""));
        material.push('\u{2}');
        for name in env_names {
            material.push_str(name);
            material.push('\u{0}');
        }
        material.push('\u{3}');
        material.push_str(&secrets_generation.to_string());
        Self {
            server: server.to_string(),
            kind: "stdio",
            digest: sha256_hex(&material),
        }
    }

    /// Key a remote launch. The URL is hashed rather than stored: it can carry a token
    /// in userinfo or a query string, and these snapshots are pasted into bug reports.
    pub fn remote(server: &str, url: &str, secrets_generation: u64) -> Self {
        let material = format!("{url}\u{3}{secrets_generation}");
        Self {
            server: server.to_string(),
            kind: "remote",
            digest: sha256_hex(&material),
        }
    }
}

/// One process's contribution to the host's topology.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologySnapshot {
    pub role: GatewayRole,
    pub compat: String,
    /// Live client sessions this process is serving. Standalone is always 1.
    pub sessions: usize,
    /// Every downstream launch this process owns, in a stable order.
    pub launches: Vec<LaunchKey>,
}

impl TopologySnapshot {
    pub fn new(
        role: GatewayRole,
        compat: &CompatKey,
        sessions: usize,
        launches: Vec<LaunchKey>,
    ) -> Self {
        let mut launches = launches;
        launches.sort();
        Self {
            role,
            compat: compat.fingerprint(),
            sessions,
            launches,
        }
    }

    /// One line for the gateway log. Deliberately terse and secret-free, because it is
    /// written on every router build.
    pub fn log_line(&self) -> String {
        format!(
            "topology: role={} compat={} sessions={} launches={}",
            self.role,
            &self.compat[..12.min(self.compat.len())],
            self.sessions,
            self.launches.len()
        )
    }
}

/// The whole host, assembled from every gateway's snapshot.
///
/// This is the acceptance measure for the topology change. `duplication_factor` is the
/// number the daemon/adapter split has to move: it is total launches divided by distinct
/// launches, so 1.0 means every downstream server runs once on the machine and 3.0 means
/// three copies of each are running because three sessions are open.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostTopology {
    pub processes: usize,
    pub sessions: usize,
    pub total_launches: usize,
    pub distinct_launches: usize,
    /// Processes that own a router, keyed by role. More than one router-owning process
    /// per compat key is the condition SBS-838 exists to remove.
    pub router_owners: usize,
}

impl HostTopology {
    pub fn from_snapshots(snapshots: &[TopologySnapshot]) -> Self {
        let mut distinct: BTreeSet<&LaunchKey> = BTreeSet::new();
        let mut total = 0usize;
        let mut sessions = 0usize;
        let mut router_owners = 0usize;
        for s in snapshots {
            sessions += s.sessions;
            if s.role.owns_downstream() {
                router_owners += 1;
            }
            for l in &s.launches {
                total += 1;
                distinct.insert(l);
            }
        }
        Self {
            processes: snapshots.len(),
            sessions,
            total_launches: total,
            distinct_launches: distinct.len(),
            router_owners,
        }
    }

    /// Copies of each downstream server running on the host. 1.0 is the Phase 3 goal.
    /// Returns 1.0 for an empty host rather than dividing by zero, since "no launches"
    /// is not duplication.
    pub fn duplication_factor(&self) -> f64 {
        if self.distinct_launches == 0 {
            return 1.0;
        }
        self.total_launches as f64 / self.distinct_launches as f64
    }

    /// Group snapshots by compat key. Only processes sharing a key are candidates for
    /// sharing a runtime, so a host with two data dirs legitimately has two daemons.
    pub fn by_compat(snapshots: &[TopologySnapshot]) -> BTreeMap<String, Vec<&TopologySnapshot>> {
        let mut out: BTreeMap<String, Vec<&TopologySnapshot>> = BTreeMap::new();
        for s in snapshots {
            out.entry(s.compat.clone()).or_default().push(s);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn launch(server: &str, cwd: Option<&str>) -> LaunchKey {
        LaunchKey::stdio(
            server,
            "npx",
            &["-y".to_string(), format!("{server}-mcp")],
            cwd,
            &names(&["API_KEY"]),
            7,
        )
    }

    /// The digest must never carry a secret value, because these snapshots go into the
    /// gateway log and the diagnostics blob users paste into bug reports.
    #[test]
    fn a_launch_key_never_embeds_env_values_or_urls() {
        let key = LaunchKey::stdio(
            "gh",
            "npx",
            &["-y".into(), "gh-mcp".into()],
            None,
            &names(&["GITHUB_TOKEN"]),
            1,
        );
        let encoded = serde_json::to_string(&key).unwrap();
        assert!(
            !encoded.contains("ghp_"),
            "no value should be reachable: {encoded}"
        );
        // The NAME is fine and useful; the value never enters the material at all.
        assert!(encoded.contains("gh"));

        let remote = LaunchKey::remote("api", "https://user:hunter2@example.com/mcp?k=abc", 1);
        let encoded = serde_json::to_string(&remote).unwrap();
        assert!(
            !encoded.contains("hunter2"),
            "userinfo must not survive: {encoded}"
        );
        assert!(
            !encoded.contains("example.com"),
            "host must not survive: {encoded}"
        );
    }

    /// Two launches may only be pooled when everything that changes the child's
    /// behaviour matches. Each of these differences must produce a different key.
    #[test]
    fn launch_keys_separate_anything_that_changes_the_child() {
        let base = launch("gh", None);
        assert_eq!(base, launch("gh", None), "identical launches pool");

        // ${ROOT} sharding: the same server from two project roots is two servers, and
        // collapsing them would run a client's calls against another client's files.
        assert_ne!(base, launch("gh", Some("C:/a")), "cwd must split");
        assert_ne!(launch("gh", Some("C:/a")), launch("gh", Some("C:/b")));

        // A rotated credential must not be served from a pool entry launched with the
        // old value, even though the value itself is never in the digest.
        let rotated = LaunchKey::stdio(
            "gh",
            "npx",
            &["-y".into(), "gh-mcp".into()],
            None,
            &names(&["API_KEY"]),
            8,
        );
        assert_ne!(base, rotated, "secrets generation must split");

        // A new env var changes the child's behaviour even if the values are unknown.
        let extra = LaunchKey::stdio(
            "gh",
            "npx",
            &["-y".into(), "gh-mcp".into()],
            None,
            &names(&["API_KEY", "GH_HOST"]),
            7,
        );
        assert_ne!(base, extra, "env var set must split");

        // Args and command, obviously.
        let other_args =
            LaunchKey::stdio("gh", "npx", &["-y".into()], None, &names(&["API_KEY"]), 7);
        assert_ne!(base, other_args);
        let other_cmd = LaunchKey::stdio(
            "gh",
            "uvx",
            &["-y".into(), "gh-mcp".into()],
            None,
            &names(&["API_KEY"]),
            7,
        );
        assert_ne!(base, other_cmd);
    }

    /// Env var ORDER must not split a key: the set is what matters, and a map iteration
    /// order change would otherwise silently halve pool hit rate in Phase 3.
    #[test]
    fn launch_keys_are_stable_across_env_ordering() {
        let a = LaunchKey::stdio("gh", "npx", &[], None, &names(&["B", "A", "C"]), 1);
        let b = LaunchKey::stdio("gh", "npx", &[], None, &names(&["C", "B", "A"]), 1);
        assert_eq!(a, b);
    }

    /// Two processes may only share a runtime when build and data dir both match.
    #[test]
    fn compat_keys_separate_builds_and_data_dirs() {
        let a = CompatKey::new("1.13.0", "C:/data");
        assert_eq!(
            a.fingerprint(),
            CompatKey::new("1.13.0", "C:/data").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            CompatKey::new("1.14.0", "C:/data").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            CompatKey::new("1.13.0", "D:/data").fingerprint()
        );
        // The fingerprint is what a rendezvous descriptor would publish, so it must not
        // leak the path it was built from.
        assert!(!a.fingerprint().contains("data"));
    }

    /// The Phase 0 baseline, expressed as an executable assertion.
    ///
    /// This is today's topology, measured on a real machine in SBS-838: three client
    /// sessions, three router-owning processes, every downstream server launched once
    /// per session. It is written as a test so the daemon/adapter split has a red/green
    /// target rather than a paragraph, and so a regression after the split fails here.
    ///
    /// Phase 3 changes the expected `duplication_factor` to 1.0 and `router_owners` to
    /// 1. Until then this documents what we actually ship.
    #[test]
    fn today_every_session_duplicates_every_downstream_server() {
        let compat = CompatKey::new("1.13.0", "C:/data");
        // Three stdio sessions, each a full gateway with the same four enabled servers.
        let servers = ["gh", "linear", "sentry", "fs"];
        let snapshots: Vec<TopologySnapshot> = (0..3)
            .map(|_| {
                TopologySnapshot::new(
                    GatewayRole::Standalone,
                    &compat,
                    1,
                    servers.iter().map(|s| launch(s, None)).collect(),
                )
            })
            .collect();

        let host = HostTopology::from_snapshots(&snapshots);

        assert_eq!(host.sessions, 3);
        assert_eq!(
            host.router_owners, 3,
            "one router per session is the defect"
        );
        assert_eq!(host.total_launches, 12, "4 servers x 3 sessions");
        assert_eq!(
            host.distinct_launches, 4,
            "but only 4 distinct servers exist"
        );
        assert_eq!(
            host.duplication_factor(),
            3.0,
            "three copies of every downstream server are running"
        );
        // Every one of them shares a compat key, so every one of them is a candidate to
        // be collapsed into a single daemon. That is the whole argument for Phase 2.
        assert_eq!(HostTopology::by_compat(&snapshots).len(), 1);
    }

    /// The shape Phase 2 and 3 are aiming at, asserted now so the target is unambiguous:
    /// one daemon owning the launches, N adapters owning only their client's stdio.
    #[test]
    fn the_target_topology_runs_each_downstream_server_once() {
        let compat = CompatKey::new("1.13.0", "C:/data");
        let servers = ["gh", "linear", "sentry", "fs"];
        let mut snapshots = vec![TopologySnapshot::new(
            GatewayRole::Daemon,
            &compat,
            3,
            servers.iter().map(|s| launch(s, None)).collect(),
        )];
        for _ in 0..3 {
            snapshots.push(TopologySnapshot::new(
                GatewayRole::StdioAdapter,
                &compat,
                1,
                vec![],
            ));
        }

        let host = HostTopology::from_snapshots(&snapshots);

        assert_eq!(host.router_owners, 1, "exactly one process owns downstream");
        assert_eq!(host.total_launches, 4);
        assert_eq!(host.duplication_factor(), 1.0);
        // The adapters still exist as processes; the win is that they hold nothing.
        assert_eq!(host.processes, 4);
    }

    /// Two data directories on one host are legitimately two runtimes. A rendezvous that
    /// ignored the compat key would attach a client to a gateway serving someone else's
    /// registry, pin store and audit log.
    #[test]
    fn different_data_dirs_are_never_pooled_together() {
        let a = TopologySnapshot::new(
            GatewayRole::Standalone,
            &CompatKey::new("1.13.0", "C:/a"),
            1,
            vec![launch("gh", None)],
        );
        let b = TopologySnapshot::new(
            GatewayRole::Standalone,
            &CompatKey::new("1.13.0", "C:/b"),
            1,
            vec![launch("gh", None)],
        );
        assert_eq!(HostTopology::by_compat(&[a, b]).len(), 2);
    }

    /// An adapter holds no router, so it must never be counted as a downstream owner.
    #[test]
    fn only_router_owning_roles_count_as_downstream_owners() {
        assert!(GatewayRole::Standalone.owns_downstream());
        assert!(GatewayRole::HttpBridge.owns_downstream());
        assert!(GatewayRole::Daemon.owns_downstream());
        assert!(!GatewayRole::StdioAdapter.owns_downstream());
    }

    #[test]
    fn an_empty_host_is_not_reported_as_duplicated() {
        let host = HostTopology::from_snapshots(&[]);
        assert_eq!(host.duplication_factor(), 1.0);
        assert_eq!(host.processes, 0);
    }

    #[test]
    fn the_log_line_is_terse_and_carries_no_path_or_secret() {
        let snap = TopologySnapshot::new(
            GatewayRole::Standalone,
            &CompatKey::new("1.13.0", "C:/Users/someone/AppData/Roaming/Toolport"),
            1,
            vec![launch("gh", Some("C:/Users/someone/projects/secret-thing"))],
        );
        let line = snap.log_line();
        assert!(line.contains("role=standalone"));
        assert!(line.contains("launches=1"));
        assert!(!line.contains("someone"), "no path material: {line}");
        assert!(!line.contains("secret-thing"), "no path material: {line}");
    }
}
