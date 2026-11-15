use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::downstream::{CancelContext, CancelRegistry};

/// Cancellation owned by a script run, separate from its upstream request.
/// Stopping host work must not suppress the script's error response to a client.
#[derive(Clone, Default)]
pub struct HostCalls(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    registry: CancelRegistry,
    next: u64,
    active: HashMap<u64, CancelContext>,
    stopped: bool,
}

pub struct HostCall {
    owner: HostCalls,
    id: u64,
    pub context: CancelContext,
}

impl HostCalls {
    pub fn start(&self) -> HostCall {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next;
        state.next += 1;
        // Ids are never reused and the registry is never closed, so this begins.
        let context = state
            .registry
            .begin_client_request(id.to_string())
            .expect("a fresh host call id begins");
        if state.stopped {
            context.cancel(Some("code mode worker stopped"));
        }
        state.active.insert(id, context.clone());
        HostCall {
            owner: self.clone(),
            context,
            id,
        }
    }

    pub fn stop(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stopped = true;
        for context in state.active.values() {
            context.cancel(Some("code mode worker stopped"));
        }
    }
}

impl Drop for HostCall {
    fn drop(&mut self) {
        let mut state = self
            .owner
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active.remove(&self.id);
        self.context.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_a_run_cancels_every_host_and_prevents_late_dispatch() {
        let calls = HostCalls::default();
        let first = calls.start();
        let second = calls.start();
        assert!(!first.context.is_cancelled());
        calls.stop();
        assert!(first.context.is_cancelled());
        assert!(second.context.is_cancelled());
        assert!(calls.start().context.is_cancelled());
    }
}
