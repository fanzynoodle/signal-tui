#!/usr/bin/env bash
set -euo pipefail

# Fail commits if there are untracked files (excluding .gitignore rules).
# This catches "oops, forgot to git add that new file" early.

untracked="$(git ls-files --others --exclude-standard || true)"
if [[ -n "${untracked}" ]]; then
  cat >&2 <<'EOF'
pre-commit: untracked files found

You probably forgot to add files to this commit, or you need to update .gitignore.

Untracked files:
EOF
  printf '%s\n' "${untracked}" >&2
  cat >&2 <<'EOF'

Fix:
  - add them:        git add <path>
  - or ignore them:  edit .gitignore

To bypass once:
  SKIP=check-no-untracked git commit ...
EOF
  exit 1
fi
