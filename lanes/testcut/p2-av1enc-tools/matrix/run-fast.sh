#!/bin/bash
# Fast-profile pass over heavy-killed mutants (cwd = synced repo root). usage: run-fast.sh <idx> <outdir>
M=lanes/testcut/p2-av1enc-tools/matrix
exec bash $M/mutrun.sh $M/sample.json "$1" "$2" fast
