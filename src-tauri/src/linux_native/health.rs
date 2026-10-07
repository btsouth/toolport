//! When the Servers page probes a server. A probe launches the server (or sends
//! a remote initialize), so it must follow configuration changes and explicit
//! requests, never filtering, focus or re-renders. Results are cached per server
//! against a fingerprint of its definition, and each server has at most one
//! probe in flight.

use crate::server_runtime::ProbeResult;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How old a result may be before opening the Servers page checks it again.
pub(super) const HEALTH_STALE_AFTER: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeReason {
    /// The registry was loaded or changed: probe new or changed servers only.
    ConfigChanged,
    /// The Servers page was opened: also refresh results older than
    /// `HEALTH_STALE_AFTER`.
    Stale,
    /// The user asked for a fresh check, or credentials outside the registry
    /// changed.
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProbeTicket {
    pub(super) server_id: String,
    token: u64,
}

#[derive(Debug, PartialEq)]
pub(super) enum ProbeOutcome {
    /// Show this result on the server's row.
    Apply,
    /// The result described an older definition; this probe replaces it.
    Rerun(ProbeTicket),
    /// The server was removed, disabled or superseded meanwhile.
    Discard,
}

struct Cached {
    fingerprint: u64,
    checked_at: Instant,
    result: ProbeResult,
}

struct InFlight {
    token: u64,
    fingerprint: u64,
    /// Whether the result still describes an enabled server's definition.
    wanted: bool,
    /// The definition to probe once this probe finishes, when it went stale
    /// while running. Probing again only then keeps one process per server.
    rerun: Option<u64>,
}

#[derive(Default)]
pub(super) struct HealthCache {
    results: HashMap<String, Cached>,
    in_flight: HashMap<String, InFlight>,
    next_token: u64,
}

impl HealthCache {
    pub(super) fn result(&self, server_id: &str) -> Option<&ProbeResult> {
        self.results.get(server_id).map(|cached| &cached.result)
    }

    pub(super) fn is_checking(&self, server_id: &str) -> bool {
        self.in_flight.contains_key(server_id)
    }

    /// Forget a server's result, for changes the fingerprint cannot see such as
    /// keychain credentials. The next plan probes it again.
    pub(super) fn invalidate(&mut self, server_id: &str) {
        self.results.remove(server_id);
        if let Some(flight) = self.in_flight.get_mut(server_id) {
            flight.wanted = false;
            flight.rerun = Some(flight.fingerprint);
        }
    }

    /// Decide which enabled servers need a probe. `enabled` pairs each enabled
    /// server id with its definition fingerprint.
    pub(super) fn plan(
        &mut self,
        enabled: &[(&str, u64)],
        reason: ProbeReason,
        now: Instant,
    ) -> Vec<ProbeTicket> {
        self.results
            .retain(|id, _| enabled.iter().any(|(enabled_id, _)| enabled_id == id));
        for (id, flight) in self.in_flight.iter_mut() {
            if !enabled.iter().any(|(enabled_id, _)| enabled_id == id) {
                flight.wanted = false;
                flight.rerun = None;
            }
        }

        let mut tickets = Vec::new();
        for &(id, fingerprint) in enabled {
            let needed = match self.results.get(id) {
                None => true,
                Some(cached) => {
                    reason == ProbeReason::Refresh
                        || cached.fingerprint != fingerprint
                        || (reason == ProbeReason::Stale
                            && now.saturating_duration_since(cached.checked_at)
                                >= HEALTH_STALE_AFTER)
                }
            };
            if let Some(flight) = self.in_flight.get_mut(id) {
                if flight.rerun.is_some() {
                    flight.rerun = Some(fingerprint);
                } else if flight.fingerprint == fingerprint && reason != ProbeReason::Refresh {
                    flight.wanted = true;
                } else if needed {
                    flight.wanted = false;
                    flight.rerun = Some(fingerprint);
                }
                continue;
            }
            if needed {
                tickets.push(self.start(id, fingerprint));
            }
        }
        tickets
    }

    /// Record a finished probe. Results from a cancelled or superseded probe
    /// never reach the cache.
    pub(super) fn complete(
        &mut self,
        ticket: &ProbeTicket,
        result: ProbeResult,
        now: Instant,
    ) -> ProbeOutcome {
        match self.in_flight.get(&ticket.server_id) {
            Some(flight) if flight.token == ticket.token => {}
            _ => return ProbeOutcome::Discard,
        }
        let flight = self
            .in_flight
            .remove(&ticket.server_id)
            .expect("the in-flight entry was just found");
        if let Some(fingerprint) = flight.rerun {
            return ProbeOutcome::Rerun(self.start(&ticket.server_id, fingerprint));
        }
        if !flight.wanted {
            return ProbeOutcome::Discard;
        }
        self.results.insert(
            ticket.server_id.clone(),
            Cached {
                fingerprint: flight.fingerprint,
                checked_at: now,
                result,
            },
        );
        ProbeOutcome::Apply
    }

    /// Ready, needs sign-in, failing and still checking, over enabled servers.
    pub(super) fn tally<'a>(
        &self,
        enabled: impl IntoIterator<Item = &'a str>,
    ) -> (usize, usize, usize, usize) {
        let (mut ready, mut auth, mut errors, mut pending) = (0, 0, 0, 0);
        for id in enabled {
            match self.results.get(id) {
                _ if self.in_flight.contains_key(id) => pending += 1,
                None => pending += 1,
                Some(cached) if cached.result.ok => ready += 1,
                Some(cached) if cached.result.auth_required => auth += 1,
                Some(_) => errors += 1,
            }
        }
        (ready, auth, errors, pending)
    }

    fn start(&mut self, server_id: &str, fingerprint: u64) -> ProbeTicket {
        self.next_token = self.next_token.wrapping_add(1);
        self.in_flight.insert(
            server_id.to_string(),
            InFlight {
                token: self.next_token,
                fingerprint,
                wanted: true,
                rerun: None,
            },
        );
        ProbeTicket {
            server_id: server_id.to_string(),
            token: self.next_token,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(id: &str) -> ProbeResult {
        ProbeResult {
            server_id: id.into(),
            ok: true,
            tool_count: 1,
            error: None,
            auth_required: false,
        }
    }

    fn ids(tickets: &[ProbeTicket]) -> Vec<&str> {
        tickets
            .iter()
            .map(|ticket| ticket.server_id.as_str())
            .collect()
    }

    fn settle(cache: &mut HealthCache, tickets: Vec<ProbeTicket>, now: Instant) {
        for ticket in tickets {
            let id = ticket.server_id.clone();
            assert_eq!(cache.complete(&ticket, ok(&id), now), ProbeOutcome::Apply);
        }
    }

    #[test]
    fn rerendering_the_same_registry_never_probes_again() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let enabled = [("a", 1), ("b", 2)];
        let first = cache.plan(&enabled, ProbeReason::ConfigChanged, now);
        assert_eq!(ids(&first), ["a", "b"]);
        // A filter keystroke or a focus change re-renders while probes run, and
        // again after they finish: neither may launch a server.
        assert!(cache
            .plan(&enabled, ProbeReason::ConfigChanged, now)
            .is_empty());
        settle(&mut cache, first, now);
        for _ in 0..3 {
            assert!(cache
                .plan(&enabled, ProbeReason::ConfigChanged, now)
                .is_empty());
        }
        assert_eq!(cache.tally(["a", "b"]), (2, 0, 0, 0));
    }

    #[test]
    fn a_changed_definition_probes_only_that_server() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let first = cache.plan(&[("a", 1), ("b", 2)], ProbeReason::ConfigChanged, now);
        settle(&mut cache, first, now);
        let next = cache.plan(&[("a", 1), ("b", 3)], ProbeReason::ConfigChanged, now);
        assert_eq!(ids(&next), ["b"]);
    }

    #[test]
    fn stale_results_refresh_only_when_the_page_opens_after_the_interval() {
        let mut cache = HealthCache::default();
        let start = Instant::now();
        let enabled = [("a", 1)];
        let first = cache.plan(&enabled, ProbeReason::ConfigChanged, start);
        settle(&mut cache, first, start);
        let soon = start + Duration::from_secs(60);
        assert!(cache.plan(&enabled, ProbeReason::Stale, soon).is_empty());
        let later = start + HEALTH_STALE_AFTER;
        assert!(cache
            .plan(&enabled, ProbeReason::ConfigChanged, later)
            .is_empty());
        assert_eq!(ids(&cache.plan(&enabled, ProbeReason::Stale, later)), ["a"]);
    }

    #[test]
    fn a_change_during_a_probe_waits_for_it_and_drops_its_result() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let first = cache.plan(&[("a", 1)], ProbeReason::ConfigChanged, now);
        assert!(cache
            .plan(&[("a", 2)], ProbeReason::ConfigChanged, now)
            .is_empty());
        assert!(cache
            .plan(&[("a", 2)], ProbeReason::Refresh, now)
            .is_empty());
        let ProbeOutcome::Rerun(second) = cache.complete(&first[0], ok("a"), now) else {
            panic!("a probe of the old definition must be rerun");
        };
        assert!(cache.result("a").is_none());
        assert_eq!(cache.complete(&second, ok("a"), now), ProbeOutcome::Apply);
        assert!(cache
            .plan(&[("a", 2)], ProbeReason::ConfigChanged, now)
            .is_empty());
    }

    #[test]
    fn removed_servers_discard_late_results_and_forget_cached_ones() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let first = cache.plan(&[("a", 1), ("b", 1)], ProbeReason::ConfigChanged, now);
        assert!(cache
            .plan(&[("b", 1)], ProbeReason::ConfigChanged, now)
            .is_empty());
        assert_eq!(
            cache.complete(&first[0], ok("a"), now),
            ProbeOutcome::Discard
        );
        assert_eq!(cache.complete(&first[1], ok("b"), now), ProbeOutcome::Apply);
        assert!(cache.result("a").is_none());
        // Re-enabling a server checks it again rather than trusting old health.
        cache.plan(&[], ProbeReason::ConfigChanged, now);
        assert_eq!(
            ids(&cache.plan(&[("b", 1)], ProbeReason::ConfigChanged, now)),
            ["b"]
        );
    }

    #[test]
    fn refresh_and_invalidate_probe_again_without_overlapping() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let first = cache.plan(&[("a", 1)], ProbeReason::ConfigChanged, now);
        settle(&mut cache, first, now);
        let refresh = cache.plan(&[("a", 1)], ProbeReason::Refresh, now);
        assert_eq!(ids(&refresh), ["a"]);
        assert!(cache.is_checking("a"));
        // Credentials changed while that probe ran: it must not be trusted.
        cache.invalidate("a");
        assert!(cache
            .plan(&[("a", 1)], ProbeReason::ConfigChanged, now)
            .is_empty());
        let ProbeOutcome::Rerun(next) = cache.complete(&refresh[0], ok("a"), now) else {
            panic!("an invalidated probe must be rerun");
        };
        assert_eq!(cache.tally(["a"]), (0, 0, 0, 1));
        assert_eq!(cache.complete(&next, ok("a"), now), ProbeOutcome::Apply);
        // A ticket from an older round never lands.
        assert_eq!(
            cache.complete(&refresh[0], ok("a"), now),
            ProbeOutcome::Discard
        );
    }
}
