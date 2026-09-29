#!/usr/bin/env bash
# Wraps a test/bin invocation in a transient user cgroup scope so a runaway
# allocation dies inside its own cgroup instead of triggering a machine-wide
# OOM kill. Used as the cargo target runner (see .cargo/config.toml) so every
# `cargo test`/`cargo run` invocation of this workspace is capped, from any
# checkout (main repo or worktree).
set -euo pipefail

if [ "${EC_NOMEMGUARD:-0}" = "1" ]; then
    echo "memguard-runner: EC_NOMEMGUARD=1, running without memory cap" >&2
    exec "$@"
fi

if ! systemd-run --user --scope --quiet -p MemoryMax=10G true >/dev/null 2>&1; then
    echo "memguard-runner: systemd-run --user --scope unavailable, running without memory cap" >&2
    exec "$@"
fi

# Explicit unique unit name: systemd-run derives `run-p<pid>.scope` when no
# --unit is given, so a scope left loaded from an earlier run (or a reused
# pid) collides with "Unit run-pNNN.scope was already loaded or has a
# fragment file" -- the wrapped binary then never starts, which reads as a
# broken tree rather than a runner collision. Measured 2026-09-29: three
# lanes hit it in one batch and their workaround was invoking the test
# binary directly, losing the memory cap.
#
# The collision is detected by PROBING, never by the wrapped command's exit
# status: `systemd-run --scope` returns the command's own status, so
# retrying on non-zero would run every failing test twice.
unit="memguard-$$-$(date +%s%N)"
# The probe must NOT reuse `$unit`: a transient scope stays loaded after it
# exits, so probing and then exec'ing under the same --unit fails with
# "unit was already loaded or has a fragment file" -- measured directly on
# 2026-09-29 (the probe's own scope was the collision). Probe under "-probe".
if ! systemd-run --user --scope --quiet --unit="${unit}-probe" -p MemoryMax=10G true >/dev/null 2>&1; then
    # Clear a stale fragment for this name (and any leftover default-named
    # scope) and probe once more before giving up on the cap.
    rm -f "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/systemd/user/${unit}.scope" \
          "${HOME}/.config/systemd/user/${unit}.scope" \
          "${HOME}"/.config/systemd/user/run-p*.scope 2>/dev/null || true
    systemctl --user daemon-reload 2>/dev/null || true
    if ! systemd-run --user --scope --quiet --unit="${unit}-r" -p MemoryMax=10G true >/dev/null 2>&1; then
        echo "memguard-runner: scope unavailable even after clearing stale fragments, running without memory cap" >&2
        exec "$@"
    fi
    exec systemd-run --user --scope --quiet --same-dir --unit="${unit}-r" \
        -p MemoryMax=10G -p MemorySwapMax=2G \
        -- "$@"
fi

exec systemd-run --user --scope --quiet --same-dir --unit="$unit" \
    -p MemoryMax=10G -p MemorySwapMax=2G \
    -- "$@"
