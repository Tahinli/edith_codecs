## HANDOFF

### Status
All research complete. Zero code written. Tool cap hit before first edit. Every formula, line number, borrow structure, and C reference verified. The code below is ready to paste — only the `MULT16_16` and `QCONST32` float-mode defs were not explicitly confirmed (all other MULT/QCONST macros confirmed as identity in float mode at `arch.h:219-268`; these follow the same pattern).

### Files
- `crates/ec-opus/src/celt_enc.rs` — all edits go here
- C reference: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/audiopus_sys-0.2.2/opus/celt/celt_encoder.c` lines 228-410 (transient), 571-583 (l1_metric), 584-745 (tf_analysis)
- `arch.h` lines 210-268: float-mode macros (all identity/no-ops)

### Exact edits to make

**Edit 1: INV_TABLE constant** — after line 42 (`const CACHE_BITS: &[u8] = &crate::celt::CACHE_BITS;`), insert:
```rust
const INV_TABLE: [u8; 128] = [
    255,255,156,110, 86, 70, 59, 51, 45, 40, 37, 33, 31, 28, 26, 25,
     23, 22, 21, 20, 19, 18, 17, 16, 16, 15, 15, 14, 13, 13, 12, 12,
     12, 12, 11, 11, 11, 10, 10, 10,  9,  9,  9,  9,  9,  9,  8,  8,
      8,  8,  8,  7,  7,  7,  7,  7,  7,  6,  6,  6,  6,  6,  6,  6,
      6,  6,  6,  6,  6,  6,  6,  6,  6,  5,  5,  5,  5,  5,  5,  5,
      5,  5,  5,  5,  5,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,
      4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  3,  3,
      3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  2,
];
```

**Edit 2: l1_metric standalone fn** — insert before the `impl CeltEncoder` block (after the free functions, ~line 253 area, near `stereo_itheta`):
```rust
/// `l1_metric()` from the reference: L1 norm scaled by `(1 + LM*bias)`.
/// In float mode `MAC16_32_Q15(c, a*b) = c + a*b`, so this is `L1*(1 + LM*bias)`.
fn l1_metric(tmp: &[f32], n: usize, lm: usize, bias: f32) -> f32 {
    let mut l1 = 0.0f32;
    for i in 0..n {
        l1 += tmp[i].abs();
    }
    l1 * (1.0 + lm as f32 * bias)
}
```

**Edit 3: Replace transient_analysis** (lines 1016-1082) with:
```rust
    /// `transient_analysis()` — libopus float-mode algorithm.
    /// Returns `(is_transient, tf_estimate, tf_chan)`.
    fn transient_analysis(&mut self, len: usize, c: usize, stride: usize) -> (bool, f32, usize) {
        let tmp = &mut self.transient_tmp[..len];
        let len2 = len / 2;
        let mut mask_metric: i32 = 0;
        let mut tf_chan: usize = 0;

        for ch in 0..c {
            // Copy this channel's samples (C layout: in[i + c*len] = in_buf[ch*stride + i])
            tmp.copy_from_slice(&self.in_buf[ch * stride..ch * stride + len]);

            // High-pass filter: (1 - 2*z^-1 + z^-2) / (1 - z^-1 + .5*z^-2)
            let mut mem0 = 0.0f32;
            let mut mem1 = 0.0f32;
            for v in tmp.iter_mut() {
                let x = *v;
                let y = mem0 + x;
                mem0 = mem1 + y - 2.0 * x;
                mem1 = x - 0.5 * y;
                *v = y;
            }
            // First few samples are bad because we don't propagate the memory
            for v in tmp.iter_mut().take(12) {
                *v = 0.0;
            }

            // Forward pass: post-echo threshold (forward_decay = 0.0625)
            let mut mean = 0.0f32;
            let mut fm0 = 0.0f32;
            for i in 0..len2 {
                let x2 = tmp[2 * i] * tmp[2 * i] + tmp[2 * i + 1] * tmp[2 * i + 1];
                mean += x2;
                tmp[i] = fm0 + 0.0625 * (x2 - fm0);
                fm0 = tmp[i];
            }

            // Backward pass: pre-echo threshold (backward_decay = 0.125)
            let mut bm0 = 0.0f32;
            let mut max_e = 0.0f32;
            for i in (0..len2).rev() {
                tmp[i] = bm0 + 0.125 * (tmp[i] - bm0);
                bm0 = tmp[i];
                max_e = max_e.max(bm0);
            }

            // Geometric mean of frame energy and half the max
            mean = (mean * max_e * 0.5 * len2 as f32).sqrt();

            // Inverse of the mean energy (float mode: SHL32/SHR32 are no-ops)
            let norm = len2 as f32 / (1e-15 + mean);

            // Harmonic mean discarding unreliable boundaries (1/4th of samples)
            let mut unmask: i32 = 0;
            let mut i = 12;
            while i < len2 - 5 {
                let id = ((64.0 * norm * (tmp[i] + 1e-15)).floor() as i32).clamp(0, 127) as usize;
                unmask += INV_TABLE[id] as i32;
                i += 4;
            }
            // Normalize: 1/4th sampling, factor of 6 in inv_table
            unmask = 64 * unmask * 4 / (6 * (len2 as i32 - 17));
            if unmask > mask_metric {
                tf_chan = ch;
                mask_metric = unmask;
            }
        }

        let is_transient = mask_metric > 200;
        // tf_max and tf_estimate (float mode: all Q-shifts are no-ops)
        let tf_max = ((27.0 * mask_metric as f32).sqrt() - 42.0).max(0.0);
        let tf_estimate = (0.0069 * tf_max.min(163.0) - 0.139).max(0.0).sqrt();

        (is_transient, tf_estimate, tf_chan)
    }
```

**Edit 4: Add tf_analysis method** — insert after `tf_encode` method ends (~line 1295, after the closing `}` of tf_encode, before `// -- Band shapes --`):
```rust
    /// `tf_analysis()` — libopus Viterbi time/frequency search.
    /// Writes `self.tf_res[0..len]` and returns `tf_select`.
    #[allow(clippy::too_many_arguments)]
    fn tf_analysis(
        &mut self,
        len: usize,
        is_transient: bool,
        lambda: i32,
        n: usize,
        lm: usize,
        tf_estimate: f32,
        tf_chan: usize,
        importance: &[i32; NB_BANDS],
    ) -> usize {
        // bias = 0.04 * max(-0.25, 0.5 - tf_estimate)
        let bias = 0.04 * (0.5 - tf_estimate).max(-0.25);
        let t = usize::from(is_transient);

        let mut metric = [0i32; NB_BANDS];
        let mut path0 = [0i32; NB_BANDS];
        let mut path1 = [0i32; NB_BANDS];
        let mut tf_res = [0i32; NB_BANDS];

        // Max band width for tmp/tmp_1 scratch
        let max_band_w = ((E_BANDS[len] - E_BANDS[len - 1]) << lm).max(1);
        let (tmp_s, tmp_1_s) = self.hadamard_tmp.split_at_mut(max_band_w);
        let x = &self.x;

        for i in 0..len {
            let band_w = (E_BANDS[i + 1] - E_BANDS[i]) << lm;
            let narrow = E_BANDS[i + 1] - E_BANDS[i] == 1;
            let tmp = &mut tmp_s[..band_w];
            let tmp_1 = &mut tmp_1_s[..band_w];

            // Copy band coefficients: X[tf_chan*N0 + (eBands[i]<<LM)]
            let offset = tf_chan * n + (E_BANDS[i] << lm);
            tmp.copy_from_slice(&x[offset..offset + band_w]);

            let mut best_level = 0i32;
            let mut best_l1 = l1_metric(tmp, band_w, if is_transient { lm } else { 0 }, bias);

            // Check the -1 case for transients
            if is_transient && !narrow {
                tmp_1.copy_from_slice(tmp);
                haar1(tmp_1, band_w >> lm, 1 << lm);
                let l1 = l1_metric(tmp_1, band_w, lm + 1, bias);
                if l1 < best_l1 {
                    best_l1 = l1;
                    best_level = -1;
                }
            }

            for k in 0..(lm + usize::from(!(is_transient || narrow))) {
                let b = if is_transient { lm - k - 1 } else { k + 1 };
                haar1(tmp, band_w >> k, 1 << k);
                let l1 = l1_metric(tmp, band_w, b, bias);
                if l1 < best_l1 {
                    best_l1 = l1;
                    best_level = (k as i32) + 1;
                }
            }

            metric[i] = if is_transient { 2 * best_level } else { -2 * best_level };
            if narrow && (metric[i] == 0 || metric[i] == -2 * lm as i32) {
                metric[i] -= 1;
            }
        }

        // Search for optimal tf resolution (tf_select)
        let mut tf_select = 0usize;
        let mut selcost = [0i32; 2];
        for sel in 0..2 {
            let mut cost0 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * sel]).abs();
            let mut cost1 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * sel + 1]).abs()
                + if is_transient { 0 } else { lambda };
            for i in 1..len {
                let curr0 = cost0.min(cost1 + lambda);
                let curr1 = (cost0 + lambda).min(cost1);
                cost0 = curr0 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * sel]).abs();
                cost1 = curr1 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * sel + 1]).abs();
            }
            selcost[sel] = cost0.min(cost1);
        }
        if selcost[1] < selcost[0] && is_transient {
            tf_select = 1;
        }

        // Final Viterbi forward pass
        let mut cost0 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select]).abs();
        let mut cost1 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select + 1]).abs()
            + if is_transient { 0 } else { lambda };
        for i in 1..len {
            // curr0: best path to state 0
            let from0 = cost0;
            let from1 = cost1 + lambda;
            let (curr0, p0) = if from0 < from1 { (from0, 0) } else { (from1, 1) };
            path0[i] = p0;
            // curr1: best path to state 1 (uses OLD cost0)
            let from0 = cost0 + lambda;
            let from1 = cost1;
            let (curr1, p1) = if from0 < from1 { (from0, 0) } else { (from1, 1) };
            path1[i] = p1;

            cost0 = curr0 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select]).abs();
            cost1 = curr1 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select + 1]).abs();
        }

        // Backward pass
        tf_res[len - 1] = if cost0 < cost1 { 0 } else { 1 };
        for i in (0..len - 1).rev() {
            tf_res[i] = if tf_res[i + 1] == 1 { path1[i + 1] } else { path0[i + 1] };
        }

        self.tf_res = tf_res;
        tf_select
    }
```

**Edit 5: Update transient call site** (lines 579-583). Replace:
```rust
        let mut is_transient = false;
        let mut short_blocks = 0usize;
        if lm > 0 && enc.tell() as i32 + 3 <= total_bits {
            is_transient = self.transient_analysis(n + OVERLAP, c, stride);
```
with:
```rust
        let mut is_transient = false;
        let mut short_blocks = 0usize;
        let mut tf_estimate = 0.0f32;
        let mut tf_chan = 0usize;
        if lm > 0 && enc.tell() as i32 + 3 <= total_bits {
            let (ta, te, tc) = self.transient_analysis(n + OVERLAP, c, stride);
            is_transient = ta;
            tf_estimate = te;
            tf_chan = tc;
```

**Edit 6: Replace tf_res wiring** (lines 611-612). Replace:
```rust
        self.tf_res = [0; NB_BANDS];
        let tf_select = 0usize;
```
with:
```rust
        let tf_select = if effective_bytes >= 15 * c as i32 {
            let lambda = (80i32).max(20480 / effective_bytes + 2);
            let mut importance = [0i32; NB_BANDS];
            for i in start..end {
                importance[i] = 13;
            }
            self.tf_analysis(end, is_transient, lambda, n, lm, tf_estimate, tf_chan, &importance)
        } else {
            self.tf_res = [0; NB_BANDS];
            0usize
        };
```

### After edits
1. `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-opus-drop`
2. `cargo check -p ec-opus` — fix any borrow/type errors
3. Gate test: `SWEEP_ONLY=sadie,hein cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture`
4. Compare to baselines: sadie@64 gap +.0043 minsec .9148; hein@64 +.0020 minsec .9210 — keep only if BOTH gaps shrink and minsec holds
5. If gate passes: full 14-row sweep vs `lanes/opus-drop-r2.sweep.txt` (no row corr −.001, no dropouts, rate ±5%)
6. `git checkout -- lanes/opus-gate-r1.sweep.txt` before committing
7. If gate fails: wrap wiring in `const TF_ANALYSIS: bool = false;` gate, commit behind flag
8. Write `lanes/opus-64-r3.handoff.md`

### Key verified facts
- Float-mode macros (`arch.h:210-268`): `SHR32/SHL32/PSHR32` = identity, `MULT16_16_Q14(a,b)=a*b`, `MULT16_16_P15(a,b)=a*b`, `SROUND16=identity`, `QCONST16(x,b)=x`, `EPSILON=1e-15`, `EXTEND32=identity`, `ADD32=a+b`, `celt_sqrt=(float)sqrt(x)`
- `haar1(x: &mut [f32], n0: usize, stride: usize)` at `celt.rs:2580`
- Input layout: C uses `in[i+c*len]`, Rust uses `in_buf[ch*stride+i]` where `stride=len=n+OVERLAP` — same layout
- `effEnd = end` for CELT-only 48kHz (C line 1612), so `len=end` is correct
- `effective_bytes` is `i32`, declared line 497, never reassigned before line 611
- `tf_encode` (lines 1256-1295) already correct — no changes needed
- Borrow strategy: split `self.hadamard_tmp` once, borrow `self.x` as `&`, use local `tf_res` array, write `self.tf_res` at end
- `importance[i]=13` for `start..end`, 0 elsewhere — matches C's `else` branch in `dynalloc_analysis` (line 1157)
- `allow_weak_transients=false` — skip weak transient path entirely
- `len2-17` in unmask normalization: for 960-sample frames, `len2=480`, `len2-17=463` — safe (no div-by-zero)
