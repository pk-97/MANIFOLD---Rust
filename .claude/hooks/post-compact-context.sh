#!/bin/bash
# Re-injects critical context after context compaction.
# The agent loses CLAUDE.md details after compaction —
# this reload prevents drift in long sessions.

cat <<'EOF'
=== MANIFOLD — POST-COMPACTION CONTEXT RELOAD ===

This is a native Rust application (Edition 2024, wgpu 28, winit 0.30).
Unity source at /Users/peterkiemann/MANIFOLD - Render Engine/ is the
BEHAVIORAL REFERENCE for remaining parity gaps (~44 tracked).

ARCHITECTURE:
- Two-thread model: content thread (PlaybackEngine, EditingService, ContentPipeline)
  + UI thread (winit event loop, UIRoot, UIBridge)
- Communication: ContentCommand (UI→Content) + ContentState (Content→UI) via crossbeam
- Typed IDs: ClipId, LayerId, EffectGroupId (newtypes wrapping String)
- AHashMap on hot paths, parking_lot mutexes, lock-free MIDI (AtomicU64 CAS)

PERFORMANCE: No per-frame allocations on hot paths. Pre-allocated scratch buffers.
AHashMap for all clip/effect lookups. Static sort comparisons.

DEVELOPMENT:
- cargo clippy --workspace -- -D warnings (before commit)
- cargo test --workspace (before commit)
- Commit to main, push. CI confirms.
- #[serde(rename_all = "camelCase")] on all serialized structs
- Uniform structs: 16-byte aligned, #[repr(C)], field order matches WGSL

FOR PARITY GAP WORK (porting from Unity):
1. READ the Unity .cs source FIRST — HALT if you haven't read it
2. TRANSLATE line-by-line — same logic, same edge cases, same constants
3. VERIFY value-level parity — every constant, format, math op matches exactly
4. UPDATE docs/parity_tracker.json + docs/PORT_STATUS.md

FAILURE MODES (most common drift after compaction):
- FM-1: NEVER synthesize from docs — only from .cs source files
- FM-9: NEVER invent platform limits — use Unity's exact values
- FM-10: Match texture formats exactly — RFloat→R32Float (not Rgba16Float)
- FM-12: Match math ops — RoundToInt→.round(), Lerp clamps t, Repeat≠modulo
- FM-14: Port services as COMPLETE UNITS, not scattered inline

FROZEN: docs/DEFINITIVE_PARITY_AUDIT.md — DO NOT EDIT
Full contract: CLAUDE.md — re-read if uncertain about any decision.
EOF
exit 0
