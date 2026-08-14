//! Time, timestamps and the rational base between them.

/// A media timestamp, in [`TimeBase`] ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// A timestamp of `ticks`.
    pub fn new(ticks: i64) -> Timestamp {
        Timestamp(ticks)
    }

    /// The tick count.
    pub fn value(&self) -> i64 {
        self.0
    }
}

/// A duration in [`TimeBase`] ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Duration(pub u64);

impl Duration {
    /// A duration of `ticks`.
    pub fn new(ticks: u64) -> Duration {
        Duration(ticks)
    }

    /// The tick count.
    pub fn value(&self) -> u64 {
        self.0
    }
}

/// Wall-clock time, split so that no fraction is lost to a float.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Time {
    /// Whole seconds.
    pub seconds: u64,
    /// Fraction of a second, 0.0..1.0.
    pub frac: f64,
}

impl Time {
    /// A time from whole seconds and a fraction.
    pub fn new(seconds: u64, frac: f64) -> Time {
        Time { seconds, frac }
    }

    /// A time from seconds, or [`None`] for a negative or non-finite input —
    /// which is what makes a caller check before seeking to it.
    pub fn try_from_secs_f64(secs: f64) -> Option<Time> {
        if !secs.is_finite() || secs < 0.0 {
            return None;
        }
        let seconds = secs.trunc() as u64;
        Some(Time {
            seconds,
            frac: secs - secs.trunc(),
        })
    }

    /// This time in seconds.
    pub fn as_secs_f64(&self) -> f64 {
        self.seconds as f64 + self.frac
    }
}

/// A rational tick duration: `numer / denom` seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeBase {
    /// Numerator, in seconds.
    pub numer: u32,
    /// Denominator.
    pub denom: u32,
}

impl TimeBase {
    /// A time base of `numer / denom` seconds per tick.
    pub fn new(numer: u32, denom: u32) -> TimeBase {
        TimeBase { numer, denom }
    }

    /// `ts` as wall-clock time; a base with a zero denominator answers zero
    /// rather than dividing by it.
    pub fn calc_time_saturating(&self, ts: Timestamp) -> Time {
        if self.denom == 0 {
            return Time::default();
        }
        let secs = ts.0 as f64 * f64::from(self.numer) / f64::from(self.denom);
        Time::try_from_secs_f64(secs).unwrap_or_default()
    }

    /// The timestamp `time` falls on in this base.
    pub fn calc_timestamp(&self, time: Time) -> Timestamp {
        if self.numer == 0 {
            return Timestamp::default();
        }
        Timestamp((time.as_secs_f64() * f64::from(self.denom) / f64::from(self.numer)) as i64)
    }
}

impl From<ec_core::TimeBase> for TimeBase {
    fn from(tb: ec_core::TimeBase) -> TimeBase {
        TimeBase::new(tb.num().max(0) as u32, tb.den().max(1) as u32)
    }
}

impl From<TimeBase> for ec_core::TimeBase {
    fn from(tb: TimeBase) -> ec_core::TimeBase {
        ec_core::TimeBase::new(i64::from(tb.numer.max(1)), i64::from(tb.denom.max(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_round_trips_through_its_base() {
        let base = TimeBase::new(1, 48_000);
        let time = base.calc_time_saturating(Timestamp::new(96_000));
        assert_eq!(time.as_secs_f64(), 2.0);
        assert_eq!(base.calc_timestamp(time), Timestamp::new(96_000));
        // A negative instant is not a time, and says so rather than wrapping.
        assert_eq!(Time::try_from_secs_f64(-1.0), None);
        assert_eq!(
            TimeBase::new(1, 0)
                .calc_time_saturating(Timestamp::new(5))
                .as_secs_f64(),
            0.0
        );
    }
}
