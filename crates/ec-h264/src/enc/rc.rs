//! Rate control: one QP per picture from a bit model, refined per macroblock
//! row inside each slice.
//!
//! The model is the classic one: at a fixed complexity a picture's size scales
//! as `2^(-QP/6)`, so `complexity = bits * 2^(QP/6)` measured on the last
//! picture of the same type predicts the QP that hits the next target. A
//! leaky-bucket term pulls the stream back when the model is wrong, which is
//! what makes the average bitrate hold over a clip rather than per picture.

/// Constant-quantiser mode is a bitrate of zero: nothing to control.
#[derive(Debug, Clone)]
pub(crate) struct RateControl {
    /// Bits per picture at the configured bitrate and frame rate.
    base: f64,
    /// Virtual buffer occupancy in bits, positive when the stream is ahead of
    /// its budget.
    buffer: f64,
    /// `bits * 2^(qp / 6)` of the last coded picture, per type (0 = I, 1 = P).
    complexity: [f64; 2],
    /// The QP the last picture of each type was coded at.
    last_qp: [f64; 2],
    /// Extra share of the group of pictures an IDR is allowed to borrow, as a
    /// multiple of one picture's budget.
    i_share: f64,
    /// Pictures per group, over which that share is repaid.
    gop: f64,
    fixed: Option<i32>,
}

impl RateControl {
    pub(crate) fn new(bitrate: u32, fps: f64, gop: u32, fixed_qp: Option<i32>) -> RateControl {
        let base = if bitrate == 0 || fps <= 0.0 {
            0.0
        } else {
            f64::from(bitrate) / fps
        };
        // An intra picture costs many times a predicted one and every picture
        // of the group predicts from it, so it borrows a slice of each of them
        // — without this the bucket saturates on the IDR alone and the next
        // half second is coded at a quantiser nothing justifies.
        let i_share = (0.15 * f64::from(gop.saturating_sub(1))).clamp(0.0, 8.0);
        RateControl {
            base,
            i_share,
            gop: f64::from(gop.max(1)),
            buffer: 0.0,
            complexity: [0.0; 2],
            last_qp: [26.0; 2],
            fixed: fixed_qp.or(if base == 0.0 { Some(26) } else { None }),
        }
    }

    /// QP for the next picture.
    pub(crate) fn frame_qp(&self, idr: bool) -> i32 {
        if let Some(qp) = self.fixed {
            return qp;
        }
        let kind = usize::from(!idr);
        let target = self.frame_target(idr);
        let mut qp = if self.complexity[kind] > 0.0 {
            6.0 * (self.complexity[kind] / target).log2()
        } else if idr {
            self.last_qp[1]
        } else {
            // First P picture: start from the I picture's quantiser.
            self.last_qp[0] + 2.0
        };
        // Never move far in one step; a jumpy quantiser looks worse than a
        // slightly wrong one.
        // Asymmetric: a picture that costs less than its budget should reach
        // the quantiser it can afford quickly (on static content that is where
        // all the quality comes from), while a picture that overspends is
        // pulled back gently, because a visible quantiser jump costs more than
        // a few frames of debt.
        let (down, up) = if idr { (6.0, 6.0) } else { (4.0, 2.0) };
        qp = qp.clamp(self.last_qp[kind] - down, self.last_qp[kind] + up);
        // Leaky bucket: 1.5 QP per picture of debt, capped.
        qp += (self.buffer / (self.base.max(1.0) * 4.0) * 2.0).clamp(-2.0, 2.0);
        // The quantiser floor is a measured choice, not a spec limit: below QP 10
        // this encoder spends large numbers of bits on differences a viewer
        // cannot see, and on static content it spends them re-coding pictures
        // that were already right. Asking for a bitrate that QP 10 cannot fill
        // therefore returns a smaller file rather than a wasteful one.
        qp.round().clamp(10.0, 51.0) as i32
    }

    /// Record what the picture actually cost.
    pub(crate) fn update(&mut self, idr: bool, qp: i32, bits: u64) {
        if self.fixed.is_some() {
            return;
        }
        let kind = usize::from(!idr);
        let bits = bits as f64;
        let c = bits * (f64::from(qp) / 6.0).exp2();
        self.complexity[kind] = if self.complexity[kind] == 0.0 {
            c
        } else {
            // Smoothed: one anomalous picture must not whipsaw the next.
            0.7 * self.complexity[kind] + 0.3 * c
        };
        self.last_qp[kind] = f64::from(qp);
        // The budget a picture of this type was given, not the flat average:
        // an IDR that lands on its (large) target must not push the bucket.
        let target = self.frame_target(idr);
        self.buffer = (self.buffer + bits - target).clamp(-4.0 * self.base, 8.0 * self.base);
    }

    /// Per-macroblock-row correction inside a slice: `spent` and `expected` are
    /// bits so far and the pro-rata budget.
    ///
    /// This is the *fast* loop, and it has to be: on content whose bit cost
    /// falls off a cliff with the quantiser — a screen capture, where dropping
    /// one QP step re-codes half the picture — the picture-level model can
    /// mispredict by an order of magnitude, and only a correction inside the
    /// picture keeps that from blowing the budget and whipsawing the next
    /// second of video. Asymmetric on purpose: overspending is capped hard,
    /// underspending is given back gently.
    pub(crate) fn row_delta(&self, spent: u64, expected: f64) -> i32 {
        if self.fixed.is_some() || expected <= 0.0 {
            return 0;
        }
        let ratio = (spent as f64 / expected).max(1e-3);
        let delta = 6.0 * ratio.log2();
        delta.round().clamp(-2.0, 6.0) as i32
    }

    /// Bits the picture is aiming for, for the per-row budget.
    pub(crate) fn frame_target(&self, idr: bool) -> f64 {
        if idr {
            self.base * (1.0 + self.i_share)
        } else {
            // What each predicted picture of the group gives back to the IDR,
            // so that the group as a whole still costs `gop * base`.
            self.base * (1.0 - self.i_share / (self.gop - 1.0).max(1.0)).max(0.3)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constant-bitrate run converges: fed pictures that always cost what the
    /// model says, the quantiser settles and the buffer does not run away.
    #[test]
    fn converges_to_the_target() {
        let mut rc = RateControl::new(4_000_000, 25.0, 25, None);
        // A synthetic source whose complexity is fixed.
        let complexity = 8.0e6;
        let mut qps = Vec::new();
        for f in 0..60 {
            let idr = f == 0;
            let qp = rc.frame_qp(idr);
            let bits = (complexity * (-f64::from(qp) / 6.0).exp2()) as u64;
            rc.update(idr, qp, bits);
            qps.push(qp);
        }
        let tail = &qps[40..];
        let spread = tail.iter().max().unwrap() - tail.iter().min().unwrap();
        assert!(spread <= 2, "quantiser still hunting: {tail:?}");
        assert!(
            rc.buffer.abs() < 2.0 * rc.base,
            "buffer ran away: {}",
            rc.buffer
        );
    }

    /// A bitrate of zero is constant-QP mode, and nothing moves it.
    #[test]
    fn zero_bitrate_is_constant_qp() {
        let mut rc = RateControl::new(0, 25.0, 25, Some(31));
        assert_eq!(rc.frame_qp(true), 31);
        rc.update(true, 31, 10_000_000);
        assert_eq!(rc.frame_qp(false), 31);
        assert_eq!(rc.row_delta(1_000_000, 1.0), 0);
    }
}
