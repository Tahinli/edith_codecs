#!/bin/sh
# usage: pinned-retry.sh <host a|b|c> <lane> <cmd...>
# Runs remote-run pinned to one host (never auto). Retries the same host on exit 75 (slot busy).
host=$1; lane=$2; shift 2
n=0
while :; do
  scripts/remote-run.sh "$lane" --host "$host" -- "$@"
  rc=$?
  [ $rc -eq 75 ] || exit $rc
  n=$((n+1)); [ $n -ge 60 ] && exit 75
  sleep 30
done
