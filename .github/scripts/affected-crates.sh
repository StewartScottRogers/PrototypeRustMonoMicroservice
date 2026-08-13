#!/usr/bin/env bash
#
# Print a JSON array of the workspace crates a diff touches, including every
# crate that depends on them. Feeds the CI test matrix.
#
#   .github/scripts/affected-crates.sh <base-sha>
#
# Run from the workspace root. Needs cargo, jq, and a checkout deep enough to
# contain <base-sha> (actions/checkout with fetch-depth: 0).

set -euo pipefail

base="${1:?usage: affected-crates.sh <base-sha>}"

meta="$(cargo metadata --format-version 1 --no-deps)"
everything="$(jq -c '[.packages[].name] | sort' <<<"$meta")"
changed="$(git diff --name-only "$base" HEAD)"

# A change to a shared build input invalidates the whole workspace, so do not
# pretend otherwise — that is how a monorepo ships a break that CI called green.
if grep -qE '^(Cargo\.(toml|lock)|rust-toolchain\.toml|clippy\.toml|deny\.toml|\.config/|\.github/)' <<<"$changed"; then
    echo "$everything"
    exit 0
fi

# Map each workspace member's directory (repo-relative) to its crate name.
declare -A name_of_dir
declare -A is_member
while IFS=$'\t' read -r name dir; do
    name_of_dir["$dir"]="$name"
    is_member["$name"]=1
done < <(jq -r --arg root "$PWD/" \
    '.packages[] | [.name, (.manifest_path | ltrimstr($root) | rtrimstr("/Cargo.toml"))] | @tsv' \
    <<<"$meta")

declare -A affected
while IFS= read -r file; do
    [ -n "$file" ] || continue
    for dir in "${!name_of_dir[@]}"; do
        if [[ "$file" == "$dir"/* ]]; then
            affected["${name_of_dir[$dir]}"]=1
        fi
    done
done <<<"$changed"

# Walk dependents until the set stops growing: touching service-core must also
# test every service built on top of it.
edges="$(jq -r '.packages[] as $p | $p.dependencies[] | [$p.name, .name] | @tsv' <<<"$meta")"
while :; do
    grew=0
    while IFS=$'\t' read -r pkg dep; do
        # $dep may be an external crate; only workspace members enter the set.
        if [ -n "${is_member[$pkg]:-}" ] && [ -n "${affected[$dep]:-}" ] && [ -z "${affected[$pkg]:-}" ]; then
            affected["$pkg"]=1
            grew=1
        fi
    done <<<"$edges"
    [ "$grew" -eq 0 ] && break
done

# The `+x` test must come first. Under `set -u`, expanding ${#affected[@]} on an
# associative array that was declared but never assigned is an "unbound
# variable" error, not a zero - so the length check cannot be the first thing
# that touches the array. This is the docs-only-change path: a PR that edits no
# crate produces an empty set and an empty matrix.
if [ -z "${affected[*]+x}" ]; then
    echo '[]'
else
    printf '%s\n' "${!affected[@]}" | jq -Rc --slurp 'split("\n") | map(select(length > 0)) | sort'
fi
