#!/usr/bin/env bash
# Install repo-tracked git hooks by pointing core.hooksPath at .githooks/.
#
# Idempotent: re-running is a no-op once configured.
#
# Usage:
#   scripts/install-git-hooks.sh [--check]
#
#   --check  Exit 0 if hooks are already installed, non-zero otherwise.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOKS_DIR_REL=".githooks"
HOOKS_DIR="$REPO_ROOT/$HOOKS_DIR_REL"

if [[ ! -d "$HOOKS_DIR" ]]; then
  echo "error: $HOOKS_DIR not found" >&2
  exit 1
fi

CURRENT="$(git -C "$REPO_ROOT" config --local --get core.hooksPath || true)"

if [[ "${1:-}" == "--check" ]]; then
  if [[ "$CURRENT" == "$HOOKS_DIR_REL" ]]; then
    echo "ok: core.hooksPath = $HOOKS_DIR_REL"
    exit 0
  fi
  echo "fail: core.hooksPath = '$CURRENT' (expected '$HOOKS_DIR_REL')" >&2
  exit 1
fi

# Mark every hook executable. Git refuses to run non-executable hooks.
find "$HOOKS_DIR" -maxdepth 1 -type f -not -name '*.md' -exec chmod +x {} +

if [[ "$CURRENT" == "$HOOKS_DIR_REL" ]]; then
  echo "git hooks already installed (core.hooksPath = $HOOKS_DIR_REL)"
  exit 0
fi

git -C "$REPO_ROOT" config --local core.hooksPath "$HOOKS_DIR_REL"
echo "installed: core.hooksPath -> $HOOKS_DIR_REL"
echo
echo "Active hooks:"
ls -1 "$HOOKS_DIR" | grep -v '\.md$' | sed 's/^/  /'
echo
echo "Test the post-merge hook locally with:"
echo "  RB_SKIP_AUTO_REBUILD=1 git pull origin main   # safe (no rebuild)"
echo "  git pull origin main                          # real (triggers rebuild)"
