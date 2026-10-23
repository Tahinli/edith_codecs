//! Rate control for the sequence encoder: one QP per picture from a bit model,
//! with a leaky bucket that pulls the stream back to its budget.
//!
//! This is the frame-level loop of the H.264 encoder's `rc.rs`, adapted to
//! HEVC: QPs run 0..=51, and the QP a P picture is coded at includes the
//! encoder's fixed P offset ([`crate::enc_p`]'s `P_QP_DELTA`). The model is the
//! classic one: at a fixed complexity a picture's size scales as `2^(-QP/6)`, so
//! `complexity = bits * 2^(QP/6)` measured on the last picture of the same type
//! predicts the QP that hits the next target.

use crate::enc_p::P_QP_DELTA;

/// Frame-level rate control state for one sequence.
#[derive(Debug, Clone)]
pub(crate) struct BitRateControl {
    /// Bits per picture at the configured bitrate and frame rate.
    base: f64,
    /// Virtual buffer occupancy in bits, positive when the stream is ahead of
    /// its budget.
    buffer: f64,
    /// `bits * 2^(qp / 6)` of the last coded picture, per kind (0 = I, 1 = P).
    complexity: [f64; 2],
    /// The coded QP the last picture of each kind was coded at.
    last_qp: [f64; 2],
    /// Extra share of the group of pictures an IDR may borrow, as a multiple of
    /// one picture's budget.
    i_share: f64,
    /// Pictures per group, over which that share is repaid.
    gop: f64,
    /// QP shift added to every picture, set per batch of GOPs by
    /// [`offset_after_batch`] when GOPs run in parallel and cannot see each other's bits.
    qp_offset: f64,
    /// Bits of the last P picture coded at each QP, with the P-picture count it was seen at
    /// (`None` when never seen). Content whose cost is a cliff in QP (near-black with
    /// temporal noise) breaks the `2^(QP/6)` model; these observations bound it.
    seen: [Option<(u64, u64)>; SEEN_QPS],
    /// Lowest QP the model may pick: `-QpBdOffset` (0 at 8 bits).
    qp_min: i32,
    /// P pictures recorded so far.
    p_count: u64,
}

/// A QP whose P picture cost more than this multiple of the picture target is a known
/// overspend: the next pictures do not go to that QP or below while the observation is fresh.
const CLIFF_RATIO: f64 = 1.5;
/// Share of the bucket's credit (bits saved against the budget) that a single picture may
/// spend above its cliff allowance, so saved bits can still pay for one expensive picture.
const CREDIT_SHARE: f64 = 0.8;
/// Most QP steps a P picture may drop below the previous P picture, bucket included.
const MAX_DESCENT: i32 = 4;
/// How many P pictures an observation stays valid for. Long enough to span the hunt back up
/// from a cliff (+2 QP per picture), short enough that a scene change releases the floor.
const CLIFF_AGE: u64 = 120;
/// Table slots for the QPs -12..=51 (the 10-bit range), `SEEN_OFFSET` slots below 0.
const SEEN_QPS: usize = 64;
const SEEN_OFFSET: i32 = 12;

impl BitRateControl {
    /// Control for `kbps` kilobits per second at `fps` frames per second, with an
    /// IDR every `gop` pictures.
    pub(crate) fn new(kbps: u32, fps: f64, gop: usize, bit_depth: u32) -> BitRateControl {
        let base = if kbps == 0 || fps <= 0.0 {
            1.0
        } else {
            f64::from(kbps) * 1000.0 / fps
        };
        // An IDR costs many times a predicted picture and every picture of the
        // group predicts from it, so it borrows a slice of each of them. Without
        // this the bucket saturates on the IDR alone.
        let gop = gop.max(1);
        let i_share = (0.15 * (gop - 1) as f64).clamp(0.0, 8.0);
        BitRateControl {
            base,
            buffer: 0.0,
            complexity: [0.0; 2],
            last_qp: [0.0; 2],
            i_share,
            gop: gop as f64,
            qp_offset: 0.0,
            seen: [None; SEEN_QPS],
            qp_min: -6 * (bit_depth as i32 - 8),
            p_count: 0,
        }
    }

    /// Take over the complexity model and last QPs of `prev` (the rate model at the end of
    /// the previous GOP) and the batch QP offset, with an empty bucket: this GOP's own pictures
    /// are the only debt it corrects.
    pub(crate) fn continue_from(&mut self, prev: &BitRateControl, offset: f64) {
        self.complexity = prev.complexity;
        self.last_qp = prev.last_qp;
        self.qp_offset = offset;
        self.seen = prev.seen;
        self.p_count = prev.p_count;
    }

    /// Lowest coded QP a P picture may use: one above the highest QP that recently cost more
    /// than [`CLIFF_RATIO`] times the target.
    fn cliff_floor(&self, target: f64) -> i32 {
        let mut floor = self.qp_min;
        for (q, s) in self.seen.iter().enumerate() {
            if let Some((bits, at)) = s
                && self.p_count - *at <= CLIFF_AGE
                && *bits as f64 > CLIFF_RATIO * target + CREDIT_SHARE * (-self.buffer).max(0.0)
            {
                floor = q as i32 - SEEN_OFFSET + 1;
            }
        }
        floor
    }

    /// Coded QP for the next picture. An IDR is `idr == true`; a P picture's
    /// result already includes the P offset.
    pub(crate) fn frame_qp(&self, idr: bool) -> i32 {
        let kind = usize::from(!idr);
        let target = self.frame_target(idr);
        let (mut qp, modelled) = if self.complexity[kind] > 0.0 {
            (6.0 * (self.complexity[kind] / target).log2(), true)
        } else if idr {
            (27.0, false)
        } else {
            // First P picture: start from the IDR's coded quantiser.
            (self.last_qp[0] + f64::from(P_QP_DELTA), false)
        };
        if modelled {
            // Never move far in one step; a jumpy quantiser looks worse than a
            // slightly wrong one. Asymmetric, as in the H.264 loop: a picture
            // that undershoots reaches the QP it can afford quickly, one that
            // overspends is pulled back gently.
            let (down, up) = if idr { (6.0, 6.0) } else { (4.0, 2.0) };
            qp = qp.clamp(self.last_qp[kind] - down, self.last_qp[kind] + up);
        }
        qp += self.qp_offset;
        // Leaky bucket: two QP steps of correction per four pictures of debt, so
        // a sustained overshoot is paid back within the clip.
        qp += (self.buffer / (self.base.max(1.0) * 4.0) * 2.0).clamp(-12.0, 12.0);
        let mut qp = qp.round().clamp(f64::from(self.qp_min), 51.0) as i32;
        if !idr {
            if self.complexity[kind] > 0.0 {
                // The bucket's credit must not turn into a dive: descend at most
                // `MAX_DESCENT` below the last P picture, so a cliff is met one observed step
                // at a time.
                qp = qp.max(self.last_qp[kind] as i32 - MAX_DESCENT);
            }
            qp = qp.max(self.cliff_floor(target).min(51));
        }
        qp
    }

    /// Record what a picture of kind `idr` coded at `qp` actually cost.
    pub(crate) fn update(&mut self, idr: bool, qp: i32, bits: u64) {
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
        if !idr {
            self.p_count += 1;
            self.seen[(qp.clamp(-SEEN_OFFSET, 51) + SEEN_OFFSET) as usize] =
                Some((bits as u64, self.p_count));
        }
        // Against the budget the picture was given, not the flat average, so an
        // IDR that lands on its large target does not push the bucket.
        let target = self.frame_target(idr);
        // Wide enough that the integral keeps its memory: a clamp near one GOP of
        // debt lets a sustained overshoot saturate the bucket and go uncorrected.
        self.buffer = (self.buffer + bits - target).clamp(-64.0 * self.base, 64.0 * self.base);
    }

    /// Bits the picture is aiming for.
    pub(crate) fn frame_target(&self, idr: bool) -> f64 {
        if idr {
            self.base * (1.0 + self.i_share)
        } else {
            // What each predicted picture of the group gives back to the IDR, so
            // that the group as a whole still costs `gop * base`.
            self.base * (1.0 - self.i_share / (self.gop - 1.0).max(1.0)).max(0.3)
        }
    }
}

/// Share of a batch's measured rate error (in QP steps) that the next batch's offset corrects.
const BATCH_GAIN: f64 = 0.7;

/// The QP offset for the next batch of GOPs, from the bits the last batch spent against its
/// budget. Six QP steps is a factor of two in rate, as in the picture model.
pub(crate) fn offset_after_batch(offset: f64, bits: f64, budget: f64) -> f64 {
    if bits <= 0.0 || budget <= 0.0 {
        return offset;
    }
    (offset + BATCH_GAIN * 6.0 * (bits / budget).log2()).clamp(-12.0, 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fed pictures that always cost what the model says, the QP settles and the
    /// bucket does not run away.
    #[test]
    fn converges_to_the_target() {
        let mut rc = BitRateControl::new(4000, 25.0, 25, 8);
        let complexity = 8.0e6;
        let mut qps = Vec::new();
        for f in 0..60 {
            let idr = f % 25 == 0;
            let qp = rc.frame_qp(idr);
            let bits = (complexity * (-f64::from(qp) / 6.0).exp2()) as u64;
            rc.update(idr, qp, bits);
            qps.push(qp);
        }
        // Predicted pictures only: an IDR is meant to sit lower, since its
        // budget is several times a predicted picture's.
        let tail: Vec<i32> = (40..60).filter(|f| f % 25 != 0).map(|f| qps[f]).collect();
        let spread = tail.iter().max().unwrap() - tail.iter().min().unwrap();
        assert!(spread <= 3, "quantiser still hunting: {tail:?}");
        assert!(
            rc.buffer.abs() < 64.0 * rc.base,
            "buffer ran away: {}",
            rc.buffer
        );
    }

    /// QP stays inside the HEVC range whatever the model is fed.
    #[test]
    fn qp_stays_in_range() {
        let mut rc = BitRateControl::new(1, 30.0, 8, 8);
        for f in 0..40 {
            let idr = f % 8 == 0;
            let qp = rc.frame_qp(idr);
            assert!((0..=51).contains(&qp));
            rc.update(idr, qp, 50_000_000);
        }
    }
}
