#!/usr/bin/env bash
# Regenerates ec-mp3's measured constant tables.
#
# Layer III's Huffman codes and its 512-tap polyphase window are normative data
# that no formula produces. Rather than copy them out of an existing
# implementation, these scripts measure them: they write legal Layer III frames
# whose main-data bits they choose, decode them with ffmpeg, and read the answer
# back through a model of the decode chain.
#
#   learn_window.py   least-squares fits the 512 window taps to the PCM ffmpeg
#                     returns for spectra we chose (residual 1.6e-6, the f32
#                     noise floor of the decoder being measured)
#   mp3op.py          builds the spectrum -> PCM operator and its pseudo-inverse
#   learn_huffman.py  walks every code tree: a bit prefix is a codeword exactly
#                     when all continuations decode to the same pair. Checks a
#                     Kraft sum of 1, full coverage and uniqueness per table
#   learn_sfb.py      MPEG-1 long scalefactor band widths, one band attenuated
#                     at a time (these agreed with the published layouts)
#   lsf_probe.py      short-block reorder at the low sampling frequencies
#   emit_tables.py    writes huffman_tables.rs
#
# Needs ffmpeg, python3 and numpy. Output lands beside the scripts; copy
# huffman_tables.rs and window.rs into crates/ec-mp3/src/.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
python3 learn_window.py
python3 mp3op.py
python3 learn_huffman.py
python3 learn_sfb.py
python3 emit_tables.py
