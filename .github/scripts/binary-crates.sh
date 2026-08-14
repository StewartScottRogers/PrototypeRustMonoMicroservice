#!/usr/bin/env bash
#
# Print a JavaScript Object Notation array of workspace crates that produce a binary — i.e. the ones
# that get a container image. Library crates like service-core are excluded.
#
# Run from the workspace root. Needs cargo and jq.

set -euo pipefail

cargo metadata --format-version 1 --no-deps |
    jq -c '[.packages[] | select(any(.targets[]; .kind | index("bin"))) | .name] | sort'
