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
