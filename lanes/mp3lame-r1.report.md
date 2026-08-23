# ec-mp3 against LAME on the real library — round 1

## Why the lane opened

`encodes_above_the_incumbent_bar` was the crate's only quality gate, and it
measures against `BAR` — a table of correlations frozen from the encoder this
crate replaces, on synthetic fixtures. A frozen number cannot notice the
reference moving, and its thinnest row (`mp3src-stereo-48000` at 128 kbit/s)
passed by +0.00014, so the gate was one rounding change away from meaningless
in either direction. The survey ranked it alongside ec-h264's missing
rate-quality reference: an unmeasured gap.

## What the gate does now

`real_library_sweep_vs_lame` encodes the first 60 s of seven of the user's own
files twice at matched constant bitrate — once with `Mp3Encoder`, once with
ffmpeg's `libmp3lame` — decodes both with the same decoder, aligns each against
the samples that went in, and correlates. Matched CBR means the sizes agree to
within 0.2%, so the correlation difference is quality and not rate.

## First measurement

| source | kbit/s | corr ours | corr LAME | gap | ours kbit/s | LAME kbit/s |
|--------|-------:|----------:|----------:|----:|------------:|------------:|
| nik   | 128 | 0.99790 | 0.99888 | -0.00098 | 127.9 | 128.1 |
| nik   | 192 | 0.99950 | 0.99982 | -0.00032 | 192.0 | 192.2 |
| zaur  | 128 | 0.99718 | 0.99855 | -0.00137 | 127.9 | 128.1 |
| zaur  | 192 | 0.99927 | 0.99976 | -0.00048 | 192.0 | 192.2 |
| her   | 128 | 0.99680 | 0.99565 | **+0.00115** | 127.9 | 128.2 |
| her   | 192 | 0.99901 | 0.99883 | **+0.00018** | 192.0 | 192.3 |
| naz   | 128 | 0.99855 | 0.99849 | **+0.00006** | 127.9 | 128.1 |
| naz   | 192 | 0.99966 | 0.99969 | -0.00003 | 192.0 | 192.2 |
| sadie | 128 | 0.99816 | 0.99897 | -0.00080 | 127.9 | 128.1 |
| sadie | 192 | 0.99951 | 0.99994 | -0.00042 | 192.0 | 192.2 |
| dl8a  | 128 | 0.99623 | 0.99737 | -0.00113 | 127.9 | 128.1 |
| dl8a  | 192 | 0.99894 | 0.99940 | -0.00046 | 192.0 | 192.2 |
| hein  | 128 | 0.99859 | 0.99958 | -0.00100 | 127.9 | 128.1 |
| hein  | 192 | 0.99964 | 0.99996 | -0.00032 | 192.0 | 192.2 |

Fourteen rows, all seven library sources readable. The floor is set at -0.0020,
just under the worst row.

## What the numbers say

The gap closes with rate on every source: the mean 128 kbit/s gap is -0.00058
and the mean 192 kbit/s gap is -0.00027. That shape — behind where bits are
scarce, level where they are not — is a bit-allocation difference, not a
transform or quantiser defect. A defect in the filterbank or the quantiser
would not politely halve itself when given more bits.

`her` is the one source where we are ahead at both rates, and it is also the
source with the widest gap in our favour. It is the sparsest material in the
list. `zaur` and `dl8a` are the two worst rows and are the densest. The
ordering is consistent with LAME's short-block switching being worth roughly a
thousandth of correlation on dense content at 128 kbit/s, and nothing at all on
sparse content — which is where the next round should look, rather than at the
psychoacoustic model.

## Not measured in this round

VBR. `encode_vbr` exists and `Mp3EncoderConfig::vbr_quality` is honoured, but
LAME's `-q`/`-V` scale does not map onto ours, so a matched-rate comparison
needs a rate-matching search before it means anything. That is its own slice.

## Round 2: the block switcher was inert, and the gate could not see it

The 128-versus-192 shape pointed at bit allocation, and the first constant on
that path is the one that decides a granule is a transient:
`next_granule_energy > here * 8.0`, on whole-granule totals. Sweeping it over
2, 4, 8, 16, 32 moved the correlation of the three worst sources by 0.00004
across the whole range, and 8 through 32 were bit-identical.

Counting block types in the coded streams explains why, and turns up something
worse than a mistuned constant:

| stream | long | start | short | stop |
|--------|-----:|------:|------:|-----:|
| ours, zaur @128  | 9193 | 1 | **1** | 1 |
| libmp3lame, same | 8988 | 58 | **88** | 58 |
| ours, her @128   | 9190 | 2 | **2** | 2 |
| libmp3lame, same | 8948 | 72 | **102** | 70 |

One short granule in 9196. A granule is 576 samples, 13 ms; requiring its
*total* energy to grow eightfold is a condition music meets about once a
minute. The switcher has been dead code since it was written.

Forcing it awake at ratio 2 produces 526 short granules and moves whole-file
correlation by 0.00004, in the wrong direction. That is the finding: **the
crate's quality gate is structurally blind to the thing short blocks exist to
fix.** Pre-echo is a few milliseconds of noise smeared backwards before an
attack; correlating over a minute divides it away. A gate like that would have
accepted the switcher being deleted outright.

### The metric the gate was missing

`worst_window_db` now reports the worst 20 ms error-to-signal ratio in each
file, for ours and for LAME, skipping windows under -60 dBFS so silence does
not produce a ratio out of nothing. Against it the constant is not flat at all:

| ratio | 1.3 | 1.6 | 2 | 3 | 4 | 8 |
|-------|----:|----:|--:|--:|--:|--:|
| dl8a @128 | -2.3 | -3.3 | -3.3 | -3.3 | -3.3 | **+1.1** |
| dl8a @192 | -6.7 | -6.7 | -6.7 | -7.8 | -7.8 | **-4.2** |

At the shipped value one 20 ms window of dl8a carries more error energy than
signal. The curve is flat from 1.6 to 4 and falls off a cliff between 4 and 8,
so the shipped 8.0 sat just past the edge of a cliff nothing could see. Below
1.6 the extra short blocks cost correlation without buying back any more of the
worst window. **4.0 lands**, at the cheap end of the flat part.

## Where the lane stands after round 2

| source | kbit/s | corr ours | corr LAME | gap | worst 20 ms ours | worst LAME |
|--------|-------:|----------:|----------:|----:|-----------------:|-----------:|
| nik   | 128 | 0.99789 | 0.99888 | -0.00099 | -11.4 | -11.2 |
| nik   | 192 | 0.99950 | 0.99982 | -0.00032 | -17.7 | -17.2 |
| zaur  | 128 | 0.99717 | 0.99855 | -0.00138 | -10.3 | -9.3 |
| zaur  | 192 | 0.99927 | 0.99976 | -0.00049 | -16.3 | -16.1 |
| her   | 128 | 0.99680 | 0.99565 | +0.00115 | -9.8 | -5.4 |
| her   | 192 | 0.99901 | 0.99883 | +0.00018 | -14.7 | -11.1 |
| naz   | 128 | 0.99855 | 0.99849 | +0.00006 | -9.3 | -6.1 |
| naz   | 192 | 0.99966 | 0.99969 | -0.00003 | -15.2 | -11.7 |
| sadie | 128 | 0.99815 | 0.99897 | -0.00082 | -1.5 | -2.0 |
| sadie | 192 | 0.99951 | 0.99994 | -0.00042 | **-3.4** | **-13.2** |
| dl8a  | 128 | 0.99625 | 0.99737 | -0.00111 | -3.3 | -6.0 |
| dl8a  | 192 | 0.99894 | 0.99940 | -0.00046 | -7.8 | -12.2 |
| hein  | 128 | 0.99858 | 0.99958 | -0.00101 | **-0.1** | **-7.6** |
| hein  | 192 | 0.99964 | 0.99996 | -0.00032 | **-5.4** | **-19.4** |

We are ahead of LAME in the worst window on all four music sources and behind
on all three where the material is speech or speech-dominated. The tell is what
happens between the two rates: on hein, LAME's worst window improves by 11.8 dB
for 50% more bits and ours improves by 5.3. A worst window that will not
improve when handed more bits is not short of bits. It is the masking model
letting a band go, or the distortion loop declining to buy it back.

## Round 3, named

Three candidates, in order of what the round-2 evidence points at:

1. The distortion loop's stop conditions -- `for _round in 0..20`, three bands
   amplified per round, and the `spent * 20 > target_bits * 19` budget break.
   The comment on that break already admits it is a corner cut against the ISO
   outer loop, and a budget break is exactly what would cap the worst window
   while bits go unspent.
2. The masking offset, `6.0 + 15.0 * (1.0 - flatness)` dB. Speech is the
   material whose flatness estimate a 1024-point FFT gets most wrong.
3. The spreading leaks, 0.0032 upward and 0.1 downward, and the absolute
   threshold's `1.0 + 400.0 * (rel - 0.35)^2` shape.

None of the three has been swept. All of them predate the only metric that can
price them.
