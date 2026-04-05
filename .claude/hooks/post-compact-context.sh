#!/bin/bash
# Re-injects critical context after context compaction.
# Keep this aligned with CLAUDE.md — lean identity + rules only.

cat <<'EOF'
=== MANIFOLD — POST-COMPACTION CONTEXT RELOAD ===

Visual DAW for live video performance. Rust codebase is authoritative (no Unity).

ARCHITECTURE:
- Two-thread model: content thread (PlaybackEngine, EditingService, ContentPipeline)
  + UI thread (winit event loop)
- Communication: ContentCommand (UI→Content, bounded 64) + ContentState (Content→UI, bounded 4) via crossbeam
- GPU output: IOSurface zero-copy triple-buffer with atomic front_index
- Project owned exclusively by content thread. UI gets Arc<Project> snapshots.
- All mutations via EditingService → UndoRedoManager → Command

PATTERNS:
- Edition 2024, native Metal GPU (zero wgpu), winit 0.30
- Typed IDs: ClipId, LayerId, EffectGroupId (String newtypes, #[serde(transparent)])
- Typed time: Beats(f64), Seconds(f64), Bpm(f32) — never raw floats in signatures
- AHashMap on hot paths, parking_lot mutexes, lock-free MIDI (AtomicU64 CAS)
- No per-frame allocations. Pre-allocated scratch buffers. Dirty-checking via DataVersion.

GPU:
- Uniform structs: 16-byte aligned, #[repr(C)], _pad fields, field order matches WGSL
- R32Float NOT filterable. R16Float no STORAGE_BINDING. NEVER introduce wgpu.

SERIALIZATION:
- #[serde(rename_all = "camelCase")] on all serialized structs
- #[serde(transparent)] on typed IDs, #[serde(skip)] for runtime-only fields

REFERENCE DOCS (read on-demand):
- docs/MANIFOLD_GPU_ARCHITECTURE.md — GPU, effects, generators, compute
- docs/VSYNC_AND_FRAME_PACING.md — frame pacing, display links
- docs/ADDING_EFFECTS_AND_GENERATORS.md — effect/generator recipes
- docs/DEVELOPMENT_REFERENCE.md — texture formats, math gotchas, module layout

DEVELOPMENT:
- cargo clippy --workspace -- -D warnings + cargo test --workspace before commit
- COMMIT AND PUSH after completing features or fixes
- Runtime bugs: instrument with println, ask user to reproduce, read logs, fix

Re-read CLAUDE.md if uncertain about any decision.
EOF
exit 0
