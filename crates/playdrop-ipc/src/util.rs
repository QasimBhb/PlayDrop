//! Small helpers shared by the transports.

use tokio::task::JoinHandle;

/// Aborts a background task when dropped, so a dropped [`crate::server::Server`] or
/// [`crate::client::Client`] never leaves tasks holding its connection open.
#[derive(Debug)]
pub(crate) struct AbortOnDrop(JoinHandle<()>);

impl AbortOnDrop {
    pub(crate) fn new(handle: JoinHandle<()>) -> Self {
        Self(handle)
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Capacity of the per-client event channel. A UI that falls further behind than this sees a
/// lag item on its stream instead of stalling the transport.
pub(crate) const EVENT_CAPACITY: usize = 1024;
