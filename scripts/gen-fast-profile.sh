#!/usr/bin/env bash
# Print the nextest `default-filter` for the [profile.fast] block in .config/nextest.toml.
#
# Source: per-test timing TSVs from the Phase 0 baseline
# (crate, binary, test, status, exec_time_s). Every row with exec_time_s >= THRESHOLD
# is excluded. Unit tests (binary "unittests ...") match by exact name; integration
# tests match by name suffix, because they moved under tests/it/ (binary `it`).
#
# Usage: scripts/gen-fast-profile.sh [THRESHOLD] [TSV...]
#   THRESHOLD defaults to 20 (seconds). TSVs default to lanes/testcut/{av1,rest,folded}.tsv; folded.tsv lists Phase 1
#   table tests whose folded members were heavy (estimated seconds).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
threshold="${1:-20}"
shift || true
if [ "$#" -eq 0 ]; then
    set -- "$root/lanes/testcut/av1.tsv" "$root/lanes/testcut/rest.tsv" "$root/lanes/testcut/folded.tsv"
fi

awk -F'\t' -v thr="$threshold" '
    FNR == 1 { next }                       # header of each TSV
    $5 + 0 < thr { next }
    {
        crate = $1; bin = $2; name = $3
        if (crate !~ /^ec-[a-z0-9-]+$/ || name !~ /^[A-Za-z0-9_:]+$/) {
            unmapped++
            print "unmapped: " crate "\t" name >> "/dev/stderr"
            next
        }
        key = crate SUBSEP name
        if (key in seen) next
        seen[key] = 1
        if (bin ~ /^unittests/) clause = "test(=" name ")"
        else                   clause = "test(/(^|::)" name "$/)"
        if (!(crate in groups)) { order[++n] = crate; groups[crate] = clause }
        else groups[crate] = groups[crate] " | " clause
        count[crate]++
        total++
    }
    END {
        printf "not ("
        for (i = 1; i <= n; i++) {
            c = order[i]
            if (i > 1) printf " |\n  "
            printf "(package(%s) & (%s))", c, groups[c]
        }
        printf ")\n"
        for (i = 1; i <= n; i++) printf "excluded %-12s %d\n", order[i], count[order[i]] > "/dev/stderr"
        printf "excluded total %d, unmapped %d\n", total, unmapped + 0 > "/dev/stderr"
    }
' "$@"
