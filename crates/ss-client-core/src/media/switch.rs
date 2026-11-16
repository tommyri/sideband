//! Gap-free rendition switching for one viewer.
//!
//! The server may send a viewer two renditions for a while: the low one it is
//! leaving and a full-rendition probe, or the full one it is leaving and the
//! low group it moves to. Each rendition is a lane holding its newest group.
//! Frames of the lane on screen go straight to the decoder. Another lane's
//! frames are kept from its keyframe on, and the viewer moves to that lane
//! once it is the better one and has caught up with the picture on screen,
//! or once the lane on screen has failed or stalled. The buffered frames are
//! then decoded in one go, so the switch shows neither a gap nor a stale
//! frame. A failed group is skipped; the next group is still accepted.
use super::{Error, Frame, GroupReader};
use std::time::{Duration, Instant};

/// Frames kept for a lane that is not on screen, about five seconds at 30 fps.
const MAX_BUFFER: usize = 150;
/// How far behind the lane on screen a better lane may be and still count
/// as caught up: one low-rendition frame and a little more (48 kHz ticks).
const CAUGHT_UP: i32 = 4800;
/// The lane on screen is stalled after this long without a frame.
const STALL: Duration = Duration::from_millis(500);

/// What the decoder should do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Decode `frame` of `rendition`; `restart` means reset that rendition's
    /// decoder first (a new group, which starts with a keyframe).
    Frame {
        /// Rendition index (0 full, 1 low).
        rendition: u8,
        /// The access unit.
        frame: Frame,
        /// Reset the decoder before this frame.
        restart: bool,
    },
    /// The group on screen failed and no other lane can take over yet; keep
    /// the last picture and ask for a keyframe.
    Lost,
}

#[derive(Default)]
struct Lane {
    group: Option<GroupReader>,
    fresh: bool,
    /// This lane was on screen and was left mid-group: its decoder still
    /// holds the references, so its buffered frames continue without a reset.
    continues: bool,
    buffered: Vec<Frame>,
    latest: Option<u32>,
    last_at: Option<Instant>,
}

impl Lane {
    fn clear(&mut self) {
        self.group = None;
        self.continues = false;
        self.buffered.clear();
    }
}

/// Chooses which rendition's groups reach the decoder.
#[derive(Default)]
pub struct Switcher {
    lanes: [Lane; 2],
    active: Option<usize>,
}

impl Switcher {
    /// Take a newly accepted group. It replaces its rendition's older group,
    /// which is reset on drop.
    pub fn offer(&mut self, group: GroupReader) {
        let lane = &mut self.lanes[usize::from(group.header.rendition())];
        lane.group = Some(group);
        lane.fresh = true;
        lane.continues = false;
        lane.buffered.clear();
    }

    /// Rendition on screen, once one has started.
    #[must_use]
    pub fn rendition(&self) -> Option<u8> {
        self.active.and_then(|lane| u8::try_from(lane).ok())
    }

    /// Wait for the next frames to decode. Cancel-safe: everything read is
    /// kept in `self` until it is returned.
    pub async fn next(&mut self) -> Vec<Step> {
        loop {
            let (index, result) = {
                let [full, low] = &mut self.lanes;
                tokio::select! {
                    result = async { full.group.as_mut()?.next_frame().await.into() }, if full.group.is_some() => (0, result),
                    result = async { low.group.as_mut()?.next_frame().await.into() }, if low.group.is_some() => (1, result),
                    else => std::future::pending().await,
                }
            };
            let result: Option<Result<Option<Frame>, Error>> = result;
            let Some(result) = result else { continue };
            if let Some(steps) = self.handle(index, result, Instant::now()) {
                return steps;
            }
        }
    }

    fn handle(
        &mut self,
        index: usize,
        result: Result<Option<Frame>, Error>,
        now: Instant,
    ) -> Option<Vec<Step>> {
        match result {
            Ok(Some(frame)) => {
                let lane = &mut self.lanes[index];
                lane.latest = Some(frame.timestamp);
                lane.last_at = Some(now);
                if self.active == Some(index) {
                    let restart = std::mem::take(&mut lane.fresh);
                    return Some(vec![step(index, frame, restart)]);
                }
                lane.fresh = false;
                if lane.buffered.len() >= MAX_BUFFER {
                    lane.clear();
                    return None;
                }
                lane.buffered.push(frame);
                self.should_switch(index, now).then(|| self.switch(index))
            }
            Ok(None) => {
                self.lanes[index].group = None;
                None
            }
            Err(_) => {
                self.lanes[index].clear();
                if self.active != Some(index) {
                    return None;
                }
                // A failed probe falls back to the lane it left, which kept flowing.
                let other = &self.lanes[1 - index];
                if other.buffered.is_empty() && !(other.continues && other.group.is_some()) {
                    Some(vec![Step::Lost])
                } else {
                    Some(self.switch(1 - index))
                }
            }
        }
    }

    fn should_switch(&self, index: usize, now: Instant) -> bool {
        let Some(active) = self.active else {
            return true;
        };
        let (on, candidate) = (&self.lanes[active], &self.lanes[index]);
        rule(
            index < active,
            on.group.is_some(),
            on.latest,
            on.last_at,
            candidate.latest,
            now,
        )
    }

    fn switch(&mut self, index: usize) -> Vec<Step> {
        if let Some(left) = self.active.filter(|&left| left != index) {
            let left = &mut self.lanes[left];
            left.buffered.clear();
            left.continues = left.group.is_some() && !left.fresh;
        }
        self.active = Some(index);
        let lane = &mut self.lanes[index];
        lane.fresh = false;
        let reset = !std::mem::take(&mut lane.continues);
        lane.buffered
            .drain(..)
            .enumerate()
            .map(|(n, frame)| step(index, frame, reset && n == 0))
            .collect()
    }
}

/// Whether a lane with buffered frames from its keyframe should go on screen.
/// `better` is a higher rendition than the one on screen.
fn rule(
    better: bool,
    on_live: bool,
    on_latest: Option<u32>,
    on_last_at: Option<Instant>,
    candidate_latest: Option<u32>,
    now: Instant,
) -> bool {
    if !on_live {
        return true;
    }
    let ahead = match (candidate_latest, on_latest) {
        (Some(c), Some(o)) => c.wrapping_sub(o).cast_signed(),
        _ => 0,
    };
    if better {
        ahead >= -CAUGHT_UP
    } else {
        ahead > 0 && on_last_at.is_none_or(|at| now.duration_since(at) > STALL)
    }
}

fn step(index: usize, frame: Frame, restart: bool) -> Step {
    Step::Frame {
        rendition: u8::try_from(index).unwrap_or(0),
        frame,
        restart,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn frame(timestamp: u32, keyframe: bool) -> Frame {
        Frame {
            timestamp,
            keyframe,
            payload: Bytes::from_static(b"x"),
        }
    }
    fn steps(steps: &[Step]) -> Vec<(u8, u32, bool)> {
        steps
            .iter()
            .map(|s| match s {
                Step::Frame {
                    rendition,
                    frame,
                    restart,
                } => (*rendition, frame.timestamp, *restart),
                Step::Lost => (9, 0, false),
            })
            .collect()
    }

    #[test]
    fn a_failed_group_falls_back_to_a_buffered_lane_or_reports_lost() {
        let now = Instant::now();
        let mut s = Switcher::default();
        s.lanes[0].fresh = true;
        let first = s.handle(0, Ok(Some(frame(0, true))), now).unwrap();
        assert_eq!(steps(&first), [(0, 0, true)]);
        let next = s.handle(0, Ok(Some(frame(1600, false))), now).unwrap();
        assert_eq!(steps(&next), [(0, 1600, false)]);
        assert_eq!(s.handle(0, Err(Error::Frame), now), Some(vec![Step::Lost]));
        // The full lane's next group plays again, from its keyframe.
        s.lanes[0].fresh = true;
        let again = s.handle(0, Ok(Some(frame(3200, true))), now).unwrap();
        assert_eq!(steps(&again), [(0, 3200, true)]);
        // With the full lane's group gone, a low group goes on screen at its
        // keyframe. (No lane here holds a real group, so liveness is `rule`'s
        // test.)
        s.lanes[0].group = None;
        let low = s.handle(1, Ok(Some(frame(3200, true))), now).unwrap();
        assert_eq!(steps(&low), [(1, 3200, true)]);
        assert_eq!(s.rendition(), Some(1));
        let more = s.handle(1, Ok(Some(frame(6400, false))), now).unwrap();
        assert_eq!(steps(&more), [(1, 6400, false)]);
    }

    #[test]
    fn falling_back_to_the_lane_left_continues_its_decoder() {
        let now = Instant::now();
        let mut s = Switcher {
            active: Some(0),
            ..Switcher::default()
        };
        // Low was left mid-group: its frames since then continue its decoder.
        s.lanes[1].continues = true;
        s.lanes[1].buffered = vec![frame(9600, false), frame(12_800, false)];
        let back = s.handle(0, Err(Error::Frame), now).unwrap();
        assert_eq!(steps(&back), [(1, 9600, false), (1, 12_800, false)]);
        assert_eq!(s.rendition(), Some(1));
        // A new low group is a fresh start again.
        s.lanes[0].fresh = true;
        s.lanes[0].buffered = vec![frame(16_000, true)];
        let up = s.switch(0);
        assert_eq!(steps(&up), [(0, 16_000, true)]);
    }

    #[test]
    fn moves_up_only_once_the_full_lane_has_caught_up() {
        let now = Instant::now();
        let low_at = Some(96_000);
        // A probe a second behind the low picture keeps buffering.
        assert!(!rule(true, true, low_at, Some(now), Some(48_000), now));
        // Within one low frame (and a little), it goes on screen.
        assert!(rule(
            true,
            true,
            low_at,
            Some(now),
            Some(96_000 - 3200),
            now
        ));
        assert!(rule(true, true, low_at, Some(now), Some(97_600), now));
        // Wrapping timestamps compare by signed distance.
        assert!(rule(
            true,
            true,
            Some(u32::MAX - 100),
            Some(now),
            Some(50),
            now
        ));
    }

    #[test]
    fn moves_down_only_when_the_lane_on_screen_stalls_or_fails() {
        let start = Instant::now();
        let full_at = Some(48_000);
        let low_newer = Some(52_800);
        let later = start + Duration::from_millis(600);
        // Full is still flowing: a low group does not replace it.
        assert!(!rule(false, true, full_at, Some(start), low_newer, start));
        // Full stalled for 600 ms and low is newer: move down.
        assert!(rule(false, true, full_at, Some(start), low_newer, later));
        // An older low frame never replaces a stalled full one.
        assert!(!rule(
            false,
            true,
            full_at,
            Some(start),
            Some(40_000),
            later
        ));
        // A failed full group: move down at once.
        assert!(rule(false, false, full_at, Some(start), low_newer, start));
    }

    #[test]
    fn a_lane_that_never_catches_up_is_dropped_once_its_buffer_is_full() {
        let now = Instant::now();
        let mut s = Switcher {
            active: Some(1),
            ..Switcher::default()
        };
        s.lanes[0].buffered = (0..MAX_BUFFER)
            .map(|n| frame(u32::try_from(n).unwrap(), n == 0))
            .collect();
        assert!(s.handle(0, Ok(Some(frame(999, false))), now).is_none());
        assert_eq!(s.lanes[0].buffered, []);
        assert_eq!(s.rendition(), Some(1));
    }
}
