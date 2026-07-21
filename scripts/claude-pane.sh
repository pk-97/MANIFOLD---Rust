#!/usr/bin/env bash
# claude-pane.sh — launch a Claude Code session in a NEW tmux pane (or window)
# WITHOUT stealing focus from the caller (the 2026-07-21 "nuked your own
# window" incident: bare `tmux new-window` switches focus, which kills the
# calling agent's visible pane). Always passes -d.
#
# Usage:
#   .claude/scripts/claude-pane.sh [-m model] [-e effort] [-w] [-n name] \
#       [-c dir] (-f prompt-file | "prompt text")
#
#   -m model    fable|opus|sonnet|haiku|opusplan (default: fable)
#   -e effort   low|medium|high (default: medium)
#   -w          open a new WINDOW instead of a split pane (still unfocused)
#   -n name     window/pane title (default: claude-<model>)
#   -c dir      working directory (default: this repo root)
#   -f file     read the init prompt from a file (recommended for long briefs;
#               the session is told to read the file, keeping the CLI arg short)
set -euo pipefail

MODEL="fable"; EFFORT="medium"; MODE="pane"; NAME=""; PROMPT_FILE=""
DIR="$(cd "$(dirname "$0")/.." && pwd)"

while getopts "m:e:wn:c:f:" opt; do
  case "$opt" in
    m) MODEL="$OPTARG" ;;
    e) EFFORT="$OPTARG" ;;
    w) MODE="window" ;;
    n) NAME="$OPTARG" ;;
    c) DIR="$OPTARG" ;;
    f) PROMPT_FILE="$OPTARG" ;;
    *) echo "usage: see header of $0" >&2; exit 2 ;;
  esac
done
shift $((OPTIND - 1))

[ -n "${TMUX:-}" ] || { echo "error: not inside tmux" >&2; exit 1; }
NAME="${NAME:-claude-$MODEL}"

if [ -n "$PROMPT_FILE" ]; then
  [ -f "$PROMPT_FILE" ] || { echo "error: no such prompt file: $PROMPT_FILE" >&2; exit 1; }
  PROMPT="Read $(printf '%q' "$PROMPT_FILE") and execute it."
else
  PROMPT="${1:-}"
  [ -n "$PROMPT" ] || { echo "error: need a prompt or -f file" >&2; exit 2; }
fi

CMD="claude --model $(printf '%q' "$MODEL") --effort $(printf '%q' "$EFFORT") $(printf '%q' "$PROMPT")"

# -d on both forms: create WITHOUT switching focus — the entire point.
if [ "$MODE" = "window" ]; then
  tmux new-window -d -n "$NAME" -c "$DIR" "$CMD"
else
  tmux split-window -d -h -c "$DIR" "$CMD"
  tmux select-pane -t "{last}" -T "$NAME" 2>/dev/null || true
fi
echo "launched $MODE '$NAME': model=$MODEL effort=$EFFORT dir=$DIR"
