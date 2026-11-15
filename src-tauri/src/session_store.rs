//! Session-scoped state with an explicit lifetime (one-gateway-per-host P1.2).
//!
//! The gateway keys several maps by session or principal: the PII pseudonym map
//! (`pii_sessions`), shaped-result cursors, and the modern HITL approval table.
//! Each is a process global with its own ad hoc lifetime, and nothing bounds how
//! long a value outlives the session that created it. This is the store they move
//! onto: one place that owns the rules, so a session's state is released on close
//! and cannot outlive its TTL or grow past a cap.
//!
//! Nothing is wired to it yet. This is the primitive, the same way the daemon
//! rendezvous landed as a module before anything dialed it. The extraction onto it
//! is the next slice of P1.2.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A map keyed by session or principal, with a per-entry TTL and a hard cap.
///
/// Not internally synchronized: callers that share one across threads hold it in a
/// mutex, which is how the globals it replaces are held today.
pub struct SessionStore<V> {
    entries: HashMap<String, Entry<V>>,
    ttl: Duration,
    cap: usize,
    /// Monotonic insert order, used to evict the oldest entry deterministically
    /// (an `Instant` can tie on a coarse clock).
    next_seq: u64,
}

/// An entry taken out by [`SessionStore::pop_oldest`].
pub struct Evicted<V> {
    pub key: String,
    pub value: V,
    /// When the entry was last touched; its TTL runs from here.
    pub last_seen: Instant,
    seq: u64,
}

struct Entry<V> {
    value: V,
    last_seen: Instant,
    seq: u64,
}

impl<V> SessionStore<V> {
    /// A store that expires an entry `ttl` after it was last touched and holds at
    /// most `cap` entries. The cap is clamped to at least one.
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
            cap: cap.max(1),
            next_seq: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert or replace `key`, reaping expired entries first and evicting the
    /// least recently inserted if that would exceed the cap.
    pub fn insert(&mut self, key: &str, value: V) {
        self.reap_expired();
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.insert(
            key.to_string(),
            Entry {
                value,
                last_seen: Instant::now(),
                seq,
            },
        );
        while self.entries.len() > self.cap {
            self.remove_oldest();
        }
    }

    /// Run `f` on the entry for `key`, refreshing its TTL. `None` when the key is
    /// absent or already expired.
    pub fn with<R>(&mut self, key: &str, f: impl FnOnce(&mut V) -> R) -> Option<R> {
        self.with_at(key, Instant::now(), f)
    }

    /// [`with`](Self::with) as of `now`, so a caller can pass its clock in.
    pub fn with_at<R>(
        &mut self,
        key: &str,
        now: Instant,
        f: impl FnOnce(&mut V) -> R,
    ) -> Option<R> {
        if self.is_expired_at(key, now) {
            self.entries.remove(key);
            return None;
        }
        let entry = self.entries.get_mut(key)?;
        entry.last_seen = now;
        Some(f(&mut entry.value))
    }

    /// Run `f` on the entry for `key`, creating it with `make` first if it is
    /// absent. The new or found entry has its TTL refreshed.
    pub fn get_or_insert_with<R>(
        &mut self,
        key: &str,
        make: impl FnOnce() -> V,
        f: impl FnOnce(&mut V) -> R,
    ) -> R {
        if self.is_expired(key) {
            self.entries.remove(key);
        }
        if !self.entries.contains_key(key) {
            self.insert(key, make());
        }
        // Read the entry directly rather than through `with`: a zero TTL would make
        // the entry look expired the instant it was inserted and panic the expect.
        let entry = self
            .entries
            .get_mut(key)
            .expect("the entry exists after insertion");
        entry.last_seen = Instant::now();
        f(&mut entry.value)
    }

    /// Read the entry for `key` without refreshing its TTL.
    pub fn peek<R>(&self, key: &str, f: impl FnOnce(&V) -> R) -> Option<R> {
        if self.is_expired(key) {
            return None;
        }
        self.entries.get(key).map(|entry| f(&entry.value))
    }

    /// Borrow the entry for `key` without refreshing its TTL, or `None` when it is
    /// absent or already expired. For callers that need several reads in one lock,
    /// where [`peek`](Self::peek)'s closure would fight the borrow checker.
    pub fn get(&self, key: &str) -> Option<&V> {
        self.get_at(key, Instant::now())
    }

    /// [`get`](Self::get) as of `now`.
    pub fn get_at(&self, key: &str, now: Instant) -> Option<&V> {
        if self.is_expired_at(key, now) {
            return None;
        }
        self.entries.get(key).map(|entry| &entry.value)
    }

    /// Remove `key`, releasing its value. This is the reap-on-close path: a
    /// transport face calls it when its session ends.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.entries.remove(key).map(|entry| entry.value)
    }

    /// Drop every entry. Used when a process-level table is intentionally reset
    /// (tests) rather than aged out.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Remove every entry for which `pred` returns true, returning how many were
    /// removed. This is the reap-on-close path for a table keyed per value rather
    /// than per principal (a session's shaped cursors, say), where closing the
    /// session means dropping the values it owns.
    pub fn remove_where(&mut self, mut pred: impl FnMut(&str, &V) -> bool) -> usize {
        let before = self.entries.len();
        self.entries.retain(|key, entry| !pred(key, &entry.value));
        before - self.entries.len()
    }

    /// Sum `weight` over every value. Lets a caller enforce a budget that is not a
    /// simple entry count (the shaped-result cache caps total bytes this way).
    pub fn weight(&self, weight: impl Fn(&V) -> usize) -> usize {
        self.entries
            .values()
            .map(|entry| weight(&entry.value))
            .sum()
    }

    /// Remove the least recently inserted entry and return it with its key and the
    /// time it was last touched, so a caller can keep it elsewhere for its remaining
    /// TTL, or [`put_back`](Self::put_back) when it cannot.
    pub fn pop_oldest(&mut self) -> Option<Evicted<V>> {
        let key = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.seq)
            .map(|(key, _)| key.clone())?;
        let entry = self.entries.remove(&key)?;
        Some(Evicted {
            key,
            value: entry.value,
            last_seen: entry.last_seen,
            seq: entry.seq,
        })
    }

    /// Return an entry taken by [`pop_oldest`](Self::pop_oldest) to its old place:
    /// same insertion order, same TTL clock. The cap is not applied, so a caller that
    /// could not move an entry elsewhere never loses it here.
    pub fn put_back(&mut self, evicted: Evicted<V>) {
        self.entries.insert(
            evicted.key,
            Entry {
                value: evicted.value,
                last_seen: evicted.last_seen,
                seq: evicted.seq,
            },
        );
    }

    /// Drop the oldest entry by insertion order, returning whether one was removed.
    pub fn remove_oldest(&mut self) -> bool {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.seq)
            .map(|(key, _)| key.clone());
        match oldest {
            Some(key) => {
                self.entries.remove(&key);
                true
            }
            None => false,
        }
    }

    /// Drop every expired entry, returning how many were removed.
    pub fn reap_expired(&mut self) -> usize {
        self.reap_expired_at(Instant::now())
    }

    /// [`reap_expired`](Self::reap_expired) as of `now`.
    pub fn reap_expired_at(&mut self, now: Instant) -> usize {
        let ttl = self.ttl;
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.last_seen) < ttl);
        before - self.entries.len()
    }

    fn is_expired(&self, key: &str) -> bool {
        self.is_expired_at(key, Instant::now())
    }

    fn is_expired_at(&self, key: &str, now: Instant) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| now.saturating_duration_since(entry.last_seen) >= self.ttl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long enough that no test run reaches it by itself; tests that need an
    /// entry expired pass a later time in.
    fn long_ttl() -> Duration {
        Duration::from_secs(60)
    }

    #[test]
    fn an_inserted_value_is_found_and_refreshes_its_ttl() {
        let mut store = SessionStore::new(long_ttl(), 8);
        store.insert("session-a", 1);
        assert_eq!(store.len(), 1);
        assert_eq!(store.with("session-a", |value| *value), Some(1));
        // `peek` reads without refreshing.
        assert_eq!(store.peek("session-a", |value| *value), Some(1));
        assert_eq!(store.with("missing", |value| *value), None);
    }

    #[test]
    fn a_value_expires_with_its_ttl() {
        let mut store = SessionStore::new(long_ttl(), 8);
        let inserted = Instant::now();
        store.insert("session-a", 1);
        assert_eq!(
            store.with_at("session-a", inserted, |value| *value),
            Some(1)
        );
        let later = inserted + 2 * long_ttl();
        assert_eq!(store.with_at("session-a", later, |value| *value), None);
        // Reading an expired key drops it rather than leaving it for a later sweep.
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn reap_expired_reports_what_it_removed() {
        let mut store = SessionStore::new(long_ttl(), 8);
        let inserted = Instant::now();
        store.insert("a", 1);
        store.insert("b", 2);
        assert_eq!(store.reap_expired_at(inserted), 0, "nothing is expired yet");
        assert_eq!(store.reap_expired_at(inserted + 2 * long_ttl()), 2);
        assert!(store.is_empty());
    }

    #[test]
    fn a_popped_entry_put_back_is_still_the_oldest() {
        let mut store = SessionStore::new(Duration::from_secs(60), 2);
        store.insert("first", 1);
        store.insert("second", 2);
        let popped = store.pop_oldest().unwrap();
        assert_eq!((popped.key.as_str(), popped.value), ("first", 1));
        store.put_back(popped);
        assert_eq!(store.len(), 2);
        assert_eq!(store.pop_oldest().unwrap().key, "first");
    }

    #[test]
    fn the_cap_evicts_the_oldest_entry() {
        let mut store = SessionStore::new(Duration::from_secs(60), 2);
        store.insert("first", 1);
        store.insert("second", 2);
        store.insert("third", 3);
        assert_eq!(store.len(), 2);
        assert_eq!(store.peek("first", |value| *value), None, "oldest evicted");
        assert_eq!(store.peek("second", |value| *value), Some(2));
        assert_eq!(store.peek("third", |value| *value), Some(3));
    }

    #[test]
    fn get_or_insert_with_creates_once_then_reuses() {
        let mut store: SessionStore<Vec<i32>> = SessionStore::new(Duration::from_secs(60), 8);
        store.get_or_insert_with("session-a", Vec::new, |values| values.push(1));
        store.get_or_insert_with(
            "session-a",
            || panic!("must not recreate"),
            |values| values.push(2),
        );
        assert_eq!(
            store.peek("session-a", |values| values.clone()),
            Some(vec![1, 2])
        );
    }

    #[test]
    fn remove_releases_the_value() {
        let mut store = SessionStore::new(Duration::from_secs(60), 8);
        store.insert("session-a", 7);
        assert_eq!(store.remove("session-a"), Some(7));
        assert_eq!(store.remove("session-a"), None);
        assert!(store.is_empty());
    }

    #[test]
    fn get_or_insert_with_does_not_panic_with_a_zero_ttl() {
        let mut store: SessionStore<i32> = SessionStore::new(Duration::ZERO, 8);
        assert_eq!(
            store.get_or_insert_with("session-a", || 1, |value| *value),
            1
        );
    }

    #[test]
    fn a_zero_cap_still_holds_one() {
        let mut store = SessionStore::new(Duration::from_secs(60), 0);
        store.insert("a", 1);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn remove_where_drops_matching_entries() {
        let mut store = SessionStore::new(Duration::from_secs(60), 8);
        store.insert("cursor-1", ("client-a", 1));
        store.insert("cursor-2", ("client-b", 2));
        store.insert("cursor-3", ("client-a", 3));
        assert_eq!(store.remove_where(|_, value| value.0 == "client-a"), 2);
        assert_eq!(store.len(), 1);
        assert_eq!(store.peek("cursor-2", |value| value.1), Some(2));
    }

    #[test]
    fn weight_sums_values_and_remove_oldest_evicts_in_insert_order() {
        let mut store = SessionStore::new(Duration::from_secs(60), 8);
        store.insert("first", 10usize);
        store.insert("second", 20);
        assert_eq!(store.weight(|value| *value), 30);
        assert!(store.remove_oldest());
        assert_eq!(store.peek("first", |value| *value), None);
        assert_eq!(store.weight(|value| *value), 20);
        assert!(store.remove_oldest());
        assert!(
            !store.remove_oldest(),
            "an empty store has nothing to evict"
        );
    }
}
