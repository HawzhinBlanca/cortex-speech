#!/usr/bin/env bash
# Turnkey history scrub for the PUBLIC repo (deep-audit F3, history leg).
#
# F3 stopped the owner-PII leak GOING FORWARD (untracked the private dataset JSONs, parameterized the
# owner-name path filters). But those strings/files still exist in the already-published git HISTORY.
# Purging the past requires REWRITING history + a force-push — a destructive, owner-only operation
# (it changes every commit hash; anyone who cloned must re-clone). The agent will NOT do this
# automatically (charter: never force-push shared history). Run this yourself when ready.
#
# Removes from ALL history:
#   1. The private per-owner datasets: *_perfect_dataset.json  (paths, every commit).
#   2. (optional) The owner-name local-folder path fragments inside past versions of the dev scripts,
#      when you pass the owner token via CORTEX_OWNER (kept OUT of this file so the repo carries no PII).
#
# Prereqs: git-filter-repo  ->  pip install git-filter-repo
# Usage:
#   bash scripts/scrub_git_history.sh                          # dry run (safety checks + plan)
#   CORTEX_OWNER=Yourname CONFIRM=1 bash scripts/scrub_git_history.sh   # rewrite, then force-push manually
set -euo pipefail

cd "$(dirname "$0")/.."   # repo root

echo "== Cortex public-repo history scrub (F3) =="
echo "repo: $(git rev-parse --show-toplevel)"
echo "current branch: $(git rev-parse --abbrev-ref HEAD)"
echo

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "ABORT: working tree is dirty. Commit or stash first." >&2
  exit 1
fi
if ! command -v git-filter-repo >/dev/null 2>&1 && ! python -c "import git_filter_repo" >/dev/null 2>&1; then
  echo "git-filter-repo not found. Install it:  pip install git-filter-repo" >&2
  exit 1
fi

# Build the string-redaction list at RUNTIME from CORTEX_OWNER, so this tracked script never embeds
# the private folder name (it would otherwise trip scripts/test_windows_repo_hygiene.py). The public
# GitHub handle is NOT redacted — only the local-folder path form.
REPL_ARGS=()
REPL_FILE=""
if [ -n "${CORTEX_OWNER:-}" ]; then
  o="$CORTEX_OWNER"
  bs='\'
  REPL_FILE="$(mktemp)"
  {
    printf 'regex:%%%s%s[^%%]*%%==>%%CORTEX_AUDIO_LIKE%%\n' "$o" "$bs$bs"
    printf 'literal:Desktop%s%s%s==>Desktop%sCortexAudio%s\n' "$bs" "$o" "$bs" "$bs" "$bs"
    printf 'literal:Desktop/%s/==>Desktop/CortexAudio/\n' "$o"
  } > "$REPL_FILE"
  REPL_ARGS=(--replace-text "$REPL_FILE")
  echo "Will redact the local-folder form of owner token '$o' from history."
else
  echo "CORTEX_OWNER unset -> will ONLY remove the dataset files (no string redaction)."
fi
echo "Will remove from history (all commits):  *_perfect_dataset.json"
echo

if [ "${CONFIRM:-0}" != "1" ]; then
  echo "DRY RUN. Re-run with CONFIRM=1 (and CORTEX_OWNER=... if redacting strings)."
  [ -n "$REPL_FILE" ] && rm -f "$REPL_FILE"
  exit 0
fi

echo "Rewriting history (make a backup clone first if unsure)…"
git filter-repo --force --path-glob '*_perfect_dataset.json' --invert-paths "${REPL_ARGS[@]}"
[ -n "$REPL_FILE" ] && rm -f "$REPL_FILE"

echo
echo "Done. Manual next steps:"
echo "  1. Verify:  git log -p -- '*_perfect_dataset.json'   (should be empty)"
echo "  2. Re-add remote if needed:  git remote add origin <url>"
echo "  3. Force-push each published branch:  git push --force-with-lease origin main"
echo "  4. Collaborators must re-clone (old clones keep the leaked history)."
