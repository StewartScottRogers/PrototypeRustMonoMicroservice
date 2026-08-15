#!/usr/bin/env bash
#
# Create the production environment and its protection rules.
#
#   .github/scripts/arm-environments.sh
#
# Until this runs, .github/workflows/deploy.yml still works - GitHub creates a
# missing environment implicitly on first use - but with no reviewer and no
# branch restriction, so nothing pauses for approval and any reference can be
# promoted. The gate is the entire point, so run this before trusting it.
#
# The policy is .github/environments/production.json, kept in version control so
# it is reviewable rather than clicked into a settings page. Same arrangement as
# .github/rulesets/master.json and arm-gates.sh.
#
# Environments with protection rules are free on a public repository. On a
# private one owned by a personal account they need a paid plan, and the create
# call below is what fails.

set -euo pipefail

repo="${1:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
policy="$here/../environments/production.json"
environment="production"

echo "Applying $policy to $repo"

# Everything that is the same for anybody lives in the policy file. The required
# reviewer cannot: it is a numeric account identifier, different for every
# person who clones this, so it is resolved here and merged in.
#
# jq is not assumed - it is not installed everywhere this has to run, which is
# the same reason arm-gates.sh posts its policy file unmodified. The two fields
# are appended with plain text instead.
reviewer_id="$(gh api user --jq .id)"
reviewer_login="$(gh api user --jq .login)"

echo "Required reviewer: $reviewer_login ($reviewer_id)"

body="$(mktemp)"
trap 'rm -f "$body"' EXIT

# Splice the reviewer into the policy: drop the closing brace, add the field.
# `prevent_self_review` is false in the policy on purpose. With a single
# maintainer, who is also the only reviewer, true would mean nobody on earth
# can approve a deployment - the request would sit forever with no error to
# explain why. Set it to true on the day there is a second reviewer.
sed '$ d' "$policy" > "$body"
cat >> "$body" <<REVIEWER
  ,"reviewers": [{ "type": "User", "id": $reviewer_id }]
}
REVIEWER

if ! gh api --method PUT "repos/$repo/environments/$environment" --input "$body" >/dev/null; then
    cat >&2 <<'MESSAGE'

Could not create the environment.

If the error mentioned a plan, that is the expected failure on a private
repository owned by a personal account: environment protection rules need a
paid plan there, and are free on a public repository.
MESSAGE
    exit 1
fi

# Which references may deploy. Only a release tag - the shape release-plz
# produces - so a branch can never be promoted to production, however green it
# looks. This is a second call because the create call above only accepts the
# *policy kind*; the patterns themselves are a separate collection.
#
# Deleting first makes the script idempotent: re-running it must converge on
# this state rather than accumulating a duplicate pattern each time.
existing="$(gh api "repos/$repo/environments/$environment/deployment-branch-policies" \
    --jq '.branch_policies[]?.id' 2>/dev/null || true)"
for id in $existing; do
    gh api --method DELETE \
        "repos/$repo/environments/$environment/deployment-branch-policies/$id" >/dev/null
done

gh api --method POST \
    "repos/$repo/environments/$environment/deployment-branch-policies" \
    -f name='*-v*' -f type='tag' >/dev/null

echo "Done. Deployments to $environment now wait for $reviewer_login, and only a *-v* tag may be deployed."
