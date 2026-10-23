#!/usr/bin/env bash
# Run B (T only) and run C (remaining heavy tests) per candidate, in decreasing runtime order.
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
OUT=$HOME/lanes/testcut-p2-h265-c1/out
TOOLS=$PWD/lanes/testcut/p2h265-tools
ALLHEAVY="pixel_pipeline_runs_over_conformance_streams motion_derivation_is_deterministic_over_conformance_streams cu_syntax_sweep_over_conformance_streams ffmpeg_decodes_bit_exactly_at_every_shape noisy_near_black_lands_within_five_percent conformance_pictures_match_the_sei_hash rext_conformance_pictures_match_the_sei_hash conformance_output_order_matches_ffprobe speedup_curve ctb_32_is_the_faster_tree_at_1080p intra_1080p_beats_the_incumbent_on_twelve_threads"
CANDS="pixel_pipeline_runs_over_conformance_streams motion_derivation_is_deterministic_over_conformance_streams cu_syntax_sweep_over_conformance_streams ffmpeg_decodes_bit_exactly_at_every_shape noisy_near_black_lands_within_five_percent"
PSEUDO=" pixel_pipeline_runs_over_conformance_streams cu_syntax_sweep_over_conformance_streams "
DELETED=""
expr() { local out=""; for n in "$@"; do out="${out:+$out | }test(/(^|::)$n\$/)"; done; echo "$out"; }
for T in $CANDS; do
  python3 -c 'import json,sys; sm=set(json.load(open(sys.argv[1]))); S=set(l.rstrip("\n") for l in open(sys.argv[2]) if l.strip()); print("\n".join(sorted(sm & S)))' "$OUT/sample-$T.json" "$OUT/A.S.txt" > "$OUT/$T.SA.txt"
  SN=$(wc -l < "$OUT/$T.SA.txt")
  echo "== $T S=$SN"
  if [ "$SN" -eq 0 ]; then echo "DECIDE $T K=0 S=0 -> delete"; DELETED="$DELETED $T"; continue; fi
  RES=$(python3 -c 'import re,sys; print("|".join(re.escape(l.rstrip("\n")) for l in open(sys.argv[1]) if l.strip()))' "$OUT/$T.SA.txt")
  cargo mutants -p ec-h265 --test-tool nextest --baseline skip --timeout-multiplier 3 --re "$RES" --cargo-test-arg=-E --cargo-test-arg="$(expr $T)" -o "$OUT/$T.B" > "$OUT/$T.B.log" 2>&1
  python3 "$TOOLS/outcomes.py" "$OUT/$T.B/mutants.out/outcomes.json" killed > "$OUT/$T.K.txt"
  KN=$(wc -l < "$OUT/$T.K.txt")
  echo "$T K=$KN"
  if [ "$KN" -eq 0 ]; then echo "DECIDE $T K=0 -> delete"; DELETED="$DELETED $T"; continue; fi
  if [[ "$PSEUDO" == *" $T "* ]]; then echo "DECIDE $T pseudo weak-oracle kills $KN -> keep"; continue; fi
  REM=""; for h in $ALLHEAVY; do case " $DELETED $T " in *" $h "*) ;; *) REM="$REM $h";; esac; done
  KRE=$(python3 -c 'import re,sys; print("|".join(re.escape(l.rstrip("\n")) for l in open(sys.argv[1]) if l.strip()))' "$OUT/$T.K.txt")
  cargo mutants -p ec-h265 --test-tool nextest --baseline skip --timeout-multiplier 3 --re "$KRE" --cargo-test-arg=-E --cargo-test-arg="$(expr $REM)" -o "$OUT/$T.C" > "$OUT/$T.C.log" 2>&1
  python3 "$TOOLS/outcomes.py" "$OUT/$T.C/mutants.out/outcomes.json" killed > "$OUT/$T.Kc.txt"
  comm -23 <(sort "$OUT/$T.K.txt") <(sort "$OUT/$T.Kc.txt") > "$OUT/$T.Kp.txt"
  KP=$(wc -l < "$OUT/$T.Kp.txt")
  echo "$T K'=$KP"
  if [ "$KP" -eq 0 ]; then echo "DECIDE $T K'=0 -> delete"; DELETED="$DELETED $T"; else echo "DECIDE $T K'=$KP -> keep"; fi
done
echo "DELETED:$DELETED"
