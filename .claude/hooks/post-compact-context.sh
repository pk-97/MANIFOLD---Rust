#!/bin/bash
# Re-injects critical porting rules after context compaction.
# Without this, long sessions lose the CLAUDE.md contract details
# and the agent starts synthesizing/approximating.

cat <<'EOF'
=== MANIFOLD PORT — POST-COMPACTION CONTEXT RELOAD ===

You are mechanically porting Unity C# to Rust. Unity source at
/Users/peterkiemann/MANIFOLD - Render Engine/ is the SINGLE SOURCE OF TRUTH.

MANDATORY WORKFLOW (every ported file):
1. READ the Unity .cs source FIRST — HALT if you haven't read it
2. MAP every field, method, interface, dependency to Rust equivalents
3. TRANSLATE line-by-line — same logic, same edge cases, same order
4. SELF-AUDIT — did you skip methods, simplify logic, change signatures?
5. VERIFY value-level parity — every constant, format, math op matches exactly
6. UPDATE docs/parity_tracker.json + docs/PORT_STATUS.md

FAILURE MODES (most common drift after compaction):
- FM-1: NEVER synthesize from docs/descriptions — only from .cs source files
- FM-2: NEVER approximate — line-by-line translation only
- FM-9: NEVER invent platform limits — use Unity's exact values
- FM-10: NEVER substitute texture formats — RFloat->R32Float, RGFloat->Rg32Float exactly
- FM-11: NEVER change constants/limits/thresholds — match Unity exactly
- FM-12: Match math ops exactly — RoundToInt->.round(), Lerp clamps t, Repeat!=modulo
- FM-13: Match every param index, uniform name, default value from Unity registries
- FM-14: Port services as COMPLETE UNITS, not scattered inline across event handlers

ARCHITECTURE RULE: Every Unity interface -> Rust trait (same methods).
Every Unity base class -> trait + shared state struct. Never flatten.

FROZEN: docs/DEFINITIVE_PARITY_AUDIT.md — read-only reference, DO NOT EDIT
CHECK: docs/parity_tracker.json before starting any port task

Full contract in CLAUDE.md — re-read if uncertain about any porting decision.
EOF
exit 0
