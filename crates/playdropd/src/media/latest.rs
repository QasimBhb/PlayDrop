//! Newest-state-per-player queue between the platform watchers and the daemon's channel.
//!
//! Platform events arrive on threads that must never wait (the zbus connection task on
//! Linux, `WinRT` callback threads on Windows), and players can fire several changes a
//! second (PLAN.md anti-pattern 10: KDE Connect relayed about 4 MPRIS signals a second and
//! drained phone batteries). So a watcher only [`Latest::push`]es: a newer state for a
//! player replaces the one still waiting, and one forwarder task moves the survivors into
//! the bounded channel at the pace the daemon reads them. The core then coalesces the wire
//! to 2 frames per second per player (protocol.md 5.9); this queue only keeps a slow reader
//! from turning into a backlog of stale states.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use playdrop_core::host::MediaState;
use tokio::sync::{mpsc, Notify};

/// Waiting states, at most one per player, in the order players first became dirty.
#[derive(Debug, Default)]
pub(super) struct Latest {
    pending: Mutex<Pending>,
    notify: Notify,
}

#[derive(Debug, Default)]
struct Pending {
    order: VecDeque<String>,
    states: HashMap<String, MediaState>,
}

impl Latest {
    /// Queues `state`, replacing the state still waiting for the same player. Never blocks.
    pub(super) fn push(&self, state: MediaState) {
        {
            let mut p = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if !p.states.contains_key(&state.player) {
                p.order.push_back(state.player.clone());
            }
            p.states.insert(state.player.clone(), state);
        }
        // `notify_one` keeps a permit when the forwarder is busy, so no push is missed.
        self.notify.notify_one();
    }

    /// Takes the oldest waiting state.
    pub(super) fn pop(&self) -> Option<MediaState> {
        let mut p = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some(player) = p.order.pop_front() {
            if let Some(s) = p.states.remove(&player) {
                return Some(s);
            }
        }
        None
    }

    /// Moves states into `tx` until the receiver is dropped. Waiting for channel capacity
    /// happens here, never in the watchers.
    pub(super) async fn forward(&self, tx: mpsc::Sender<MediaState>) {
        loop {
            while let Some(state) = self.pop() {
                if tx.send(state).await.is_err() {
                    return;
                }
            }
            tokio::select! {
                () = self.notify.notified() => {}
                () = tx.closed() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn state(player: &str, playing: bool) -> MediaState {
        MediaState {
            player: player.into(),
            name: None,
            playing,
            title: None,
            artist: None,
            album: None,
            duration_ms: None,
            position_ms: None,
            position_at_ms: None,
            volume: None,
            can: Vec::new(),
        }
    }

    #[test]
    fn newer_state_replaces_waiting_one_and_keeps_order() {
        let q = Latest::default();
        q.push(state("a", false));
        q.push(state("b", false));
        q.push(state("a", true));
        let first = q.pop().unwrap();
        assert_eq!(first.player, "a");
        assert!(first.playing, "the newest state for a wins");
        assert_eq!(q.pop().unwrap().player, "b");
        assert!(q.pop().is_none());
    }

    #[tokio::test]
    async fn forward_drains_into_channel_and_stops_when_receiver_drops() {
        let q = Arc::new(Latest::default());
        let (tx, mut rx) = mpsc::channel(1);
        let task = tokio::spawn({
            let q = Arc::clone(&q);
            async move { q.forward(tx).await }
        });
        for i in 0..50 {
            q.push(state("a", i % 2 == 0));
        }
        q.push(state("b", true));
        let mut received = 0;
        while rx.recv().await.unwrap().player != "b" {
            received += 1;
        }
        assert!(received < 50, "replaced states are not all delivered");
        drop(rx);
        q.push(state("c", true));
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("forwarder ends")
            .unwrap();
    }
}
