# lane av1422rest

Goal: ec-av1 4:2:2 (8/10/12-bit) byte-exact against libdav1d.

## Sweep (libaom 4:2:2, CDEF+LR on, cpu-used 7, 6 frames, testsrc2 for odd seeds / mandelbrot for even, seeded temporal noise)
Sizes 64x64, 72x40, 160x96, 192x120, 256x120, 330x190 x 8/10/12-bit x seeds 1-5 = 90 streams.

| state | DIFFs |
|---|---|
| before (pre rect4 / sub8 above-right / 12-bit DC fixes, per task brief) | 10-bit 64x64 all seeds; 192x120 s2; 256x120 s2,s4; 160x96 s4; 12-bit 64x64 s1,s4 |
| after = main bbb021d0 (this lane's sweep) | none: 90/90 exact (plus the pre-existing 8/10-bit cdeflr on/off cases) |

## Root causes
The three fixes already on main (32x8/8x32 chroma dc-sign votes, double-shifted 4:2:2 sub-8x8 above-right reach, exact rectangular DC division at 12-bit) cover every listed DIFF. No further decoder defect found, so no ec-av1 source change.

## Change
crates/ec-av1/tests/yuv422_dav1d.rs: adds 12-bit, seeds, testsrc2/mandelbrot variation, and the small/odd LR-on cases. Default run is a modest subset (all 64x64 plus 10-bit 2-seed 120-row cases); `EC_422_SWEEP=1` runs the full 90-stream sweep (~212 s on server c).

## Gates
clippy -D warnings clean; full ec-av1 suite (AOM dir set, EC_MEMGUARD_MAX=6G): 0 failed (818 lib tests passed, 49 ignored; yuv422 test passes in ~144 s default).
