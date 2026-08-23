## HANDOFF — r2 tf_analysis port (no code written yet, all research done)

### What's done
All source analysis complete. Critical correction discovered this session: **libopus float-mode `SHL32`/`SHR32`/`PSHR32` are ALL no-ops** (`arch.h:227-231`: `#define SHR32(a,shift) (a)`, `#define SHL32(a,shift) (a)`, `#define PSHR32(a,shift) (a)`). This means:
- `norm = len2 / (1e-15 + mean)` — NOT `len2 * 2^20 / (mean/2 + eps)`
- `x2 = tmp[2i]^2 + tmp[2i+1]^2` — NOT divided by 65536
- `tf_estimate = sqrt(max(0, 0.0069 * min(163, tf_max) - 0.139))` — NOT `* 16384`
- `bias = 0.04 * max(-0.25, 0.5 - tf_estimate)` (same as before, MULT16_16_Q14 is just multiply)

All imports already present: `haar1`, `TF_SELECT`, `E_BANDS`, `NB_BANDS` all imported at `celt_enc.rs:32-38`. `hadamard_tmp` (960 floats) available as scratch.

### What remains — exact implementation

#### Step 1: Add INV_TABLE constant after line 42 (`const CACHE_BITS`)
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

#### Step 2: Replace `transient_analysis` (celt_enc.rs:1016-1082)
New signature: `fn transient_analysis(&mut self, len, c, stride) -> (bool, f32, usize)`

Algorithm (float-mode verified from `celt_encoder.c:228-408` + `arch.h:227-231`):
```
for ch in 0..c:
  copy in_buf[ch*stride..ch*stride+len] to tmp
  high-pass: y=mem0+x; mem0=mem1+y-2x; mem1=x-0.5y; tmp[i]=y
  clear tmp[0..12]
  forward pass (len2=len/2): x2=tmp[2i]^2+tmp[2i+1]^2; mean+=x2; tmp[i]=mem0+0.0625*(x2-mem0); mem0=tmp[i]
  backward pass: tmp[i]=mem0+0.125*(tmp[i]-mem0); mem0=tmp[i]; maxE=max(maxE,mem0)
  mean = sqrt(mean * maxE * 0.5 * len2)     // geometric mean
  norm = len2 / (1e-15 + mean)               // float-mode SHL32/SHR32 are no-ops!
  unmask = sum(inv_table[clamp(0..127, floor(64*norm*(tmp[i]+1e-15)))] for i in 12..len2-5 step 4)
  unmask = 64 * unmask * 4 / (6 * (len2 - 17))
  if unmask > mask_metric: tf_chan=ch; mask_metric=unmask

is_transient = mask_metric > 200
tf_max = max(0, sqrt(27*mask_metric) - 42).min(163)
tf_estimate = sqrt(max(0, 0.0069*tf_max - 0.139))    // NO *16384 — SHL32 is no-op in float
return (is_transient, tf_estimate, tf_chan)
```

#### Step 3: Add `tf_analysis` method (after tf_encode, ~line 1295)
```rust
fn tf_analysis(&mut self, len: usize, is_transient: bool, lambda: i32,
    n: usize, lm: usize, tf_estimate: f32, tf_chan: usize,
    importance: &[i32; NB_BANDS]) -> usize
```
Port from `celt_encoder.c:584-743`. Key details:
- `bias = 0.04 * (-0.25_f32).max(0.5 - tf_estimate)`
- l1_metric: `L1 = sum(|tmp[j]|); L1 *= 1.0 + lm_or_b as f32 * bias` (float-mode MAC16_32_Q15 = `c + a*b`)
- Per band: copy `self.x[tf_chan*n + (E_BANDS[i]<<lm) .. +N]` to tmp; try all tf levels via `haar1`; track best_level
- Transient -1 case: copy to tmp_1, `haar1(tmp_1, N>>lm, 1<<lm)`, l1 with B=lm+1
- `metric[i] = if transient {2*best_level} else {-2*best_level}`
- Narrow band fix: `if narrow && (metric==0 || metric==-2*lm) { metric -= 1 }`
- tf_select search: 2 Viterbi passes (sel=0,1), `selcost[sel] = min(cost0,cost1)`. tf_select=1 only if `is_transient && selcost[1]<selcost[0]`
- Final Viterbi: forward pass storing path0/path1, backward pass setting `self.tf_res[i]`
- Use `self.hadamard_tmp` split into two halves for tmp/tmp_1 (each needs max `(100-78)<<3 = 176` floats; hadamard_tmp has 960)
- Returns tf_select

#### Step 4: Wire in at lines 611-613
Replace:
```rust
self.tf_res = [0; NB_BANDS];
let tf_select = 0usize;
```
With:
```rust
let tf_select = if effective_bytes >= 15 * c as i32 {
    let lambda = (80i32).max(20480 / effective_bytes + 2);
    let mut importance = [0i32; NB_BANDS];
    for i in start..end { importance[i] = 13; }
    self.tf_analysis(end, is_transient, lambda, n, lm, tf_estimate, tf_chan, &importance)
} else {
    self.tf_res = [0; NB_BANDS];
    0usize
};
```
Note: `effEnd = end` for 48kHz fullband (mode->effEBands = 21 = NB_BANDS). No `tf_res[effEnd..end]` fill needed.

#### Step 5: Update call site at line 582
Change:
```rust
is_transient = self.transient_analysis(n + OVERLAP, c, stride);
```
To:
```rust
let (ta, tf_est, tf_ch) = self.transient_analysis(n + OVERLAP, c, stride);
is_transient = ta;
```
And add `let mut tf_estimate = 0.0f32; let mut tf_chan = 0usize;` before line 579, set them from the return.

#### Step 6: Compile + gate
```
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-opus-drop
cargo check -p ec-opus          # fix errors
SWEEP_ONLY=sadie,hein cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture
```
Keep if BOTH sadie and hein gaps shrink AND minsec doesn't drop.

#### Step 7: If kept — full sweep, cleanup, commit
```
cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture
```
Compare to `lanes/opus-drop-r2.sweep.txt` (no row corr −.001, no dropouts, rate ±5%).
Remove `sadie64_persecond_diag` from conformance.rs (~line 2862). `git checkout -- lanes/opus-gate-r1.sweep.txt`. Commit on `lane-opus-64`. Write `lanes/opus-64-r2.report.md` + `lanes/opus-64-r2.sweep.txt`.

### Key gotchas
- **Float-mode SHL32/SHR32/PSHR32 are no-ops** — verified at `arch.h:227-231`. All Q-shifts in the C code disappear in float mode.
- **importance = 0 for bands before `start`** — libopus leaves them uninitialized (stack), effectively 0. Set 13 only for `start..end`.
- **`effEnd = end`** for 48kHz fullband — no extra band fill needed.
- **Stereo `tf_chan`**: for mono always 0; for stereo, channel with highest unmask. The per-channel loop handles this.
- **`len` to tf_analysis = `end`** (absolute band count), not `end - start`. Bands before start have X=0 → metric=0 → tf_res=0.
- **`allow_weak_transients`**: libopus passes this from `st->typer`, default false. Skip it (treat as false) for first attempt.
