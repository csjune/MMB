use std::sync::Arc;

/// Wakes the UI thread after a background thread queues an event for it.
pub(crate) type Notify = Arc<dyn Fn() + Send + Sync>;

/// Calls the notifier when dropped, including while a panicking thread
/// unwinds, so the UI thread gets to see the disconnected channel. Declare it
/// before the event sender so the sender is dropped first.
pub(crate) struct NotifyOnExit(pub(crate) Notify);

impl Drop for NotifyOnExit {
    fn drop(&mut self) {
        (self.0)();
    }
}
