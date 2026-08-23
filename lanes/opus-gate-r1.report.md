# lane-opus-gate r1 — encoder gate vs libopus (verdict: GATE LANDED, FIRES on sadie@64k)

VERDICT: err_ratio range 0.866–23.640 (worst naz@96k) — REPORT-ONLY, instrument unvalidated; dropout rows 1 (sadie@64k: ours 1 s corr<0.9, min .8987; ref 0, min .9100); rate drift worst −0.5%. RATE PASS, DROPOUT FAIL.

Test: `encoder_library_gate_vs_libopus` (#[ignore]) in crates/ec-opus/tests/conformance.rs. 7 sources × {64,96} kbps, 120 s cap per source (was 600 s in charter; 14 rows took 12 min), both bitstreams decoded by ec-opus. Hard asserts: rate ±5%, no row where ours has dropout seconds and ref has none.

## Findings
- 12/14 rows our corr ≥ libopus corr at equal size; sadie@64 (+.0067) and hein@64 (+.0033) trail — same low-rate files as vorbis residual.
- Dropout class `enc-dropout-lowrate-transient`: sadie@64 only; hein@64 (.9082), her@64 (.9462) are nearest. Encoder fix out of scope for this test-only lane → debt.
- err_ratio column: Q values −29..−1056 mean the spectral error feeding the opus_compare mapping is not on opus_compare's scale (real Q is 0–100). Ratios 2–24× while time-domain corr is ≥ ref on the same rows → instrument suspect (alignment/phase-sensitive or unnormalised). NOT a gate until validated against opus_compare on one file. Previous "~2× weighted error" debt figure came from a different path; neither is validated.

## Table (lanes/opus-gate-r1.sweep.txt)
nik	64	68.9	69.2	-0.4	0.9875	0.9864	-0.0011	-530.65	-154.78	4.249	0.9758	0.9745	0	0
nik	96	100.9	101.0	-0.2	0.9944	0.9940	-0.0003	-347.70	-31.55	5.240	0.9884	0.9888	0	0
zaur	64	63.3	63.6	-0.5	0.9882	0.9866	-0.0016	-388.30	-123.36	3.165	0.9644	0.9686	0	0
zaur	96	93.7	93.9	-0.2	0.9946	0.9937	-0.0008	-266.53	-125.18	1.975	0.9855	0.9854	0	0
her	64	67.7	67.9	-0.4	0.9829	0.9794	-0.0035	-854.77	-339.00	4.842	0.9462	0.9406	0	0
her	96	101.3	101.5	-0.2	0.9914	0.9902	-0.0012	-899.19	-191.50	10.105	0.9725	0.9728	0	0
naz	64	71.2	71.3	-0.2	0.9903	0.9880	-0.0023	-1056.61	-128.39	21.263	0.9694	0.9644	0	0
naz	96	105.2	105.4	-0.2	0.9954	0.9948	-0.0006	-832.74	-29.16	23.640	0.9850	0.9804	0	0
sadie	64	63.3	63.3	-0.1	0.9793	0.9859	+0.0067	-767.51	-392.46	3.145	0.8987	0.9100	1	0
sadie	96	84.8	84.8	-0.0	0.9911	0.9920	+0.0009	-603.35	-231.16	3.674	0.9572	0.9511	0	0
dl8a	64	65.7	65.8	-0.2	0.9880	0.9865	-0.0015	-598.74	-378.84	2.032	0.9757	0.9723	0	0
dl8a	96	96.8	96.8	-0.0	0.9940	0.9934	-0.0007	-381.94	-423.47	0.866	0.9876	0.9864	0	0
hein	64	64.8	64.9	-0.0	0.9841	0.9874	+0.0033	-661.65	-388.31	2.364	0.9082	0.9148	0	0
hein	96	86.9	86.9	-0.0	0.9932	0.9930	-0.0002	-461.53	-246.46	2.211	0.9600	0.9499	0	0
