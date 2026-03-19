#!/bin/bash
# Runs cargo check before allowing Claude to stop.
# Catches type errors, missing imports, borrow checker issues
# before the user even looks at the output.
#
# Exit 0 = allow stop (compiles fine or no Rust project found)
# Exit 2 = block stop (compilation errors — Claude must fix them)

cd "/Users/peterkiemann/MANIFOLD - Rust" || exit 0

# Quick check: if no .rs files have uncommitted changes, skip
# (covers both staged and unstaged, plus untracked new files)
if ! git diff --name-only HEAD 2>/dev/null | grep -q '\.rs$' && \
   ! git diff --cached --name-only 2>/dev/null | grep -q '\.rs$' && \
   ! git ls-files --others --exclude-standard 2>/dev/null | grep -q '\.rs$'; then
  exit 0
fi

OUTPUT=$(cargo check 2>&1)
RC=$?

if [ $RC -ne 0 ]; then
  ERRORS=$(echo "$OUTPUT" | grep -E '^error' | head -20)
  echo "cargo check FAILED — fix compilation errors before finishing:" >&2
  echo "$ERRORS" >&2
  exit 2
fi

exit 0
