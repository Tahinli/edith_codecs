RETRACT lanes/av1422remeasure.report.md — the "18/18 byte-exact, 0 wrong samples" table was a tautology.

The per-cell counter packed our decoded samples into a buffer and then compared
that buffer against the same samples; aomdec's output was loaded, length-checked,
and never read. It reported 0 wrong on every cell, including cells that differ
from aomdec --rawvideo in 17 658 samples. The six committed 4:2:2 pins ARE
genuinely byte-exact (re-verified with a correct comparator). The twelve fresh
320x240 cells are exact. The claim that the comparison was measuring anything
is withdrawn, and the odd-geometry cells were never measured at all.

Corrected: the defect is an ordinary 4:2:0 odd-frame-HEIGHT reconstruction
defect, reachable today with no bypass. 320x232 4:2:0 decodes with 17 658 wrong
chroma samples in all 16 frames including the keyframe; 320x248 fails too;
320x240 and 320x242 pass. Present on 6d564cd6 and 11d366b1 with identical
numbers. It is present at the decoder's EC_AV1_PREFILT_DUMP (post-reconstruction,
pre-loop-filter), so deblock, CDEF and loop restoration are exonerated, and an
--enable-restoration=0 encode still fails. See lanes/av1422anom.report.md.
