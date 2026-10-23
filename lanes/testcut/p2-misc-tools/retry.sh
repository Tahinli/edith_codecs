#!/usr/bin/env bash
# Usage: retry.sh <host a|b|c> <log> <remote command...>
# Pinned server run; retries on exit 75 (slot busy) only. Never uses --host auto.
host=$1; log=$2; shift 2
for i in $(seq 1 30); do
  scripts/remote-run.sh testcut-p2-misc --host "$host" -- "$@" >> "$log" 2>&1
  rc=$?
  [ "$rc" -ne 75 ] && { echo "retry-done rc=$rc" >> "$log"; exit "$rc"; }
  echo "retry-busy attempt=$i" >> "$log"
done
echo "retry-gaveup" >> "$log"; exit 75
