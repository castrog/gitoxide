#!/usr/bin/env bash
# Create a bare repository whose history repeatedly rewrites one wide tree.
#
# Usage: make-wide-tree-history.sh <destination> <packages> <commits> [flat|sharded]
#
# The first commit adds `<packages>` manifests. Each later commit bumps the version of one
# manifest, cycling through packages, so every commit rewrites the root tree, the container
# tree that holds the package directories, the package tree, and one blob.
#
# * `flat` stores package directories directly in `crates/`, so `crates/` has `<packages>`
#   entries. With 4,000 packages it is about 150 KiB, matching the Callisto report.
# * `sharded` stores them as `crates/<shard>/<package>` with 64 shards, so each commit rewrites
#   one small shard tree and a 64-entry `crates/` tree instead of one wide tree.
#
# Objects are written by `git fast-import` and then repacked with the options in `REPACK_ARGS` (see below),
# so Git and gix read the same Git-generated pack. A commit-graph is written as `git gc` would.
set -eu -o pipefail

if [ $# -lt 3 ] || [ $# -gt 4 ]; then
  echo "usage: $0 <destination> <packages> <commits> [flat|sharded]" >&2
  exit 2
fi
dest=$1
packages=$2
commits=$3
layout=${4:-flat}
shards=64
# `git repack` defaults; override with e.g. `REPACK_ARGS="-a -d -f --depth=50 --window=250"` (`git gc --aggressive`).
read -r -a REPACK_ARGS <<< "${REPACK_ARGS:--a -d -f --depth=50 --window=10}"

case "$layout" in
  flat|sharded) ;;
  *) echo "layout must be 'flat' or 'sharded'" >&2; exit 2 ;;
esac
if [ -e "$dest" ]; then
  echo "refusing to overwrite existing '$dest'" >&2
  exit 1
fi

# Keep user and system configuration out of the generated pack.
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
git init --quiet --bare --initial-branch=main "$dest"

package_path() {
  local name
  name=$(printf 'package-%04d' "$1")
  if [ "$layout" = sharded ]; then
    printf 'crates/%02x/%s/Cargo.toml' $(($1 % shards)) "$name"
  else
    printf 'crates/%s/Cargo.toml' "$name"
  fi
}

# Print a fast-import `M` command with an inline manifest for package `$1` at version `$2`.
manifest() {
  local content
  content=$(printf '[package]\nname = "package-%04d"\nversion = "0.1.%d"\nedition = "2021"\n' "$1" "$2")
  printf 'M 100644 inline %s\ndata %d\n%s\n' "$(package_path "$1")" "${#content}" "$content"
}

commit_header() {
  local message="$2"
  printf 'commit refs/heads/main\nmark :%d\n' "$1"
  printf 'author Bench <bench@example.com> %d +0000\n' $((1700000000 + $1))
  printf 'committer Bench <bench@example.com> %d +0000\n' $((1700000000 + $1))
  printf 'data %d\n%s\n' "${#message}" "$message"
}

{
  commit_header 1 "add $packages packages"
  for ((p = 0; p < packages; p++)); do
    manifest "$p" 0
  done
  for ((c = 2; c <= commits; c++)); do
    p=$(((c - 2) % packages))
    commit_header "$c" "bump package $p"
    manifest "$p" "$c"
  done
} | git -C "$dest" fast-import --quiet

git -C "$dest" repack --quiet "${REPACK_ARGS[@]}"
git -C "$dest" prune-packed
git -C "$dest" commit-graph write --reachable --no-progress
echo "$dest: layout=$layout packages=$packages commits=$(git -C "$dest" rev-list --count main) repack='${REPACK_ARGS[*]}'"
