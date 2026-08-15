---
name: gh-deploy-env
description: >
  Environments, deployment protection rules, and promoting a published
  container image to production in this repository. Use when adding or changing
  a deployment environment, when a deployment did not pause for approval, when
  a promotion was refused, when deciding what "deploy" should mean before there
  is anywhere to deploy to, or when connecting this pipeline to a real cloud
  target for the first time.
---

# Environments and deployment

`.github/workflows/deploy.yml`, `.github/environments/production.json` and
`.github/scripts/arm-environments.sh` are the last stage of the pipeline: a
human decides that one already-published image digest is the one production
means.

> **Write everything out in full.** No acronyms or abbreviations in prose. A
> widely recognised acronym may follow the full term in parentheses on first
> mention only. Identifiers — workflow names, tags, environment variables —
> stay as they are.

## What "deploy" honestly means here

**There is nowhere to deploy.** No server, no cluster, no cloud account. The
usual centrepiece of a deployment skill — federated identity from GitHub to a
cloud provider, so a workflow can assume a role without a stored credential —
cannot be built against a provider that does not exist, and a workflow that
pretended to deploy would be worse than no workflow at all.

What does exist is a published, attested container image. So the deployable act
is **promotion**: after a human approves, the tag `production` starts pointing
at one specific digest whose build provenance has been verified. Nothing is
copied, rebuilt or restarted.

That is a real deployment in a registry-driven world, and it buys four things
that are not nothing:

1. A human gate, enforced by GitHub rather than by convention.
2. Only a release tag may be promoted, never a branch, however green.
3. The attestation is checked **before** promotion, so a digest that cannot be
   proved to have come from this repository never gets the name.
4. GitHub records a deployment, so what was promoted, when, and by whom is
   queryable rather than remembered.

## Arm it before trusting it

```
.github/scripts/arm-environments.sh
```

Until that runs, `deploy.yml` still works — and that is the trap. **GitHub
creates a missing environment implicitly on first use**, with no reviewer and
no branch restriction. The workflow runs straight through, reports success, and
promotes whatever it was pointed at. The gate is the entire point, so an
unarmed environment is the one failure mode that looks exactly like success.

Same arrangement as `arm-gates.sh` for the branch ruleset: the policy is in
version control so it is reviewable, and a script applies it, so nobody has to
reproduce a settings page from memory.

## Non-negotiable rules

1. **`prevent_self_review` is `false`, deliberately.** With one maintainer, who
   is also the only reviewer, `true` means nobody on earth can approve a
   deployment — the request waits forever with no error explaining why. Set it
   to `true` the day there is a second reviewer, and not before.

2. **Only a `*-v*` tag may deploy.** The deployment branch policy allows tags
   of the shape release-plz produces and nothing else. A branch is a moving
   target; production must name something that cannot change underneath it.

3. **Promote by digest, never by tag.** `deploy.yml` resolves the version tag
   to a digest and retags *that*. Retagging `:0.1.0` directly would promote
   whatever `:0.1.0` happens to point at when the job runs, which is not
   necessarily what was verified a step earlier.

4. **Verify the attestation before promoting, not after.** The image workflow
   signs "this digest came from this workflow, repository and commit". An
   attestation nobody ever reads is a signature on an unopened document; this
   is the half of the arrangement that opens it.

5. **Deployment stays manual.** A release must not promote itself. Everything
   else in this pipeline is automatic on purpose; this one step should need a
   person, which is why the trigger is `workflow_dispatch` alone.

6. **Environments are free on a public repository.** On a private one owned by
   a personal account they need a paid plan, and it is the create call in
   `arm-environments.sh` that fails. Same constraint, same message, as the
   branch ruleset.

## Adding a second environment

Copy `production.json`, change the name in the script, and give it its own
protection rules. Resist adding a `staging` that differs from `production` in
nothing but its name — an environment that gates nothing teaches people that
environments gate nothing.

## Failure modes

| Symptom | Cause | Fix |
| --- | --- | --- |
| The deployment ran without pausing for approval | The environment was created implicitly and has no reviewer | Run `arm-environments.sh`. Success without a pause is what an unarmed environment looks like. |
| `Branch or tag is not allowed to deploy to production` | Dispatched from a branch, or from a tag that is not `*-v*` | Dispatch from the release tag. This is the rule working. |
| The approval request never appears | `prevent_self_review` is `true` and you are the only reviewer | Rule 1. |
| `gh attestation verify` fails | The image was built before attestation was switched on, or was not built by this repository | Do not promote it. Rebuild from the tag with the `Image` workflow, which produces an attested digest. |
| `Could not create the environment`, mentioning a plan | A private repository on a personal account | Rule 6. |

## Waiting on a real target

Everything below needs somewhere to deploy to, and is deliberately not built:

- **Federated identity to a cloud provider.** Exchange the workflow's OpenID
  Connect token for a short-lived cloud credential, so no long-lived secret is
  ever stored. This is the single biggest security improvement available to
  this pipeline, and it cannot be written against a provider that does not
  exist yet.
- **An actual rollout** — applying the promoted digest to a running service,
  and the health check and rollback that must come with it.
- **Environment secrets and variables**, which only mean something once a
  deployment consumes them.

When a target does appear, the shape here does not change: the environment, its
reviewer and its tag restriction stay exactly as they are, and the rollout
becomes another step in the job that already has approval behind it.
