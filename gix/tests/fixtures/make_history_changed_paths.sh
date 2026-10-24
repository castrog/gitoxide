#!/usr/bin/env bash
set -eu -o pipefail

# A history that repeatedly rewrites one wide tree, so that a full-history diff looks up many
# versions of the same trees, and that has every kind of parent relationship a history walk has
# to handle: ordinary commits, a merge, an octopus merge, a type change, a deletion, and a
# `hotfix` branch that is discarded, so its commits must not be part of the history of `main`.
#
# All objects are repacked so trees are stored as long delta chains, which makes the pack delta
# cache matter. `changed-paths.baseline` records, for each commit reachable from `main` and each of
# its parents, the paths `git diff-tree -r` reports as `<commit> <parent> <path>`, using `-` as parent
# of root commits, as computed by Git in each repository:
#
# * `repo`: the full history.
# * `replaced`: like `repo`, but one commit is replaced by `refs/replace/` with a commit that changes
#   another file, which affects the diffs with its parent and with its child.
# * `shallow.git`: a shallow clone of `repo` deep enough to contain the octopus merge, whose boundary commits
#   are diffed with the empty tree.

baseline() {
  git rev-list main | while read -r commit; do
    read -r -a parents <<<"$(git rev-list --parents -n 1 "$commit" | cut -s -d' ' -f2-)"
    if [ "${#parents[@]}" = 0 ]; then
      git diff-tree -r --root --no-renames --name-only --no-commit-id "$commit" | sed "s/^/$commit - /"
    else
      for parent in "${parents[@]}"; do
        git diff-tree -r --no-renames --name-only "$parent" "$commit" | sed "s/^/$commit $parent /"
      done
    fi
  done | LC_ALL=C sort >changed-paths.baseline
}

bump() {
  echo "version = $2" >"crates/package-$1/Cargo.toml"
  git add "crates/package-$1/Cargo.toml"
  git commit -q -m "bump package-$1 to $2"
}

git init -q repo
(cd repo
  git checkout -q -b main
  mkdir crates
  for package in $(seq -w 100 249); do
    mkdir "crates/package-$package"
    echo "version = 0" >"crates/package-$package/Cargo.toml"
  done
  echo readme >README
  git add .
  git commit -q -m "add 150 packages"

  for version in $(seq 1 40); do
    bump $((100 + version % 150)) "$version"
  done

  git checkout -q -b feature
  bump 200 feature
  mkdir crates/package-200/src
  echo "fn main() {}" >crates/package-200/src/main.rs
  git add crates/package-200/src
  git commit -q -m "add a source directory"
  git checkout -q main
  bump 120 main
  git merge -q --no-ff -m "merge feature" feature

  for package in 130 140 150; do
    git checkout -q -b "octopus-$package" main
    bump "$package" octopus
  done
  git checkout -q main
  git merge -q -m "octopus merge" octopus-130 octopus-140 octopus-150

  git checkout -q -b hotfix
  bump 160 hotfix
  bump 161 hotfix
  git checkout -q main

  git rm -q -r crates/package-170
  echo "now a file" >crates/package-170
  git add crates/package-170
  git commit -q -m "replace a package directory with a file"
  git rm -q -r crates/package-171
  git commit -q -m "delete a package"

  for version in $(seq 41 60); do
    bump $((100 + version % 150)) "$version"
  done

  git repack -q -a -d -f --depth=50 --window=10
  git commit-graph write --no-progress --reachable
  baseline
)

git clone -q --no-local --bare repo replaced
(cd replaced
  target=$(git rev-parse main~5)
  readme=$(echo replaced | git hash-object -w --stdin)
  tree=$(git ls-tree "$target" | sed "s/^\(100644 blob \)[0-9a-f]*\(	README\)$/\1$readme\2/" | git mktree)
  replacement=$(git cat-file commit "$target" | sed "1s/^tree .*/tree $tree/" | git hash-object -t commit -w --stdin)
  git replace "$target" "$replacement"
  git remote remove origin
  baseline
)

git clone -q --no-local --bare --depth 25 "file://$PWD/repo" shallow.git
(cd shallow.git
  git remote remove origin
  baseline
)
