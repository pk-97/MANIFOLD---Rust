# Landing — A1 projection verdict + P0 orphan purge (2026-07-09, Fable)

**Branch:** `feat/a1-projection-verdict-p0` · **Docs:** UI_PROJECTION_LAYER_DESIGN.md rewritten
with the kill verdict; STRUCTURAL_AUDIT_VERDICTS §2/§3 amended + §5 confirmed + §6 added;
FOUNDATIONAL_GAPS A1 outcome; KICK_SWEEP §6.6/§4 truth-fixes; BUG-083/084 filed.

**Status line (quoted):** `SHIPPED (P0, the only surviving phase) · 2026-07-09 · Fable.`

**What landed (code):** 10 orphan `ContentState` fields deleted (6 FIXME'd + 4 the lint found
when un-suppressed: `is_exporting`/`export_progress`/`export_status`/`recording_dropped_frames`),
their emit writes, the write-only `cached_osc_timecode` cache; `send_export_progress` demoted
to documented keep-alive; struct-level `#[allow(dead_code)]` removed — rustc dead_code +
the pre-commit clippy gate is now the permanent orphan enforcement (I1).

**Gate output:** `cargo clippy --workspace -- -D warnings` → Finished, zero warnings (allow
removed). `cargo test -p manifold-app` → 163+10+1+3 pass, 0 fail. `docs_index_sync` green.
Negative `rg` for the 10 field names in `manifold-app/src` → zero hits.

**Level reached:** L1 (deletion of never-read fields has no observable surface; the
pre-deletion observation was the lint firing — §1.1 proof command in the design doc).

**Verification debt:** none opened — no behavior changed; export/recording behavior was
already feedback-less (that's BUG-083/084, pre-existing, now tracked).

**Shortcuts taken:** none.

**Click-script for Peter (≤2 min):**
1. Pull main, run the app, load any project. Expected: everything behaves identically
   (deleted fields were never read).
2. Optional: start a video export. Expected: same as before — no progress display until the
   finish toast. That gap is now BUG-083 (MED) and is the release-relevant find of this pass.

**Deviation from the draft:** the pre-Fable draft recommended C-then-A; this landing ships C
(as a lint un-suppression, not a new test) and KILLS A — evidence chain in the design doc
§1.1/§2. Peter may override; the reviving trigger is named there.
