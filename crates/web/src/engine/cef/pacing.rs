//! A page's frames, paced by Gate (CEF's external begin frames): one begin
//! frame per display refresh, and none while the last one's frame is still on
//! its way. Left to its own timer, a windowless browser keeps rendering
//! whether or not the GPU keeps up, and a GPU-bound page (3D) falls seconds
//! behind its input; held to one frame in flight, it drops frames instead.
//!
//! A begin frame the page has nothing new to draw for never paints, and CEF
//! doesn't say so: after a while (a few times the page's recent frame time)
//! it stops holding the next one back.

use std::time::{Duration, Instant};

/// Waited for at least, as display refreshes, before taking a begin frame
/// to have had nothing to draw.
const PATIENCE_REFRESHES: u32 = 2;
/// Waited for at most.
const MAX_PATIENCE: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct Pacing {
    /// When the begin frame still waiting for its frame was sent.
    waiting: Option<Instant>,
    /// How long begin frames took to paint, lately (smoothed).
    frame_time: Duration,
    /// The display's refresh interval (smoothed).
    refresh: Duration,
    last_refresh: Option<Instant>,
    /// Display refreshes are being followed.
    pub ticking: bool,
}

impl Pacing {
    /// A display refresh at `now`: whether to send a begin frame.
    pub(crate) fn refresh(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last_refresh.replace(now) {
            let interval = now - last;
            // Stalls (a hidden window, a busy main thread) aren't the rate.
            if interval < Duration::from_millis(50) {
                self.refresh = if self.refresh.is_zero() { interval } else { (self.refresh * 15 + interval) / 16 };
            }
        }
        if let Some(sent) = self.waiting {
            let refresh = if self.refresh.is_zero() { Duration::from_millis(16) } else { self.refresh };
            let patience = (self.frame_time * 5 / 2).clamp(refresh * PATIENCE_REFRESHES, MAX_PATIENCE);
            if now - sent < patience {
                return false;
            }
        }
        self.waiting = Some(now);
        true
    }

    /// The page painted at `now`: the next begin frame may go.
    pub(crate) fn painted(&mut self, now: Instant) {
        if let Some(sent) = self.waiting.take() {
            let took = now - sent;
            self.frame_time = if self.frame_time.is_zero() { took } else { (self.frame_time * 7 + took) / 8 };
        }
    }

    /// Display refreshes stopped being followed (the page was hidden): the
    /// next ones start afresh.
    pub(crate) fn stop(&mut self) {
        self.ticking = false;
        self.waiting = None;
        self.last_refresh = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFRESH: Duration = Duration::from_micros(16_667);

    fn refreshes(pacing: &mut Pacing, start: Instant, count: u32) -> Vec<bool> {
        (1..=count).map(|i| pacing.refresh(start + REFRESH * i)).collect()
    }

    #[test]
    fn one_frame_in_flight() {
        let mut pacing = Pacing::default();
        let start = Instant::now();
        assert!(pacing.refresh(start));
        // The frame takes 5 refreshes (a GPU-bound page): nothing more goes.
        assert_eq!(refreshes(&mut pacing, start, 1), [false]);
        pacing.painted(start + REFRESH * 5);
        assert!(pacing.refresh(start + REFRESH * 6));
        // Its frame time now holds later begin frames back that long, too.
        assert!(!pacing.refresh(start + REFRESH * 10));
    }

    #[test]
    fn a_begin_frame_with_nothing_to_draw_stops_waiting() {
        let mut pacing = Pacing::default();
        let start = Instant::now();
        // A page that paints within a refresh.
        for i in 0..20 {
            assert!(pacing.refresh(start + REFRESH * i), "refresh {i}");
            pacing.painted(start + REFRESH * i + REFRESH / 2);
        }
        // Then nothing to draw: the next begin frame goes after two refreshes.
        let idle = start + REFRESH * 20;
        assert!(pacing.refresh(idle));
        assert_eq!(refreshes(&mut pacing, idle, 2), [false, true]);
    }

    #[test]
    fn patience_has_a_limit() {
        let mut pacing = Pacing::default();
        let start = Instant::now();
        assert!(pacing.refresh(start));
        pacing.painted(start + Duration::from_secs(2));
        let next = start + Duration::from_secs(2);
        assert!(pacing.refresh(next));
        assert!(!pacing.refresh(next + Duration::from_millis(200)));
        assert!(pacing.refresh(next + MAX_PATIENCE + Duration::from_millis(1)));
    }
}
