#!/usr/bin/env bash
#
# Make CI OK, Image OK and Security OK required to merge into master.
#
#   .github/scripts/arm-gates.sh
#
# Until this runs, every check still executes and reports - but nothing stops a
# red pull request being merged.
#
# GitHub rejects branch protection on a private repository owned by a personal
# account on the free plan, for both rulesets and the older branch-protection
# API. Either make the repository public or move it to a paid plan, then run
# this. The policy itself is .github/rulesets/master.json, kept in version
# control so it is reviewable rather than clicked into a settings page.

set -euo pipefail

repo="${1:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
policy="$here/../rulesets/master.json"

echo "Applying $policy to $repo"

# The _comment key exists for humans reading the file; the API rejects unknown
# top-level fields, so strip it on the way through.
if ! jq 'del(._comment)' "$policy" | gh api --method POST "repos/$repo/rulesets" --input - >/dev/null; then
    cat >&2 <<'MESSAGE'

Could not create the ruleset.

If the error mentioned GitHub Pro, that is the expected failure on a private
repository owned by a personal account on the free plan. Make the repository
public, or move to a paid plan, and run this again. Nothing else needs to
change - the workflows already produce the three checks this policy requires.
MESSAGE
    exit 1
fi

echo "Done. CI OK, Image OK and Security OK are now required on master."
