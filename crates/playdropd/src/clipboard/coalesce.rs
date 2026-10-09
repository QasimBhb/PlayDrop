//! Pure coalescing and dedup for watcher reports. Time is passed in, so tests need no clock.
//!
//! Rules: report at most one clip per `min_gap`, the newest clip of a burst wins, and never
//! report a clip (text or image) whose hash equals the last one reported. The first clip after
//! a quiet period goes out at once, so a single copy is not delayed.

use std::time::{Duration, Instant};

use super::LocalClip;

/// What the caller should do next.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Poll {
    /// Report this clip now.
    Ready(LocalClip),
    /// A clip is pending; poll again at this instant (or earlier if a new clip arrives).
    Wait(Instant),
    /// Nothing pending; wait for the next clip.
    Idle,
}

pub(super) struct Coalescer {
    min_gap: Duration,
    last_sent: Option<(blake3::Hash, Instant)>,
    pending: Option<(LocalClip, blake3::Hash)>,
}

impl Coalescer {
    pub(super) fn new(min_gap: Duration) -> Self {
        Self {
            min_gap,
            last_sent: None,
            pending: None,
        }
    }

    /// Records a new clipboard state. A newer clip replaces a pending one. A clip equal to the
    /// last reported one also cancels anything pending: the clipboard is back where the peer
    /// already has it.
    pub(super) fn push(&mut self, clip: LocalClip, hash: blake3::Hash) {
        if self.last_sent.is_some_and(|(last, _)| last == hash) {
            self.pending = None;
        } else {
            self.pending = Some((clip, hash));
        }
    }

    pub(super) fn poll(&mut self, now: Instant) -> Poll {
        if self.pending.is_none() {
            return Poll::Idle;
        }
        if let Some((_, at)) = self.last_sent {
            let due = at + self.min_gap;
            if now < due {
                return Poll::Wait(due);
            }
        }
        let (clip, hash) = self.pending.take().expect("checked above");
        self.last_sent = Some((hash, now));
        Poll::Ready(clip)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{text_hash, ClipContent};
    use super::*;

    const GAP: Duration = Duration::from_millis(100);

    fn clip(text: &str) -> (LocalClip, blake3::Hash) {
        (LocalClip::text(text, false), text_hash(text))
    }

    fn push(c: &mut Coalescer, text: &str) {
        let (clip, hash) = clip(text);
        c.push(clip, hash);
    }

    fn ready_text(p: Poll) -> String {
        match p {
            Poll::Ready(LocalClip {
                content: ClipContent::Text(t),
                ..
            }) => t,
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn first_clip_goes_out_at_once() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(GAP);
        assert_eq!(c.poll(t0), Poll::Idle);
        push(&mut c, "a");
        assert_eq!(ready_text(c.poll(t0)), "a");
        assert_eq!(c.poll(t0), Poll::Idle);
    }

    #[test]
    fn burst_within_gap_sends_only_the_newest_after_the_gap() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(GAP);
        push(&mut c, "a");
        assert_eq!(ready_text(c.poll(t0)), "a");

        let t1 = t0 + Duration::from_millis(10);
        push(&mut c, "b");
        assert_eq!(c.poll(t1), Poll::Wait(t0 + GAP));
        push(&mut c, "c");
        assert_eq!(c.poll(t1 + Duration::from_millis(20)), Poll::Wait(t0 + GAP));

        assert_eq!(ready_text(c.poll(t0 + GAP)), "c");
        assert_eq!(c.poll(t0 + GAP), Poll::Idle);
    }

    #[test]
    fn never_repeats_the_last_sent_text() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(GAP);
        push(&mut c, "a");
        assert_eq!(ready_text(c.poll(t0)), "a");
        push(&mut c, "a");
        assert_eq!(c.poll(t0 + Duration::from_secs(5)), Poll::Idle);
    }

    #[test]
    fn line_ending_rewrite_counts_as_the_same_text() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(GAP);
        push(&mut c, "a\nb");
        assert_eq!(ready_text(c.poll(t0)), "a\nb");
        push(&mut c, "a\r\nb");
        assert_eq!(c.poll(t0 + Duration::from_secs(1)), Poll::Idle);
    }

    #[test]
    fn returning_to_last_sent_cancels_pending() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(GAP);
        push(&mut c, "a");
        assert_eq!(ready_text(c.poll(t0)), "a");
        push(&mut c, "b");
        push(&mut c, "a");
        assert_eq!(c.poll(t0 + Duration::from_millis(50)), Poll::Idle);
        assert_eq!(c.poll(t0 + GAP), Poll::Idle);
    }

    #[test]
    fn spaced_changes_each_go_out() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(GAP);
        for (i, text) in ["a", "b", "a", "b"].iter().enumerate() {
            let now = t0 + GAP * u32::try_from(i).unwrap();
            push(&mut c, text);
            assert_eq!(ready_text(c.poll(now)), *text);
        }
    }
}
