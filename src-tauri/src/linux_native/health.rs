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
    /// The keyring was found open: check again the servers that showed it
    /// locked, without asking.
    Unlocked,
}

/// Enabled servers by health state. The page's summary reads each result
/// (see `server_posture_copy`); the tests use this count to check the cache.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Tally {
    pub(super) ready: usize,
    pub(super) auth: usize,
    pub(super) errors: usize,
    pub(super) locked: usize,
    pub(super) pending: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProbeTicket {
    pub(super) server_id: String,
    token: u64,
    /// The user asked for this check (Refresh, or Retry on this server), so it
    /// may ask to unlock the keyring. Checks the page starts never do.
    pub(super) interactive: bool,
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
    /// This probe was the user's.
    interactive: bool,
    /// A user's Refresh or Retry is waiting for this probe to finish: the
    /// rerun is theirs, not this probe's.
    rerun_interactive: bool,
}

#[derive(Default)]
pub(super) struct HealthCache {
    results: HashMap<String, Cached>,
    in_flight: HashMap<String, InFlight>,
    next_token: u64,
    /// The page polls the keyring while a row shows it locked.
    polling_keyring: bool,
}

impl HealthCache {
    pub(super) fn result(&self, server_id: &str) -> Option<&ProbeResult> {
        self.results.get(server_id).map(|cached| &cached.result)
    }

    /// A probe of this server is running.
    pub(super) fn checking(&self, server_id: &str) -> bool {
        self.in_flight.contains_key(server_id)
    }

    /// Some row shows "Keyring locked".
    pub(super) fn has_locked(&self) -> bool {
        self.results
            .values()
            .any(|cached| cached.result.keyring_locked)
    }

    /// Whether to start polling the keyring: some row shows it locked and no
    /// poll runs yet. [`Self::keep_polling`] ends it.
    pub(super) fn start_keyring_poll(&mut self) -> bool {
        let start = self.has_locked() && !self.polling_keyring;
        self.polling_keyring |= start;
        start
    }

    /// The poll goes on while a row shows the keyring locked.
    pub(super) fn keep_polling(&mut self) -> bool {
        self.polling_keyring = self.has_locked();
        self.polling_keyring
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
    #[cfg(test)]
    pub(super) fn plan(
        &mut self,
        enabled: &[(&str, u64)],
        reason: ProbeReason,
        now: Instant,
    ) -> Vec<ProbeTicket> {
        self.plan_asked(enabled, reason, now, None)
    }

    /// [`Self::plan`] for a check the user asked for: a Refresh asks for every
    /// server, a Retry for `retried`. Their tickets, and a rerun queued behind a
    /// probe already running, are interactive.
    pub(super) fn plan_asked(
        &mut self,
        enabled: &[(&str, u64)],
        reason: ProbeReason,
        now: Instant,
        retried: Option<&str>,
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
            let asked = reason == ProbeReason::Refresh || retried == Some(id);
            let needed = match self.results.get(id) {
                None => true,
                Some(cached) => {
                    reason == ProbeReason::Refresh
                        || cached.fingerprint != fingerprint
                        || (reason == ProbeReason::Stale
                            && (now.saturating_duration_since(cached.checked_at)
                                >= HEALTH_STALE_AFTER
                                // "Keyring locked" says nothing about the
                                // server, and may only mean the keyring was
                                // busy: check again whenever the page opens.
                                || cached.result.keyring_locked))
                        || (reason == ProbeReason::Unlocked && cached.result.keyring_locked)
                }
            };
            if let Some(flight) = self.in_flight.get_mut(id) {
                if flight.rerun.is_some() {
                    flight.rerun = Some(fingerprint);
                    flight.rerun_interactive |= asked;
                } else if flight.fingerprint == fingerprint && !asked {
                    flight.wanted = true;
                } else if needed || asked {
                    flight.wanted = false;
                    flight.rerun = Some(fingerprint);
                    flight.rerun_interactive |= asked;
                }
                continue;
            }
            if needed {
                tickets.push(self.start(id, fingerprint, asked));
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
            // A rerun that replaces the user's probe, or that the user queued,
            // is still theirs.
            let interactive = flight.interactive || flight.rerun_interactive;
            return ProbeOutcome::Rerun(self.start(&ticket.server_id, fingerprint, interactive));
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
    #[cfg(test)]
    pub(super) fn tally<'a>(&self, enabled: impl IntoIterator<Item = &'a str>) -> Tally {
        let mut tally = Tally::default();
        for id in enabled {
            match self.results.get(id) {
                _ if self.in_flight.contains_key(id) => tally.pending += 1,
                None => tally.pending += 1,
                Some(cached) if cached.result.ok => tally.ready += 1,
                Some(cached) if cached.result.auth_required => tally.auth += 1,
                // Not checked: the keyring is locked. Not a failure.
                Some(cached) if cached.result.keyring_locked => tally.locked += 1,
                Some(_) => tally.errors += 1,
            }
        }
        tally
    }

    fn start(&mut self, server_id: &str, fingerprint: u64, interactive: bool) -> ProbeTicket {
        self.next_token = self.next_token.wrapping_add(1);
        self.in_flight.insert(
            server_id.to_string(),
            InFlight {
                token: self.next_token,
                fingerprint,
                wanted: true,
                rerun: None,
                interactive,
                rerun_interactive: false,
            },
        );
        ProbeTicket {
            server_id: server_id.to_string(),
            token: self.next_token,
            interactive,
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
            failure: None,
            auth_target: None,
            keyring_locked: false,
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

    /// Failure mode: after a Retry on one locked row, or an unlock by
    /// anything else, the other rows kept "Keyring locked" until Refresh.
    #[test]
    fn an_open_keyring_checks_every_locked_row_again_and_only_those() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let enabled = [("a", 1), ("b", 2), ("c", 3)];
        for ticket in cache.plan(&enabled, ProbeReason::ConfigChanged, now) {
            let mut result = ok(&ticket.server_id);
            result.ok = ticket.server_id == "c";
            result.keyring_locked = !result.ok;
            assert_eq!(cache.complete(&ticket, result, now), ProbeOutcome::Apply);
        }
        assert!(cache.start_keyring_poll());
        assert!(!cache.start_keyring_poll(), "one poll");
        let again = cache.plan(&enabled, ProbeReason::Unlocked, now);
        assert_eq!(ids(&again), ["a", "b"]);
        assert!(again.iter().all(|ticket| !ticket.interactive));
        assert!(cache.plan(&enabled, ProbeReason::Unlocked, now).is_empty());
        assert!(cache.keep_polling(), "still showing locked while checked");
        settle(&mut cache, again, now);
        assert!(!cache.has_locked());
        assert!(!cache.keep_polling());
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
        assert_eq!(
            cache.tally(["a", "b"]),
            Tally {
                ready: 2,
                ..Tally::default()
            }
        );
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

    /// Failure mode: a Refresh or Retry during a check the page started only
    /// queued a rerun, and the rerun took the finished check's
    /// `interactive = false`, so a locked keyring stayed "Keyring locked".
    #[test]
    fn a_refresh_or_retry_queued_behind_an_automatic_probe_stays_the_users() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let first = cache.plan(&[("a", 1), ("b", 1)], ProbeReason::ConfigChanged, now);
        assert!(first.iter().all(|ticket| !ticket.interactive));

        // Refresh while both run: nothing starts now, both reruns are the user's.
        assert!(cache
            .plan_asked(&[("a", 1), ("b", 1)], ProbeReason::Refresh, now, None)
            .is_empty());
        for ticket in &first {
            let ProbeOutcome::Rerun(next) = cache.complete(ticket, ok(&ticket.server_id), now)
            else {
                panic!("a refresh during a probe must rerun it");
            };
            assert!(next.interactive, "{next:?}");
            assert_eq!(
                cache.complete(&next, ok(&next.server_id), now),
                ProbeOutcome::Apply
            );
        }

        // A row's Retry on one server while the page checks both again.
        let again = cache.plan(&[("a", 2), ("b", 2)], ProbeReason::ConfigChanged, now);
        cache.invalidate("a");
        assert!(cache
            .plan_asked(
                &[("a", 2), ("b", 2)],
                ProbeReason::ConfigChanged,
                now,
                Some("a")
            )
            .is_empty());
        for ticket in &again {
            match cache.complete(ticket, ok(&ticket.server_id), now) {
                ProbeOutcome::Rerun(next) => {
                    assert_eq!(next.server_id, "a");
                    assert!(next.interactive, "the retried server's rerun is the user's");
                }
                ProbeOutcome::Apply => assert_eq!(ticket.server_id, "b"),
                ProbeOutcome::Discard => panic!("nothing was removed"),
            }
        }
    }

    /// Failure mode: a check that found the keyring locked (or only busy)
    /// stayed "Keyring locked" for five minutes of page opens, after the
    /// keyring was unlocked, unless the user chose Retry.
    #[test]
    fn a_keyring_locked_result_is_checked_again_when_the_page_opens() {
        let mut cache = HealthCache::default();
        let now = Instant::now();
        let first = cache.plan(&[("a", 1), ("b", 1)], ProbeReason::ConfigChanged, now);
        for ticket in first {
            let mut result = ok(&ticket.server_id);
            if ticket.server_id == "a" {
                result.ok = false;
                result.keyring_locked = true;
            }
            assert_eq!(cache.complete(&ticket, result, now), ProbeOutcome::Apply);
        }
        let soon = now + Duration::from_secs(1);
        let again = cache.plan(&[("a", 1), ("b", 1)], ProbeReason::Stale, soon);
        assert_eq!(
            ids(&again),
            ["a"],
            "only the locked result is checked again"
        );
        assert!(!again[0].interactive, "the page's own check never asks");
        // Re-renders and config reloads still never probe it on their own.
        assert!(cache
            .plan(&[("a", 1), ("b", 1)], ProbeReason::ConfigChanged, soon)
            .is_empty());
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
        assert_eq!(
            cache.tally(["a"]),
            Tally {
                pending: 1,
                ..Tally::default()
            }
        );
        assert_eq!(cache.complete(&next, ok("a"), now), ProbeOutcome::Apply);
        // A ticket from an older round never lands.
        assert_eq!(
            cache.complete(&refresh[0], ok("a"), now),
            ProbeOutcome::Discard
        );
    }
}
