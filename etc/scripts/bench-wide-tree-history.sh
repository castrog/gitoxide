#!/usr/bin/env bash
# Compare `git log` and `gix-history-bench` on repositories made by `make-wide-tree-history.sh`.
#
# Usage: bench-wide-tree-history.sh <fixture-dir> [extra gix-history-bench options...]
#
# Creates the flat and sharded fixtures in `<fixture-dir>` if they don't exist, then runs
# `hyperfine` for Git and for each `gix-history-bench` mode on the same pack. `hyperfine`
# reports wall time and peak memory (resident set size). Build the example first:
#
#   cargo build --release -p gix --example gix-history-bench
#
# Set `GIX_HISTORY_BENCH` to compare another build, `RUNS` to change the number of runs (default 10),
# `MODES` to a subset of `walk decode diff`, and `FIXTURES` to a space-separated subset of fixture names.
set -eu -o pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <fixture-dir> [gix-history-bench options...]" >&2
  exit 2
fi
root=$(cd "$(dirname "$0")/../.." && pwd)
dir=$1
shift
bench=${GIX_HISTORY_BENCH:-$root/target/release/examples/gix-history-bench}
runs=${RUNS:-10}
modes=${MODES:-walk decode diff}
fixtures=${FIXTURES:-flat-500 flat-2000 flat-4000 sharded-500 sharded-2000 sharded-4000}
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null

mkdir -p "$dir"
for name in $fixtures; do
  [ -d "$dir/$name" ] && continue
  layout=${name%-*}
  packages=${name#*-}
  "$root/etc/scripts/make-wide-tree-history.sh" "$dir/$name" "$packages" $((packages * 2)) "$layout"
done

echo "git: $(git --version), gix: $(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown), bench: $bench $*"
for name in $fixtures; do
  repo="$dir/$name"
  commands=(-n git "git --git-dir=$repo log --name-only --no-renames --format=%H main")
  for mode in $modes; do
    commands+=(-n "gix-$mode" "$bench $repo $mode $*")
  done
  echo "== $name: $(git --git-dir="$repo" count-objects -v | awk '/size-pack/ {print $2 " KiB pack"}')"
  hyperfine -N --warmup 2 --runs "$runs" --style basic "${commands[@]}" 2>&1 | grep -E '^Benchmark|Wall Time|Memory'
done
