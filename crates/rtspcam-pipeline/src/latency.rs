//! How far a stream has fallen behind live, and catching up when it falls too far.
//!
//! Each picture's *lag* is its arrival at the decode thread's output against its presentation
//! time from the camera: `now - pts` is the wall-clock moment the session would have started if
//! this picture had come without delay. The earliest such moment seen is the anchor, so the
//! fastest picture so far has lag 0 and the rest show how much later they came. A steady lag
//! means a fixed latency; a rising one means latency is building up (in the camera, the
//! network, socket buffers or our own queue to the decoder).
//!
//! With a cap set, catching up has two levels (RealTimeWebCam's design):
//!
//! 1. **Skip:** while a picture is more than the cap behind, keep decoding but don't present
//!    it, until the lag is back within half the cap. A short jump forward, no freeze.
//! 2. **Resync:** if skipping lasts longer than [`MAX_CATCH_UP`], or the lag grows by another
//!    cap's worth while skipping, drop frames until the next key frame and re-anchor. This
//!    freezes the picture for up to a key-frame interval, so it's the last resort.

use std::time::{Duration, Instant};

/// How long level 1 may try before giving way to a resync.
pub(crate) const MAX_CATCH_UP: Duration = Duration::from_secs(1);

/// What to do with a decoded picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Show,
    /// Behind by more than the cap: decoded, but not presented.
    Skip,
    /// Catching up by skipping didn't work: wait for the next key frame.
    Resync,
}

/// Measures lag per picture and decides whether to present it.
#[derive(Debug)]
pub(crate) struct LatencyCap {
    cap: Option<Duration>,
    /// The earliest `now - pts` seen this session.
    anchor: Option<Instant>,
    /// Since when, and from what lag, we have been skipping.
    behind: Option<(Instant, Duration)>,
}

impl LatencyCap {
    /// `cap`: how far behind live a picture may be before catching up starts. `None` only
    /// measures.
    pub(crate) fn new(cap: Option<Duration>) -> Self {
        Self {
            cap,
            anchor: None,
            behind: None,
        }
    }

    /// Forgets the anchor: for a new session, or after a resync.
    pub(crate) fn reset(&mut self) {
        self.anchor = None;
        self.behind = None;
    }

    /// The lag of a picture with presentation time `pts` that is ready at `now`, and what to
    /// do with it.
    pub(crate) fn picture(&mut self, pts: Duration, now: Instant) -> (Duration, Verdict) {
        let Some(base) = now.checked_sub(pts) else {
            return (Duration::ZERO, Verdict::Show);
        };
        let anchor = self.anchor.map_or(base, |a| a.min(base));
        self.anchor = Some(anchor);
        let lag = base.saturating_duration_since(anchor);
        let Some(cap) = self.cap else {
            return (lag, Verdict::Show);
        };
        let verdict = match self.behind {
            None if lag > cap => {
                self.behind = Some((now, lag));
                Verdict::Skip
            }
            None => Verdict::Show,
            Some(_) if lag <= cap / 2 => {
                self.behind = None;
                Verdict::Show
            }
            Some((since, from)) if now - since > MAX_CATCH_UP || lag > from + cap => {
                self.reset();
                Verdict::Resync
            }
            Some(_) => Verdict::Skip,
        };
        (lag, verdict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Duration = Duration::from_millis(350);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Feeds pictures at 25 fps (`pts` 40 ms apart) that are ready at `start + pts + delay`.
    fn run(cap: &mut LatencyCap, start: Instant, delays: &[u64]) -> Vec<(u64, Verdict)> {
        delays
            .iter()
            .enumerate()
            .map(|(i, &delay)| {
                let pts = ms(40 * i as u64);
                let (lag, verdict) = cap.picture(pts, start + pts + ms(delay));
                (lag.as_millis() as u64, verdict)
            })
            .collect()
    }

    #[test]
    fn a_steady_delay_has_no_lag() {
        let mut cap = LatencyCap::new(Some(CAP));
        let start = Instant::now();
        let out = run(&mut cap, start, &[200; 10]);
        assert!(
            out.iter().all(|&(lag, v)| lag == 0 && v == Verdict::Show),
            "{out:?}"
        );
    }

    #[test]
    fn a_late_first_picture_is_re_anchored_to_the_fastest() {
        let mut cap = LatencyCap::new(Some(CAP));
        let start = Instant::now();
        let out = run(&mut cap, start, &[100, 0, 30, 0]);
        let lags: Vec<u64> = out.iter().map(|&(lag, _)| lag).collect();
        assert_eq!(lags, [0, 0, 30, 0]);
    }

    #[test]
    fn skips_while_behind_then_shows_again_within_half_the_cap() {
        let mut cap = LatencyCap::new(Some(CAP));
        let start = Instant::now();
        let out = run(&mut cap, start, &[0, 400, 300, 200, 175, 100]);
        let verdicts: Vec<Verdict> = out.iter().map(|&(_, v)| v).collect();
        use Verdict::*;
        assert_eq!(verdicts, [Show, Skip, Skip, Skip, Show, Show]);
    }

    #[test]
    fn resyncs_when_the_lag_keeps_growing() {
        let mut cap = LatencyCap::new(Some(CAP));
        let start = Instant::now();
        let out = run(&mut cap, start, &[0, 400, 600, 760]);
        let verdicts: Vec<Verdict> = out.iter().map(|&(_, v)| v).collect();
        use Verdict::*;
        assert_eq!(verdicts, [Show, Skip, Skip, Resync]);
        // Re-anchored: the next picture is the new reference.
        let (lag, verdict) = cap.picture(ms(400), start + ms(2000));
        assert_eq!((lag, verdict), (Duration::ZERO, Show));
    }

    #[test]
    fn resyncs_when_catching_up_takes_too_long() {
        let mut cap = LatencyCap::new(Some(CAP));
        let start = Instant::now();
        // Behind by 400 ms for 30 pictures (1.2 s): never back within half the cap.
        let mut delays = vec![0];
        delays.extend([400; 30]);
        let out = run(&mut cap, start, &delays);
        let first_resync = out.iter().position(|&(_, v)| v == Verdict::Resync);
        // Skipping starts at picture 1 (40 ms); more than a second later is picture 27.
        assert_eq!(first_resync, Some(27), "{out:?}");
    }

    #[test]
    fn without_a_cap_it_only_measures() {
        let mut cap = LatencyCap::new(None);
        let start = Instant::now();
        let out = run(&mut cap, start, &[0, 2000, 5000]);
        assert_eq!(
            out,
            [
                (0, Verdict::Show),
                (2000, Verdict::Show),
                (5000, Verdict::Show)
            ]
        );
    }
}
