[libaom-av1 @ 0x559d99e953c0] Value 9.000000 for parameter 'cpu-used' out of range [0 - 8]
[libaom-av1 @ 0x559d99e953c0] Error setting option cpu-used to value 9.
[vost#0:0/libaom-av1 @ 0x559d99e94d40] Error applying encoder options: Numerical result out of range
Error opening output file t.ivf.
Error opening output files: Numerical result out of range
libaom cpu-used 9: FAILED Command '['ffmpeg', '-y', '-hide_banner', '-loglevel', 'error', '-r', '24', '-i', 'filmA.y4m', '-threads', '1', '-c:v', 'libaom-av1', '-cpu-used', '9', '-b:v', '0', '-crf', '5', '-row-mt', '0', '-tiles', '1x1', '-f', 'ivf', 't.ivf']' returned non-zero exit status 222.
[libaom-av1 @ 0x55be68d713c0] Value 10.000000 for parameter 'cpu-used' out of range [0 - 8]
[libaom-av1 @ 0x55be68d713c0] Error setting option cpu-used to value 10.
[vost#0:0/libaom-av1 @ 0x55be68d70d40] Error applying encoder options: Numerical result out of range
Error opening output file t.ivf.
Error opening output files: Numerical result out of range
libaom cpu-used 10: FAILED Command '['ffmpeg', '-y', '-hide_banner', '-loglevel', 'error', '-r', '24', '-i', 'filmA.y4m', '-threads', '1', '-c:v', 'libaom-av1', '-cpu-used', '10', '-b:v', '0', '-crf', '5', '-row-mt', '0', '-tiles', '1x1', '-f', 'ivf', 't.ivf']' returned non-zero exit status 222.

| encoder preset | 4-point ladder (B / dB) | BD-rate vs rav1e speed 6 | wall, 4 points (s) | fps |
|---|---|---|---|---|
| rav1e speed 6 | 200390/46.29, 89922/44.10, 37953/40.76, 14006/36.48 | +0.0% | 26.8 | 1.79 |
| rav1e speed 8 | 201034/46.16, 89657/43.99, 37937/40.70, 14050/36.45 | +2.1% | 19.4 | 2.48 |
| rav1e speed 10 | 218498/46.07, 97345/43.85, 41333/40.53, 16567/36.29 | +16.9% | 10.2 | 4.73 |
| libaom cpu-used 6 | 509017/48.21, 146653/46.13, 70638/44.07, 41202/42.05 | -20.7% | 18.8 | 2.55 |
| libaom cpu-used 8 | 509017/48.21, 146653/46.13, 70638/44.07, 41202/42.05 | -20.7% | 19.9 | 2.42 |
| svt-av1 preset 8 | 159747/45.62, 78490/43.79, 48265/41.98, 27240/39.74 | -3.9% | 3.5 | 13.80 |
| svt-av1 preset 10 | 164568/45.21, 79122/43.22, 47737/41.41, 27093/39.35 | +10.9% | 2.3 | 20.78 |
| svt-av1 preset 12 | 164568/45.21, 79122/43.22, 47737/41.41, 27093/39.35 | +10.9% | 2.7 | 17.70 |
RC=0
