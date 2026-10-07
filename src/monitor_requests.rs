//! Bookkeeping for requests sent to the monitor worker: request ids, refresh
//! coalescing, timeouts and recovery from a stalled worker. It never talks to
//! the worker or the UI itself; callers act on what it returns.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use monbcon::BrightnessUpdate;

/// How long a started request may run before the worker counts as stalled.
const MONITOR_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum RefreshDecision {
    /// Send a refresh with this id to the worker.
    Send { request_id: u64 },
    /// A refresh is already running; one more will follow it.
    Coalesced,
    /// The worker is stalled; the refresh will run once it recovers.
    Deferred,
}

/// What to do once the last outstanding request of a stalled worker finishes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recovery {
    /// A refresh was requested while stalled.
    Refresh,
    /// Brightness changes may have piled up while stalled.
    ResumeApply,
}

pub(crate) struct MonitorRequests {
    next_request_id: u64,
    latest_refresh_id: u64,
    refresh: RefreshRequestState,
    pending: PendingWorkerRequests,
    stalled: bool,
    refresh_after_stall: bool,
}

impl MonitorRequests {
    pub(crate) fn new() -> Self {
        Self {
            next_request_id: 1,
            latest_refresh_id: 0,
            refresh: RefreshRequestState::default(),
            pending: PendingWorkerRequests::default(),
            stalled: false,
            refresh_after_stall: false,
        }
    }

    pub(crate) fn is_stalled(&self) -> bool {
        self.stalled
    }

    pub(crate) fn request_refresh(&mut self) -> RefreshDecision {
        if self.stalled {
            self.refresh_after_stall = true;
            return RefreshDecision::Deferred;
        }
        if !self.refresh.request() {
            return RefreshDecision::Coalesced;
        }
        RefreshDecision::Send {
            request_id: self.begin_refresh(),
        }
    }

    /// Call after handling the result of the latest refresh. Returns the id
    /// of the follow-up refresh to send, if one was coalesced.
    pub(crate) fn complete_refresh(&mut self) -> Option<u64> {
        if !self.refresh.complete() {
            return None;
        }
        if self.stalled {
            self.refresh = RefreshRequestState::default();
            self.refresh_after_stall = true;
            return None;
        }
        Some(self.begin_refresh())
    }

    /// Call when a refresh from [`Self::request_refresh`] or
    /// [`Self::complete_refresh`] couldn't be sent.
    pub(crate) fn cancel_refresh(&mut self) {
        self.refresh = RefreshRequestState::default();
    }

    pub(crate) fn begin_apply(&mut self) -> u64 {
        self.next_request_id()
    }

    /// Records a request that was sent to the worker.
    pub(crate) fn track(&mut self, request_id: u64, updates: Vec<BrightnessUpdate>) {
        self.pending.track(request_id, updates);
    }

    pub(crate) fn mark_started(&mut self, request_id: u64, now: Instant) {
        self.pending.mark_started(request_id, now);
    }

    /// Results of older refreshes are stale and must be ignored.
    pub(crate) fn is_latest_refresh(&self, request_id: u64) -> bool {
        request_id == self.latest_refresh_id
    }

    pub(crate) fn finish(&mut self, request_id: u64) -> Option<Recovery> {
        self.pending.finish(request_id);
        if !self.pending.is_empty() || !std::mem::take(&mut self.stalled) {
            return None;
        }
        Some(if std::mem::take(&mut self.refresh_after_stall) {
            Recovery::Refresh
        } else {
            Recovery::ResumeApply
        })
    }

    /// Returns true when a running request has just timed out, which leaves
    /// the worker stalled until its outstanding requests finish.
    pub(crate) fn check_timeout(&mut self, now: Instant) -> bool {
        let timed_out = self
            .pending
            .mark_next_timeout(now, MONITOR_OPERATION_TIMEOUT)
            .is_some();
        self.stalled |= timed_out;
        timed_out
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending.next_deadline(MONITOR_OPERATION_TIMEOUT)
    }

    /// Forgets every outstanding request after the worker was replaced and
    /// returns the brightness updates they carried.
    pub(crate) fn reset(&mut self) -> Vec<BrightnessUpdate> {
        self.refresh = RefreshRequestState::default();
        self.stalled = false;
        self.refresh_after_stall = false;
        self.pending.take_updates()
    }

    fn begin_refresh(&mut self) -> u64 {
        let request_id = self.next_request_id();
        self.latest_refresh_id = request_id;
        request_id
    }

    fn next_request_id(&mut self) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id = request_id.wrapping_add(1).max(1);
        request_id
    }
}

/// Coalesces refresh requests so at most one follow-up refresh is queued
/// while another one is in flight.
#[derive(Clone, Copy, Default)]
struct RefreshRequestState {
    in_flight: bool,
    again: bool,
}

impl RefreshRequestState {
    fn request(&mut self) -> bool {
        if self.in_flight {
            self.again = true;
            false
        } else {
            self.in_flight = true;
            true
        }
    }

    fn complete(&mut self) -> bool {
        if self.again {
            self.again = false;
            true
        } else {
            self.in_flight = false;
            false
        }
    }
}

struct PendingWorkerRequest {
    started_at: Option<Instant>,
    timed_out: bool,
    updates: Vec<BrightnessUpdate>,
}

#[derive(Default)]
struct PendingWorkerRequests {
    requests: HashMap<u64, PendingWorkerRequest>,
}

impl PendingWorkerRequests {
    fn track(&mut self, request_id: u64, updates: Vec<BrightnessUpdate>) {
        self.requests.insert(
            request_id,
            PendingWorkerRequest {
                started_at: None,
                timed_out: false,
                updates,
            },
        );
    }

    fn mark_started(&mut self, request_id: u64, started_at: Instant) {
        if let Some(request) = self.requests.get_mut(&request_id) {
            request.started_at = Some(started_at);
        }
    }

    fn finish(&mut self, request_id: u64) {
        self.requests.remove(&request_id);
    }

    fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    fn mark_next_timeout(&mut self, now: Instant, timeout: Duration) -> Option<u64> {
        let timed_out = self.requests.iter_mut().find(|(_, request)| {
            !request.timed_out
                && request
                    .started_at
                    .is_some_and(|started_at| now.saturating_duration_since(started_at) >= timeout)
        });
        timed_out.map(|(&request_id, request)| {
            request.timed_out = true;
            request_id
        })
    }

    /// When the earliest started request that hasn't timed out yet will.
    fn next_deadline(&self, timeout: Duration) -> Option<Instant> {
        self.requests
            .values()
            .filter(|request| !request.timed_out)
            .filter_map(|request| request.started_at)
            .min()
            .map(|started_at| started_at + timeout)
    }

    fn take_updates(&mut self) -> Vec<BrightnessUpdate> {
        std::mem::take(&mut self.requests)
            .into_values()
            .flat_map(|request| request.updates)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use monbcon::{BrightnessUpdate, MonitorId};

    use super::{
        MONITOR_OPERATION_TIMEOUT, MonitorRequests, PendingWorkerRequests, Recovery,
        RefreshDecision, RefreshRequestState,
    };

    fn update(id: &str) -> BrightnessUpdate {
        BrightnessUpdate {
            generation: 1,
            id: MonitorId::new(id),
            value: 50,
        }
    }

    fn sent_refresh(requests: &mut MonitorRequests) -> u64 {
        match requests.request_refresh() {
            RefreshDecision::Send { request_id } => {
                requests.track(request_id, Vec::new());
                request_id
            }
            _ => panic!("expected the refresh to be sent"),
        }
    }

    fn sent_apply(requests: &mut MonitorRequests, updates: Vec<BrightnessUpdate>) -> u64 {
        let request_id = requests.begin_apply();
        requests.track(request_id, updates);
        request_id
    }

    /// Starts a request and lets it time out, stalling the worker.
    fn stall_on(requests: &mut MonitorRequests, request_id: u64) {
        let started_at = Instant::now();
        requests.mark_started(request_id, started_at);
        assert!(!requests.check_timeout(started_at + Duration::from_secs(9)));
        assert!(requests.check_timeout(started_at + MONITOR_OPERATION_TIMEOUT));
        assert!(requests.is_stalled());
    }

    #[test]
    fn refreshes_requested_while_one_runs_coalesce_into_one_follow_up() {
        let mut requests = MonitorRequests::new();
        let first = sent_refresh(&mut requests);
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Coalesced
        ));
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Coalesced
        ));

        let follow_up = requests.complete_refresh().expect("one follow-up refresh");
        assert_ne!(follow_up, first);
        assert!(requests.is_latest_refresh(follow_up));
        assert_eq!(requests.complete_refresh(), None);
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Send { .. }
        ));
    }

    #[test]
    fn a_refresh_that_could_not_be_sent_does_not_block_the_next_one() {
        let mut requests = MonitorRequests::new();
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Send { .. }
        ));
        requests.cancel_refresh();
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Send { .. }
        ));
    }

    #[test]
    fn results_of_a_refresh_from_before_a_reset_are_stale() {
        let mut requests = MonitorRequests::new();
        let old = sent_refresh(&mut requests);
        requests.reset();
        let new = sent_refresh(&mut requests);
        assert!(!requests.is_latest_refresh(old));
        assert!(requests.is_latest_refresh(new));
    }

    #[test]
    fn a_refresh_requested_while_stalled_runs_after_recovery() {
        let mut requests = MonitorRequests::new();
        let apply = sent_apply(&mut requests, vec![update("a")]);
        stall_on(&mut requests, apply);

        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Deferred
        ));
        assert_eq!(requests.finish(apply), Some(Recovery::Refresh));
        assert!(!requests.is_stalled());
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Send { .. }
        ));
    }

    #[test]
    fn a_coalesced_refresh_is_deferred_when_the_worker_stalls() {
        let mut requests = MonitorRequests::new();
        let refresh = sent_refresh(&mut requests);
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Coalesced
        ));
        stall_on(&mut requests, refresh);

        // The late result arrives while still stalled, so the follow-up waits.
        assert_eq!(requests.complete_refresh(), None);
        assert_eq!(requests.finish(refresh), Some(Recovery::Refresh));
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Send { .. }
        ));
    }

    #[test]
    fn recovery_without_a_refresh_resumes_applying_brightness() {
        let mut requests = MonitorRequests::new();
        let apply = sent_apply(&mut requests, vec![update("a")]);
        stall_on(&mut requests, apply);
        assert_eq!(requests.finish(apply), Some(Recovery::ResumeApply));
    }

    #[test]
    fn the_worker_stays_stalled_until_every_outstanding_request_finishes() {
        let mut requests = MonitorRequests::new();
        let first = sent_apply(&mut requests, vec![update("a")]);
        let second = sent_apply(&mut requests, vec![update("b")]);
        stall_on(&mut requests, first);

        assert_eq!(requests.finish(first), None);
        assert!(requests.is_stalled());
        assert_eq!(requests.finish(second), Some(Recovery::ResumeApply));
    }

    #[test]
    fn finishing_without_a_stall_needs_no_recovery() {
        let mut requests = MonitorRequests::new();
        let apply = sent_apply(&mut requests, vec![update("a")]);
        requests.mark_started(apply, Instant::now());
        assert_eq!(requests.finish(apply), None);
    }

    #[test]
    fn reset_returns_unsent_updates_and_clears_the_stall() {
        let mut requests = MonitorRequests::new();
        let apply = sent_apply(&mut requests, vec![update("a"), update("b")]);
        stall_on(&mut requests, apply);
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Deferred
        ));

        let updates = requests.reset();
        assert_eq!(updates.len(), 2);
        assert!(!requests.is_stalled());
        assert_eq!(requests.next_deadline(), None);
        assert!(matches!(
            requests.request_refresh(),
            RefreshDecision::Send { .. }
        ));
    }

    #[test]
    fn repeated_refresh_requests_coalesce_into_one_follow_up() {
        let mut requests = RefreshRequestState::default();
        assert!(requests.request());
        assert!(!requests.request());
        assert!(!requests.request());
        assert!(requests.complete());
        assert!(!requests.complete());
        assert!(requests.request());
    }

    #[test]
    fn pending_worker_requests_time_out_only_after_the_worker_starts_them() {
        let started_at = Instant::now();
        let mut requests = PendingWorkerRequests::default();
        requests.track(7, vec![update("monitor-a")]);

        assert_eq!(
            requests.mark_next_timeout(
                started_at + Duration::from_secs(20),
                Duration::from_secs(10)
            ),
            None
        );
        requests.mark_started(7, started_at);
        assert_eq!(
            requests
                .mark_next_timeout(started_at + Duration::from_secs(9), Duration::from_secs(10)),
            None
        );
        assert_eq!(
            requests.mark_next_timeout(
                started_at + Duration::from_secs(10),
                Duration::from_secs(10)
            ),
            Some(7)
        );
        assert_eq!(
            requests.mark_next_timeout(
                started_at + Duration::from_secs(11),
                Duration::from_secs(10)
            ),
            None
        );
        let updates = requests.take_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].id.as_str(), "monitor-a");
        assert!(requests.is_empty());
    }

    #[test]
    fn next_deadline_follows_the_earliest_started_request_that_has_not_timed_out() {
        let timeout = Duration::from_secs(10);
        let started_at = Instant::now();
        let mut requests = PendingWorkerRequests::default();
        requests.track(1, Vec::new());
        requests.track(2, Vec::new());
        assert_eq!(requests.next_deadline(timeout), None);

        requests.mark_started(2, started_at + Duration::from_secs(3));
        requests.mark_started(1, started_at);
        assert_eq!(requests.next_deadline(timeout), Some(started_at + timeout));

        requests.mark_next_timeout(started_at + timeout, timeout);
        assert_eq!(
            requests.next_deadline(timeout),
            Some(started_at + Duration::from_secs(3) + timeout)
        );
    }

    #[test]
    fn finishing_an_unknown_late_request_keeps_the_tracker_consistent() {
        let mut requests = PendingWorkerRequests::default();
        requests.finish(99);
        assert!(requests.is_empty());
    }
}
