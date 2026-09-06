#!/bin/bash
for f in "$@"; do
  echo "--- $(basename $f .log)"
  grep -h "^| \(film\|screen\)" "$f" | sed 's/ [0-9.]* dB[^|]*|/ |/'
  grep -h "motion_mode" "$f" | sed 's/:.*(/ (/'
done
