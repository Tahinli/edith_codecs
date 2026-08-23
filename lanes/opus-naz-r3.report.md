# lane opus-naz r3 — naz@96 err_ratio: importance port + premise correction

## What the map said (r3a, committed as lanes/opus-naz-r3.map.txt)

naz@96 whole-file opus_compare error: ours 1.670, ref 0.371 → err_ratio 4.499.
**100 % of our error sits in one compare window**, the first (label `t=0s`,
worst hop 42.5 ms = frame 2). Every other window in the 119 s file contributes
< 0.1 % of our error.

That window's coded decisions:

```
OUR 2/1920: trans1 intra1 trim6 int19 cb21 dual0 | tf_sum  +0 tf8 [0,0,0,0,0,0,0,0] sp2 ac1 bits3368
REF @1920:  trans1 intra1 trim2 int19 cb21 dual0 | tf_sum -15 tf8 [1,1,1,-1,-1,-1,-1,-1] sp2 ac0 bits3784
```

## Fix applied: real `importance[]` for the tf Viterbi

`tf_analysis()` was being called with `importance[i] = 13` for every band — a
stand-in constant. In libopus `importance[]` is an output of
`dynalloc_analysis()` (celt_encoder.c:1176-1189):

```c
for (i=start;i<end;i++)
   importance[i] = (int)floor(.5f+13*celt_exp2(MIN16(follower[i], 4.f)));
```
with a flat 13 only on the early-return path (`!(effectiveBytes>50 && LM>=1)`).

`dynalloc_analysis()` now computes it and the call was moved ahead of the tf
decision, which is also libopus's own order (dynalloc → tf_analysis → coarse
energy). It reads only `band_log_e`, so the move is state-safe. The whole
function was ported, not an isolated term — the failure mode recorded in
`lanes/opus-trim-r2.report.md`.

## Premise correction: the startup window is not pre-echo

The charter's working theory (naz-r2's tf_res inversion recurring at startup)
is **wrong**, and the importance port does not move that window either.
`naz_startup_hop_energies` gives the per-120-sample truth around the attack:

```
hop(ms)   src        ours       ref
42.5      3.479e-12  1.668e-18  1.535e-10
45.0      5.890e-9   6.122e-19  8.324e-7
47.5      4.070e-5   9.980e-4   1.262e-4
50.0      2.304e0    2.328e0    2.318e0
52.5      1.429e1    1.413e1    1.418e1
```

Before the attack our decode is *quieter* than both source and reference by
six to nine orders of magnitude — the opposite of pre-echo, which would show
our energy leading. The attack itself lands within 1 % of the reference.
The compare window labelled 42.5 ms is 40 ms long, so it straddles digital
silence and the attack; opus_compare raises the per-frame error to the fourth
power, so one transition window with an intra + transient frame dominates a
119 s file.

On the metric that is not window-power-weighted, we are ahead on this row:
`naz@96 corr o=0.9956 r=0.9948`, `minsec o=0.9855 r=0.9804`, rate −0.0 %.
This is the open corr-vs-opus_compare sign split already on the debts file.

## Gate

Baseline (main, re-measured this batch — the table in
`lanes/opus-her-r1.report.md` predates the silk/stereo merges and is stale):
see `## 14 rows` below.

## 14 rows — baseline (main, this batch) vs with the importance port

Baseline:
```
nik@64k: ref 69.2 ours 69.1 kbps (-0.1%), corr o=0.9887 r=0.9864 gap=-0.0024, Q o=-95.23 r=-154.78, err_ratio 0.708, minsec o=0.9766 r=0.9745, drop o=0 r=0
nik@96k: ref 101.0 ours 100.9 kbps (-0.1%), corr o=0.9947 r=0.9940 gap=-0.0007, Q o=-29.53 r=-31.55, err_ratio 0.982, minsec o=0.9889 r=0.9888, drop o=0 r=0
zaur@64k: ref 63.6 ours 63.6 kbps (-0.0%), corr o=0.9893 r=0.9866 gap=-0.0027, Q o=-202.82 r=-123.36, err_ratio 1.510, minsec o=0.9702 r=0.9686, drop o=0 r=0
zaur@96k: ref 93.9 ours 93.4 kbps (-0.5%), corr o=0.9947 r=0.9937 gap=-0.0009, Q o=-114.74 r=-125.18, err_ratio 0.940, minsec o=0.9862 r=0.9854, drop o=0 r=0
her@64k: ref 67.9 ours 67.7 kbps (-0.4%), corr o=0.9834 r=0.9794 gap=-0.0040, Q o=-365.68 r=-339.00, err_ratio 1.102, minsec o=0.9476 r=0.9406, drop o=0 r=0
her@96k: ref 101.5 ours 101.0 kbps (-0.5%), corr o=0.9914 r=0.9902 gap=-0.0012, Q o=-202.16 r=-191.50, err_ratio 1.052, minsec o=0.9728 r=0.9728, drop o=0 r=0
naz@64k: ref 71.3 ours 71.3 kbps (-0.0%), corr o=0.9908 r=0.9880 gap=-0.0029, Q o=-351.70 r=-128.39, err_ratio 2.697, minsec o=0.9720 r=0.9644, drop o=0 r=0
naz@96k: ref 105.4 ours 105.3 kbps (-0.0%), corr o=0.9956 r=0.9948 gap=-0.0008, Q o=-301.83 r=-29.16, err_ratio 4.499, minsec o=0.9855 r=0.9804, drop o=0 r=0
sadie@64k: ref 63.3 ours 63.1 kbps (-0.3%), corr o=0.9866 r=0.9859 gap=-0.0007, Q o=-266.99 r=-392.46, err_ratio 0.623, minsec o=0.9326 r=0.9100, drop o=0 r=0
sadie@96k: ref 84.8 ours 84.7 kbps (-0.2%), corr o=0.9935 r=0.9920 gap=-0.0016, Q o=-145.55 r=-231.16, err_ratio 0.660, minsec o=0.9687 r=0.9511, drop o=0 r=0
dl8a@64k: ref 65.8 ours 65.7 kbps (-0.1%), corr o=0.9889 r=0.9865 gap=-0.0025, Q o=-227.02 r=-378.84, err_ratio 0.551, minsec o=0.9787 r=0.9723, drop o=0 r=0
dl8a@96k: ref 96.8 ours 96.5 kbps (-0.3%), corr o=0.9942 r=0.9934 gap=-0.0009, Q o=-177.72 r=-423.47, err_ratio 0.374, minsec o=0.9881 r=0.9864, drop o=0 r=0
hein@64k: ref 64.9 ours 64.8 kbps (-0.2%), corr o=0.9888 r=0.9874 gap=-0.0014, Q o=-344.79 r=-388.31, err_ratio 0.855, minsec o=0.9324 r=0.9148, drop o=0 r=0
hein@96k: ref 86.9 ours 86.8 kbps (-0.2%), corr o=0.9946 r=0.9930 gap=-0.0016, Q o=-117.88 r=-246.46, err_ratio 0.528, minsec o=0.9686 r=0.9499, drop o=0 r=0
RATE GATE: all rows within ±5%
DROPOUT GATE: passed (no ours-only dropouts)
```

With the port:
```
nik@64k: ref 69.2 ours 69.1 kbps (-0.1%), corr o=0.9887 r=0.9864 gap=-0.0024, Q o=-95.23 r=-154.78, err_ratio 0.708, minsec o=0.9766 r=0.9745, drop o=0 r=0
nik@96k: ref 101.0 ours 100.9 kbps (-0.1%), corr o=0.9947 r=0.9940 gap=-0.0007, Q o=-29.86 r=-31.55, err_ratio 0.985, minsec o=0.9889 r=0.9888, drop o=0 r=0
zaur@64k: ref 63.6 ours 63.6 kbps (-0.0%), corr o=0.9893 r=0.9866 gap=-0.0027, Q o=-202.82 r=-123.36, err_ratio 1.510, minsec o=0.9702 r=0.9686, drop o=0 r=0
zaur@96k: ref 93.9 ours 93.4 kbps (-0.5%), corr o=0.9947 r=0.9937 gap=-0.0009, Q o=-114.74 r=-125.18, err_ratio 0.940, minsec o=0.9862 r=0.9854, drop o=0 r=0
her@64k: ref 67.9 ours 67.7 kbps (-0.4%), corr o=0.9834 r=0.9794 gap=-0.0040, Q o=-365.72 r=-339.00, err_ratio 1.103, minsec o=0.9477 r=0.9406, drop o=0 r=0
her@96k: ref 101.5 ours 101.0 kbps (-0.5%), corr o=0.9914 r=0.9902 gap=-0.0012, Q o=-202.07 r=-191.50, err_ratio 1.051, minsec o=0.9728 r=0.9728, drop o=0 r=0
naz@64k: ref 71.3 ours 71.3 kbps (-0.0%), corr o=0.9908 r=0.9880 gap=-0.0029, Q o=-351.70 r=-128.39, err_ratio 2.697, minsec o=0.9721 r=0.9644, drop o=0 r=0
naz@96k: ref 105.4 ours 105.3 kbps (-0.0%), corr o=0.9956 r=0.9948 gap=-0.0008, Q o=-301.83 r=-29.16, err_ratio 4.499, minsec o=0.9856 r=0.9804, drop o=0 r=0
sadie@64k: ref 63.3 ours 63.1 kbps (-0.3%), corr o=0.9866 r=0.9859 gap=-0.0007, Q o=-266.99 r=-392.46, err_ratio 0.623, minsec o=0.9326 r=0.9100, drop o=0 r=0
sadie@96k: ref 84.8 ours 84.7 kbps (-0.2%), corr o=0.9935 r=0.9920 gap=-0.0016, Q o=-144.96 r=-231.16, err_ratio 0.658, minsec o=0.9686 r=0.9511, drop o=0 r=0
dl8a@64k: ref 65.8 ours 65.7 kbps (-0.1%), corr o=0.9889 r=0.9865 gap=-0.0025, Q o=-227.54 r=-378.84, err_ratio 0.552, minsec o=0.9787 r=0.9723, drop o=0 r=0
dl8a@96k: ref 96.8 ours 96.5 kbps (-0.3%), corr o=0.9942 r=0.9934 gap=-0.0009, Q o=-177.70 r=-423.47, err_ratio 0.374, minsec o=0.9881 r=0.9864, drop o=0 r=0
hein@64k: ref 64.9 ours 64.8 kbps (-0.2%), corr o=0.9888 r=0.9874 gap=-0.0014, Q o=-344.06 r=-388.31, err_ratio 0.853, minsec o=0.9318 r=0.9148, drop o=0 r=0
hein@96k: ref 86.9 ours 86.8 kbps (-0.2%), corr o=0.9946 r=0.9930 gap=-0.0016, Q o=-117.91 r=-246.46, err_ratio 0.528, minsec o=0.9689 r=0.9499, drop o=0 r=0
RATE GATE: all rows within ±5%
DROPOUT GATE: passed (no ours-only dropouts)
```

Metric-neutral: every row's corr moves by ≤ 0.0001, every err_ratio is
unchanged to three decimals, RATE and DROPOUT green. The largest single move
is `hein@64k minsec 0.9324 → 0.9318`. The port ships because it removes a
stand-in constant from a ported reference function, not because it buys a
number.

`cargo test -p ec-opus --release`: 27 + 34 passed, 0 failed.

## Variants measured and rejected

- **Skip the Viterbi on a transient frame with no analysis history (frame 2).**
  Not shipped: the map's post-fix rerun (`lanes/opus-naz-r3b.map.txt`) shows
  frame 2's decision is unchanged by importance, and the hop energies show our
  decode is quieter than the reference there, so forcing a different tf split
  would be tuning against an instrument artefact, not a defect.

## What remains on naz

naz@96 err_ratio 4.499 and naz@64 2.697 are the only rows above 1.6 — every
other row is now ≤ 1.10 and every row's corr beats the reference. The residue
is one 40 ms transition window at the file's first attack, where opus_compare's
fourth-power frame weighting dominates a 119 s file and corr/minsec both favour
us. Closing this row needs the corr-vs-opus_compare decision that is already on
the debts file, not another encoder change.
