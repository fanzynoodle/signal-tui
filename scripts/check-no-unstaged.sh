#!/usr/bin/env bash
set -euo pipefail

# Fail commits if there are unstaged changes in tracked files.
# This catches "oops, I edited something but forgot to stage it".

unstaged="$(git diff --name-only || true)"
if [[ -n "${unstaged}" ]]; then
  cat >&2 <<'EOF'
pre-commit: unstaged changes found

You probably forgot to stage changes for this commit, or you meant to keep local edits out of the commit.

Unstaged paths:
EOF
  printf '%s\n' "${unstaged}" >&2
  cat >&2 <<'EOF'

Fix:
  - stage them:      git add <path>
  - or discard them: git restore <path>

To bypass once:
  SKIP=check-no-unstaged git commit ...
EOF
  exit 1
fi
