#!/bin/bash
# Runs one heavy-first shard on a server (cwd = synced repo root). usage: run-shard.sh <shard.idx> <outdir>
M=lanes/testcut/p2-av1enc-tools/matrix
exec bash $M/mutrun.sh $M/sample.json "$1" "$2" heavy
