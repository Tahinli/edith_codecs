//! Exact rational time. No `f64` anywhere on a timing path.
//!
//! Every timestamp in the family is an integer tick count plus the [`TimeBase`]
//! that gives the tick its duration in seconds. NTSC rates are exactly
//! representable (`24000/1001`, `30000/1001`, `60000/1001`) and stay exact
//! through rescaling, which is why `23.976` can never decay into `23.0` here.

use std::cmp::Ordering;

use crate::error::{Error, Result};

/// A positive rational number of seconds per tick, stored reduced.
///
/// Also used as a *rate* (frames per second) where a rate is what is meant;
/// [`TimeBase::inverse`] converts between the two readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeBase {
    num: i64,
    den: i64,
}

const fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

impl TimeBase {
    /// Reduced `num/den`. Panics if either part is not positive — for literals;
    /// use [`TimeBase::try_new`] for values parsed out of a container.
    pub const fn new(num: i64, den: i64) -> TimeBase {
        assert!(num > 0 && den > 0, "time base parts must be positive");
        let g = gcd(num, den);
        TimeBase {
            num: num / g,
            den: den / g,
        }
    }

    /// Reduced `num/den`, or [`Error::Corrupt`] if either part is not positive.
    pub fn try_new(num: i64, den: i64) -> Result<TimeBase> {
        if num <= 0 || den <= 0 {
            return Err(Error::corrupt(format!(
                "time base {num}/{den} not positive"
            )));
        }
        Ok(TimeBase::new(num, den))
    }

    /// Tick duration of a sample rate, i.e. `1/rate` (48 kHz audio: `1/48000`).
    pub const fn from_rate(rate: u32) -> TimeBase {
        TimeBase::new(1, rate as i64)
    }

    /// Numerator of the reduced fraction.
    pub const fn num(&self) -> i64 {
        self.num
    }

    /// Denominator of the reduced fraction.
    pub const fn den(&self) -> i64 {
        self.den
    }

    /// `den/num` — a tick duration read as a rate, or a rate as a duration.
    pub const fn inverse(&self) -> TimeBase {
        TimeBase {
            num: self.den,
            den: self.num,
        }
    }

    /// Convert `ticks` from this base into `to`, saturating at [`i64`] bounds.
    ///
    /// Saturation is unreachable for real media (i64 nanoseconds is 292 years);
    /// use [`TimeBase::checked_rescale`] when the input is attacker-controlled
    /// and the difference must be observed.
    pub fn rescale(&self, ticks: i64, to: TimeBase, rounding: Rounding) -> i64 {
        match self.checked_rescale(ticks, to, rounding) {
            Some(v) => v,
            None if ticks >= 0 => i64::MAX,
            None => i64::MIN,
        }
    }

    /// Convert `ticks` from this base into `to`, or `None` on [`i64`] overflow.
    ///
    /// Exact: the whole computation runs in [`i128`] and rounds exactly once.
    pub fn checked_rescale(&self, ticks: i64, to: TimeBase, rounding: Rounding) -> Option<i64> {
        let n = ticks as i128 * self.num as i128 * to.den as i128;
        let d = self.den as i128 * to.num as i128;
        i64::try_from(rounding.div(n, d)).ok()
    }

    /// Seconds as a float. Display and logging only — never feed this back into
    /// a timestamp.
    pub fn as_secs_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Seconds per tick: 1/1.
    pub const SECONDS: TimeBase = TimeBase::new(1, 1);
    /// Milliseconds: 1/1000 (Matroska default TimestampScale territory).
    pub const MILLIS: TimeBase = TimeBase::new(1, 1_000);
    /// Microseconds: 1/1_000_000.
    pub const MICROS: TimeBase = TimeBase::new(1, 1_000_000);
    /// Nanoseconds: 1/1_000_000_000 (Matroska timestamps).
    pub const NANOS: TimeBase = TimeBase::new(1, 1_000_000_000);
    /// MPEG transport/program stream clock: 1/90000.
    pub const MPEG_TS: TimeBase = TimeBase::new(1, 90_000);
    /// NTSC film frame duration: 1001/24000 (23.976 fps, exactly).
    pub const NTSC_FILM: TimeBase = TimeBase::new(1001, 24_000);
    /// NTSC video frame duration: 1001/30000 (29.97 fps, exactly).
    pub const NTSC_VIDEO: TimeBase = TimeBase::new(1001, 30_000);
}

/// How a rescale that does not divide evenly picks its integer result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Rounding {
    /// Toward negative infinity.
    Down,
    /// Toward positive infinity.
    Up,
    /// Toward zero (truncate).
    Zero,
    /// To the nearest, halves away from zero. The default: it is what seek and
    /// container timestamp conversion want.
    #[default]
    Nearest,
}

impl Rounding {
    /// Divide `n` by a positive `d` under this rounding mode.
    fn div(self, n: i128, d: i128) -> i128 {
        debug_assert!(d > 0, "time base denominators are positive by construction");
        let q = n / d;
        let r = n % d;
        match self {
            Rounding::Zero => q,
            Rounding::Down => {
                if r < 0 {
                    q - 1
                } else {
                    q
                }
            }
            Rounding::Up => {
                if r > 0 {
                    q + 1
                } else {
                    q
                }
            }
            Rounding::Nearest => {
                if r.abs() * 2 >= d {
                    if n >= 0 { q + 1 } else { q - 1 }
                } else {
                    q
                }
            }
        }
    }
}

/// A tick count carrying its own [`TimeBase`].
///
/// Ordering and equality compare *instants*, exactly, across different bases.
#[derive(Debug, Clone, Copy)]
pub struct Timestamp {
    /// Tick count in `base`.
    pub ticks: i64,
    /// Duration of one tick.
    pub base: TimeBase,
}

impl Timestamp {
    /// A timestamp of `ticks` in `base`.
    pub const fn new(ticks: i64, base: TimeBase) -> Timestamp {
        Timestamp { ticks, base }
    }

    /// The same instant expressed in `base`, saturating at [`i64`] bounds.
    pub fn rescale(&self, base: TimeBase, rounding: Rounding) -> Timestamp {
        Timestamp::new(self.base.rescale(self.ticks, base, rounding), base)
    }

    /// The same instant expressed in `base`, or `None` on [`i64`] overflow.
    pub fn checked_rescale(&self, base: TimeBase, rounding: Rounding) -> Option<Timestamp> {
        self.base
            .checked_rescale(self.ticks, base, rounding)
            .map(|t| Timestamp::new(t, base))
    }

    /// Advance by `ticks` of this timestamp's own base.
    pub fn checked_add_ticks(&self, ticks: i64) -> Option<Timestamp> {
        self.ticks
            .checked_add(ticks)
            .map(|t| Timestamp::new(t, self.base))
    }

    /// `self - other`, in this timestamp's base, exact.
    pub fn checked_diff(&self, other: Timestamp) -> Option<i64> {
        let o = other.checked_rescale(self.base, Rounding::Nearest)?;
        self.ticks.checked_sub(o.ticks)
    }

    /// Seconds as a float. Display only.
    pub fn as_secs_f64(&self) -> f64 {
        self.ticks as f64 * self.base.as_secs_f64()
    }
}

impl PartialEq for Timestamp {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Timestamp {}

impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timestamp {
    /// Exact cross-base comparison: both sides are put over the common
    /// denominator in [`i128`] rather than converted (and rounded) first.
    fn cmp(&self, other: &Self) -> Ordering {
        let a = self.ticks as i128 * self.base.num() as i128 * other.base.den() as i128;
        let b = other.ticks as i128 * other.base.num() as i128 * self.base.den() as i128;
        a.cmp(&b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduces_and_compares_equal() {
        assert_eq!(TimeBase::new(2, 4), TimeBase::new(1, 2));
        // NTSC fractions are already reduced and must survive verbatim.
        assert_eq!(TimeBase::NTSC_FILM.num(), 1001);
        assert_eq!(TimeBase::NTSC_FILM.den(), 24_000);
        assert_eq!(TimeBase::NTSC_FILM.inverse(), TimeBase::new(24_000, 1001));
    }

    #[test]
    fn rejects_non_positive_parts() {
        assert!(TimeBase::try_new(1, 0).is_err());
        assert!(TimeBase::try_new(0, 1).is_err());
        assert!(TimeBase::try_new(-1, 24_000).is_err());
    }

    #[test]
    fn ntsc_ten_hours_zero_drift() {
        // 10 h of 24000/1001 film: frame index -> exact base and back.
        let frames = 863_136_i64; // 10 h at 23.976 fps
        let film = TimeBase::NTSC_FILM;
        let fine = TimeBase::new(1, 24_000_000);
        let ticks = film
            .checked_rescale(frames, fine, Rounding::Nearest)
            .unwrap();
        assert_eq!(ticks, frames * 1001 * 1000);
        assert_eq!(
            fine.checked_rescale(ticks, film, Rounding::Nearest)
                .unwrap(),
            frames
        );
        // Per-frame accumulation must equal the closed form: no drift.
        let mut acc = 0_i64;
        for _ in 0..frames {
            acc += 1001 * 1000;
        }
        assert_eq!(acc, ticks);
    }

    #[test]
    fn ntsc_never_becomes_integer_fps() {
        // 23.976 fps as a rate, one second of it, in the film base.
        let one_second = TimeBase::SECONDS
            .checked_rescale(1, TimeBase::NTSC_FILM, Rounding::Nearest)
            .unwrap();
        assert_eq!(one_second, 24); // 23.976 frames rounds to 24 ticks, never 23
        // A 24 fps base would be a different instant; prove the two differ.
        let film = Timestamp::new(24, TimeBase::NTSC_FILM);
        let flat = Timestamp::new(24, TimeBase::new(1, 24));
        assert!(film > flat);
    }

    #[test]
    fn rounding_modes() {
        let ms = TimeBase::MILLIS;
        let s = TimeBase::SECONDS;
        // 1500 ms -> seconds under each mode.
        assert_eq!(ms.rescale(1500, s, Rounding::Down), 1);
        assert_eq!(ms.rescale(1500, s, Rounding::Up), 2);
        assert_eq!(ms.rescale(1500, s, Rounding::Zero), 1);
        assert_eq!(ms.rescale(1500, s, Rounding::Nearest), 2);
        // Negative timestamps (B-frame dts) round symmetrically.
        assert_eq!(ms.rescale(-1500, s, Rounding::Down), -2);
        assert_eq!(ms.rescale(-1500, s, Rounding::Up), -1);
        assert_eq!(ms.rescale(-1500, s, Rounding::Zero), -1);
        assert_eq!(ms.rescale(-1500, s, Rounding::Nearest), -2);
    }

    #[test]
    fn overflow_is_reported_not_wrapped() {
        // Seconds into nanoseconds: 2^63 seconds does not fit.
        let s = TimeBase::SECONDS;
        assert_eq!(
            s.checked_rescale(i64::MAX, TimeBase::NANOS, Rounding::Nearest),
            None
        );
        assert_eq!(
            s.rescale(i64::MAX, TimeBase::NANOS, Rounding::Nearest),
            i64::MAX
        );
        assert_eq!(
            s.rescale(i64::MIN, TimeBase::NANOS, Rounding::Nearest),
            i64::MIN
        );
        // In range, nothing saturates.
        assert_eq!(
            s.checked_rescale(3, TimeBase::NANOS, Rounding::Nearest),
            Some(3_000_000_000)
        );
    }

    #[test]
    fn timestamp_math() {
        let a = Timestamp::new(90_000, TimeBase::MPEG_TS); // 1 s
        let b = Timestamp::new(1_000, TimeBase::MILLIS); // 1 s
        assert_eq!(a, b);
        assert_eq!(a.checked_diff(b), Some(0));
        assert_eq!(a.checked_add_ticks(90_000).unwrap().ticks, 180_000);
        assert_eq!(a.rescale(TimeBase::MILLIS, Rounding::Nearest).ticks, 1_000);
        assert!(Timestamp::new(0, TimeBase::MILLIS) < a);
    }
}
