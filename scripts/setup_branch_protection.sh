#!/usr/bin/env bash
#
# Protect `main` — external review 2026-08-06 P5.6 / charter item 49.
#
# WHY THIS IS A SCRIPT AND NOT A LIST OF CLICKS. Branch protection configured by hand is invisible: it
# lives only in a settings page, nobody can review it, and nobody can tell whether it drifted. Checked in,
# it is auditable, reproducible on a fork or a restored repo, and its reasoning survives.
#
#   HOW TO RUN (needs repo-admin rights; the OAuth flow cannot run in a headless session):
#     gh auth login            # once, interactively
#     bash scripts/setup_branch_protection.sh
#
#   Verify afterwards:
#     gh api repos/HawzhinBlanca/cortex-speech/branches/main/protection | python -m json.tool
#
set -euo pipefail

REPO="${REPO:-HawzhinBlanca/cortex-speech}"
BRANCH="${BRANCH:-main}"

# REQUIRED APPROVALS — read this before changing it.
#
# The audit says "require review". On a SOLO repository that is a trap: GitHub does not let you approve
# your own pull request, so `required_approving_review_count: 1` makes `main` permanently unmergeable by
# its only maintainer. The honest configuration for one maintainer is 0 — which still forces every change
# through a PR and through the required checks, but does not pretend a second pair of eyes exists.
#
# Set this to 1 the moment there IS a second maintainer. Until then, be clear-eyed: this enforces
# "the gates must pass", not "somebody else looked".
REQUIRED_APPROVALS="${REQUIRED_APPROVALS:-0}"

# The four checks that actually run on a pull request, named exactly as GitHub reports them (the `name:`
# of each job in .github/workflows/ci.yml — NOT the job id, which is what a hand-written list usually
# gets wrong and which silently required nothing).
read -r -d '' CONTEXTS <<'JSON' || true
[
  "Provenance & License Gate",
  "Windows Release Gate",
  "Linux Build Smoke",
  "macOS Build Smoke"
]
JSON

echo "Protecting ${REPO}@${BRANCH}"
echo "  required approvals : ${REQUIRED_APPROVALS}"
echo "  required checks    : $(echo "$CONTEXTS" | tr -d '\n[:space:]')"
echo

payload=$(cat <<JSON
{
  "required_status_checks": {
    "strict": true,
    "contexts": ${CONTEXTS}
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": ${REQUIRED_APPROVALS},
    "dismiss_stale_reviews": true,
    "require_last_push_approval": false
  },
  "restrictions": null,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "block_creations": false,
  "required_conversation_resolution": true,
  "lock_branch": false,
  "allow_fork_syncing": false
}
JSON
)

# Notes on the settings that are not self-explanatory:
#
#   strict: true              — a PR must be up to date with main before merging, so a check cannot pass
#                               against a stale base and then break main on merge.
#   enforce_admins: true      — the rules apply to admins too. Without this the protection is theatre:
#                               the one person who would bypass it is the only person who can.
#                               CONSEQUENCE, stated plainly: after this runs, the direct
#                               `git push origin <branch>:main` this repo's CLAUDE.md documents STOPS
#                               WORKING. Every change goes through a PR. That is the intent.
#   dismiss_stale_reviews     — a new push invalidates prior approvals. Harmless at 0 approvals, correct
#                               the moment approvals are raised.
#   required_linear_history   — matches how this repo already moves main (fast-forward), and keeps the
#                               history bisectable.
#   allow_force_pushes: false — force-push to main is how published history gets rewritten under people.
#   required_conversation_resolution — an unresolved review comment cannot be merged past silently.
#
# RECOVERY, because a protected branch can lock you out: if a required check is renamed or removed, main
# becomes unmergeable until the contexts list matches reality. Fix it by re-running this script with the
# corrected names, or temporarily:
#   gh api -X DELETE repos/${REPO}/branches/${BRANCH}/protection

echo "$payload" | gh api -X PUT "repos/${REPO}/branches/${BRANCH}/protection" --input - >/dev/null

echo "Applied. Current state:"
gh api "repos/${REPO}/branches/${BRANCH}/protection" \
  --jq '{
    checks: .required_status_checks.contexts,
    strict: .required_status_checks.strict,
    admins_included: .enforce_admins.enabled,
    approvals: .required_pull_request_reviews.required_approving_review_count,
    linear_history: .required_linear_history.enabled,
    force_pushes: .allow_force_pushes.enabled,
    deletions: .allow_deletions.enabled,
    conversation_resolution: .required_conversation_resolution.enabled
  }'

cat <<'EOF'

Done. Two things change from here:

  1. `git push origin <branch>:main` no longer works — by design. Open a PR instead:
       git push origin codex/newbranch
       gh pr create --base main --head codex/newbranch --fill
       gh pr merge --squash --auto      (merges itself once the four checks go green)

  2. `verify_10.py`'s `branch-protection` leg is owner-gated and does not read GitHub. It stays listed
     as pending until someone moves it out of OWNER_GATED deliberately — this script does not and must
     not edit that list. Configuring the thing and claiming the gate are different acts.
EOF
