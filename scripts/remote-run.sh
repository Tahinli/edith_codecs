#!/usr/bin/env bash
# remote-run.sh: run heavy cargo commands on the lane servers instead of this machine.
#
# Usage:
#   scripts/remote-run.sh <lane> [--host auto|a|b|c] [--with-edith] [--clean] -- <command...>
#   scripts/remote-run.sh <lane> --clean
#
#   --host auto|a|b|c  auto (default) probes a, b and c each round (one ssh each: busy slots and
#                    MemAvailable) and tries the least loaded first: fewest busy slots, then most
#                    MemAvailable. Unreachable hosts are skipped. It retries for up to 10 minutes
#                    (20 s between rounds), then builds locally.
#   --with-edith     also sync the edith tree ($EDITH_TREE, default
#                    $HOME/Documents/Code/Rust/edith) to ~/lanes/<lane>/edith and run
#                    the command inside it. Its path deps ../../../edith_codecs resolve to
#                    ~/lanes/<lane>/edith_codecs.
#   --clean          remove ~/lanes/<lane>, ~/.cache/tgt-<lane> and ~/tmp/<lane> on every host,
#                    and ~/.cache/tgt-<lane>, ~/.cache/tmp-<lane> locally.
#
# The source tree is the git toplevel of the current directory (worktrees work). It is rsynced
# to ~/lanes/<lane>/edith_codecs on the host, without target/, fixtures/ and .git. The host gets
# fixtures -> ~/fixtures as a symlink. The command runs under the host's slot lock (~/.remote-slot),
# so one heavy job at a time per server.
#
# Three job slots per host (~/.remote-slot.0, .1 and .2), each with CARGO_BUILD_JOBS=2.
# Remote env: CARGO_TARGET_DIR=~/.cache/tgt-<lane>, TMPDIR=~/tmp/<lane>, CARGO_BUILD_JOBS=2,
# EC_MEMGUARD_MAX=3500M (the memory cap read by scripts/memguard-runner.sh). Every EC_* variable
# from the local environment is forwarded and wins over the defaults.
#
# Exit codes: the remote command's code. 75 means the slot stayed busy (with --host a or b, after
# 10 minutes). A command that itself exits 75 looks the same and is retried on the next host.
# 255 means the chosen host was unreachable. stderr shows "remote-run: host=a|b|c|local" when a run starts.

set -uo pipefail

[ -f "$HOME/.config/edith_codecs/remote-hosts" ] && . "$HOME/.config/edith_codecs/remote-hosts"
HOST_A=${EC_HOST_A:-}
HOST_B=${EC_HOST_B:-}
HOST_C=${EC_HOST_C:-}
EDITH_TREE=${EDITH_TREE:-$HOME/Documents/Code/Rust/edith}
EDITH_ASSETS=${EDITH_ASSETS:-$HOME/Documents/Code/Rust/edith/assets}
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=20)
RSYNC_RSH="ssh -o BatchMode=yes -o ConnectTimeout=20"
BUSY=75
UNREACH=254
RETRY_SECS=20
RETRY_TOTAL=600

die() { echo "remote-run: $*" >&2; exit 2; }

[ $# -ge 1 ] || { sed -n '2,25p' "$0" >&2; exit 2; }
case $1 in
  -h|--help) sed -n '2,25p' "$0" >&2; exit 0 ;;
esac
lane=$1
shift

host=auto
with_edith=0
clean=0
cmd=()
while [ $# -gt 0 ]; do
  case $1 in
    --host) [ $# -ge 2 ] || die "--host needs a value"; host=$2; shift 2 ;;
    --host=*) host=${1#--host=}; shift ;;
    --with-edith) with_edith=1; shift ;;
    --clean) clean=1; shift ;;
    --) shift; cmd=("$@"); break ;;
    *) die "unknown argument: $1 (put the command after --)" ;;
  esac
done

[[ $lane =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die "bad lane name: $lane"
case $host in
  auto|a|b|c) ;;
  *) die "--host must be auto, a, b or c" ;;
esac

host_addr() { case $1 in a) echo "$HOST_A" ;; b) echo "$HOST_B" ;; c) echo "$HOST_C" ;; esac; }

# rsh <ssh-target> <script>: run a bash script on the host. The script goes through stdin, so no
# quoting reaches fish, the remote login shell.
rsh() { printf '%s\n' "$2" | "${SSH[@]}" "$1" 'bash -c "$(cat)"'; }

# Literal "$HOME/lanes/<lane>/<rel>" text, meant to be expanded by the remote bash.
remote_path() { printf '%s' "\$HOME/lanes/$lane/$1"; }

forwarded_env() {
  local name
  while IFS= read -r name; do
    case $name in
      EC_*) printf 'export %s=%q\n' "$name" "${!name}" ;;
    esac
  done < <(compgen -e)
}

# sync_tree <addr> <local-dir> <remote-rel> [rsync filters...]
sync_tree() {
  local addr=$1 src=$2 rel=$3 lock try
  shift 3
  # Parallel runs of one lane sync into the same remote tree; a concurrent --delete
  # removes the other's temp files (rsync code 23), which used to read as "unreachable".
  # Serialise syncs per lane and host, and retry a partial transfer once.
  # --checksum --no-times: a file whose content changed lands with the host's current
  # mtime. Kept source mtimes can be older than the host's last build (a merge of a
  # lane edited yesterday), and cargo then reuses a stale artifact of the old content.
  lock="${XDG_CACHE_HOME:-$HOME/.cache}/remote-sync-$lane-${addr##*@}.lock"
  for try in 1 2; do
    if flock "$lock" rsync -a --checksum --no-times --delete -e "$RSYNC_RSH" --exclude=__pycache__/ "$@" \
        "$src/" "$addr:lanes/$lane/$rel/"; then
      return 0
    fi
  done
  return 1
}

# run_payload <host-letter>: the remote script that runs under the slot lock.
run_payload() {
  local letter=$1 cmd_q run_rel=edith_codecs
  (( with_edith )) && run_rel=edith
  printf -v cmd_q '%q ' "${cmd[@]}"
  forwarded_env
  cat <<EOF
export PATH="\$HOME/.cargo/bin:\$PATH"
export CARGO_TARGET_DIR="\$HOME/.cache/tgt-$lane" TMPDIR="\$HOME/tmp/$lane" CARGO_BUILD_JOBS=2
export EC_MEMGUARD_MAX="\${EC_MEMGUARD_MAX:-3500M}"
mkdir -p "\$TMPDIR" "\$CARGO_TARGET_DIR" || exit 1
cd "\$HOME/lanes/$lane/edith_codecs" || exit 1
[ -e .git ] || git init -q || exit 1
ln -sfn "\$HOME/fixtures" fixtures   # replace a local worktree link that would dangle here
EOF
  if (( with_edith )); then
    cat <<EOF
(cd "\$HOME/lanes/$lane/edith" && { [ -e .git ] || git init -q; }) || exit 1
# libspa-sys runs bindgen; with clang-libs but no clang binary, libclang misses its own
# builtin headers (stdbool.h), so name the newest resource include dir explicitly.
inc=\$(ls -d /usr/lib/clang/*/include 2>/dev/null | sort -V | tail -1)
[ -n "\$inc" ] && export BINDGEN_EXTRA_CLANG_ARGS="\${BINDGEN_EXTRA_CLANG_ARGS:+\$BINDGEN_EXTRA_CLANG_ARGS }-I\$inc"
EOF
  fi
  cat <<EOF
got=0
for slot in 0 1 2; do
  exec 9>"\$HOME/.remote-slot.\$slot" || exit 1
  if flock -n 9; then got=1; break; fi
  exec 9>&-
done
[ "\$got" = 1 ] || exit $BUSY
echo "remote-run: host=$letter" >&2
cd "\$HOME/lanes/$lane/$run_rel" || exit 1
$cmd_q
EOF
}

if (( clean )); then
  for h in a b c; do
    echo "remote-run: cleaning host=$h" >&2
    rsh "$(host_addr "$h")" "rm -rf \"\$HOME/lanes/$lane\" \"\$HOME/.cache/tgt-$lane\" \"\$HOME/tmp/$lane\"" \
      || echo "remote-run: clean failed on host=$h (rc $?)" >&2
  done
  rm -rf "$HOME/.cache/tgt-$lane" "$HOME/.cache/tmp-$lane"
  echo "remote-run: clean done" >&2
  exit 0
fi

(( ${#cmd[@]} > 0 )) || die "no command given (usage: $0 $lane [opts] -- <command...>)"
ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || die "not inside a git checkout"
if (( with_edith )) && [ ! -d "$EDITH_TREE" ]; then
  die "edith tree not found: $EDITH_TREE"
fi

# try_host <letter>: sync, then run. Returns the remote exit code, $BUSY, or $UNREACH.
try_host() {
  local letter=$1 addr setup rc
  addr=$(host_addr "$letter")
  setup="mkdir -p \"$(remote_path edith_codecs)\""
  (( with_edith )) && setup+=" \"$(remote_path edith)\""
  # A worktree's assets/ may be a symlink to the main checkout; never leave one on the host.
  (( with_edith )) && setup+="; if [ -L \"$(remote_path edith/assets)\" ]; then rm \"$(remote_path edith/assets)\"; fi"
  rsh "$addr" "$setup" || { echo "remote-run: host=$letter setup failed" >&2; return $UNREACH; }
  sync_tree "$addr" "$ROOT" edith_codecs --exclude=target/ --exclude=/fixtures --exclude=/.git --exclude=/graphify-out --exclude=fuzz/corpus/ --exclude=fuzz/artifacts/ --exclude=/scripts/aac-tables/sbr-cache.json --exclude=/lanes/i64/ --exclude=/lanes/b64b/ \
    || { echo "remote-run: host=$letter rsync of edith_codecs failed" >&2; return $UNREACH; }
  if (( with_edith )); then
    sync_tree "$addr" "$EDITH_TREE" edith --exclude=target/ --exclude=/assets --exclude=/.git --exclude=/graphify-out \
      || { echo "remote-run: host=$letter rsync of edith failed" >&2; return $UNREACH; }
    # assets/ is gitignored, so worktrees lack it; the tests read it. Take the main checkout's copy (~28 MB).
    if [[ -d "$EDITH_ASSETS" ]]; then
      sync_tree "$addr" "$EDITH_ASSETS" edith/assets \
        || { echo "remote-run: host=$letter rsync of edith assets failed" >&2; return $UNREACH; }
    fi
  fi
  rsh "$addr" "$(run_payload "$letter")"
  rc=$?
  (( rc == 255 )) && return $UNREACH
  return $rc
}

run_local() {
  local dir=$ROOT tgt=$HOME/.cache/tgt-$lane tmp=$HOME/.cache/tmp-$lane
  (( with_edith )) && dir=$EDITH_TREE
  mkdir -p "$tgt" "$tmp" || exit 1
  echo "remote-run: host=local" >&2
  cd "$dir" || exit 1
  CARGO_TARGET_DIR=$tgt TMPDIR=$tmp CARGO_BUILD_JOBS=4 "${cmd[@]}"
  exit $?
}

# probe_host <ssh-target>: print "<busy slots> <MemAvailable kB>" for one host. Each slot lock is
# taken with flock -n and dropped at once, so a running job is never disturbed.
probe_host() {
  local script
  script=$(cat <<'EOF'
busy=0
for slot in 0 1 2; do
  exec 9>"$HOME/.remote-slot.$slot" || exit 1
  flock -n 9 || busy=$((busy + 1))
  exec 9>&-
done
echo "$busy $(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)"
EOF
)
  printf '%s\n' "$script" | ssh -o BatchMode=yes -o ConnectTimeout=5 "$1" 'bash -c "$(cat)"'
}

# auto_order: print the host letters, least loaded first. Unreachable hosts are left out. If none
# answers, print the old order c a b so try_host reports each host as unreachable as before.
auto_order() {
  local letter out busy mem rows=()
  for letter in a b c; do
    out=$(probe_host "$(host_addr "$letter")" 2>/dev/null)
    read -r busy mem <<<"$out"
    if [[ $busy =~ ^[0-9]+$ && $mem =~ ^[0-9]+$ ]]; then
      rows+=("$busy $mem $letter")
    else
      echo "remote-run: host=$letter probe failed, skipped" >&2
    fi
  done
  if (( ${#rows[@]} == 0 )); then
    printf '%s\n' c a b
    return
  fi
  printf '%s\n' "${rows[@]}" | sort -k1,1n -k2,2nr | awk '{print $3}'
}

candidates=("$host")
deadline=$((SECONDS + RETRY_TOTAL))
busy=0
while :; do
  busy=0
  if [ "$host" = auto ]; then
    mapfile -t candidates < <(auto_order)
    echo "remote-run: auto order: ${candidates[*]}" >&2
  fi
  for letter in "${candidates[@]}"; do
    try_host "$letter"
    rc=$?
    case $rc in
      "$UNREACH") echo "remote-run: host=$letter unreachable" >&2 ;;
      "$BUSY") busy=1; echo "remote-run: host=$letter slot busy" >&2 ;;
      *) exit "$rc" ;;
    esac
  done
  (( busy )) || break
  (( SECONDS < deadline )) || break
  sleep "$RETRY_SECS"
done

if [ "$host" != auto ]; then
  if (( busy )); then
    echo "remote-run: host=$host still busy after ${RETRY_TOTAL}s" >&2
    exit "$BUSY"
  fi
  echo "remote-run: host=$host unreachable" >&2
  exit 255
fi
echo "remote-run: no server slot free or reachable, running locally" >&2
run_local
