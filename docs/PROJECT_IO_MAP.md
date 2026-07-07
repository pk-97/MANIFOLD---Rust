# Project IO & Migration — Current-State Map

Status: AUTHORITATIVE current-state map, 2026-07-07, from a full read of
`manifold-io` (all 11 source files) plus the app-side seams (`project_io.rs`,
`autosave.rs`, `app_lifecycle.rs`). Sibling of CORE_ENGINE_MAP.md /
FREEZE_COMPILER_MAP.md. §9 honest edges is the payload — each is a latent-bug
lens; the top ones are logged as BUG-062..065 in `docs/BUG_BACKLOG.md`.

The stakes here are singular: the `.manifold` file is the one unrecoverable
asset. A renderer bug costs a frame; an IO bug costs the show file.

## 1. Crate shape

| File | Role | Lines |
|---|---|---|
| `loader.rs` | Load pipeline: format detect, migrate, pre-pass, deserialize, 10-step post-load validation | 346 |
| `saver.rs` | `save_project` (V2) / `save_project_v1` (plain JSON, legacy/testing) | 87 |
| `archive.rs` | V2 ZIP writer/reader: manifest, history journal, hash dedup, atomic rename | 486 |
| `migrate.rs` | JSON-level version migration chain v1.0 → v1.11 | 1307 |
| `migrations/param_storage_v14.rs` | The PARAM_STORAGE D4 wire migration (runs as the 1.10→1.11 step) | 677 |
| `path_resolver.rs` | Broken-path re-linking: relative-first, then filename+size search | 313 |
| `manifest.rs` | `ProjectManifest` / `SnapshotEntry` / `ProjectInfo` | 83 |
| `preset_file.rs` / `venue_file.rs` | Standalone preset / stage-layout export-import | 182 / 144 |

No GPU, no renderer dependency. The one seam that needs both project and
renderer catalog (embedded-preset snapshotting) deliberately lives app-side in
`manifold-app/src/project_io.rs`, not here.

## 2. On-disk formats

- **V2 (current)**: ZIP named `.manifold` containing `project.json`
  (uncompressed, for fast reads), `manifest.json` (format version, current
  hash, history index), and `history/<hash>.json.gz` snapshot blobs.
- **V1 (legacy)**: plain JSON text file. Detected by ZIP-open failure —
  there is no magic-byte check; any non-ZIP bytes are treated as V1 JSON.
- Standalone files: preset JSON (`preset_file.rs`), venue/stage JSON
  (`venue_file.rs`). Neither is versioned with a migration chain (§9 E8).
- Snapshot identity and save dedup both key on **the first 6 hex chars (24
  bits) of SHA-256** of the pretty-printed project JSON (`compute_hash`,
  archive.rs:289).

## 3. Save paths — there are exactly two, with different sources

1. **Manual save / Save As** (`app_lifecycle.rs:51`): serializes
   `Application::local_project` — the **UI thread's replica** (the copy UI
   commands execute against optimistically) — after stamping viewport state
   and running `snapshot_and_prune_embedded_presets`. Failure surfaces a
   dialog (G4).
2. **Autosave** (`autosave.rs`): dirty-debounced (60s after the last edit,
   never mid-drag, parked in perform mode), serializes the **content-thread
   snapshot** (`last_snapshot_arc` deep-cloned on a background thread) with a
   `UiStateStamp` of the same viewport fields. Same save path underneath
   (`is_auto = true`). First failure in a streak raises a dialog.

Both funnel into `save_v2_archive`: read old manifest → **dedup** (identical
hash → skip write entirely) → insert new history entry → prune to 50 newest
auto-saves (manual saves are never pruned) → copy surviving history blobs from
the old archive → write new zip to a unique temp file → atomic rename → fsync
the **directory**. On any write error the temp file is removed and the
original archive is untouched.

`ProjectIOService::save_project` (project_io.rs:428) is the ported Unity path,
currently `#[allow(dead_code)]` — the live saves route through the two paths
above. Three save-shaped functions exist for one behavior (§9 E5).

## 4. Load pipeline (in order)

`load_project_with` (loader.rs:49):

1. Read file → try ZIP (`project.json` entry) → else V1 plain JSON.
2. `migrate::migrate_if_needed` — the JSON-level version chain (§5).
3. **Embedded-presets pre-pass**: parse just `embeddedPresets` and hand them
   to the app's installer BEFORE the typed deserialize — the V1.4 param
   loader resolves params against the preset registry *during* `Project`
   deserialization (BUG-036 root cause). Pre-pass failure is a WARN, not an
   error (§9 E6).
4. Typed `serde_json` deserialize into `Project`.
5. `strip_unknown_effects` — unrecognized effect types silently deleted.
6. `on_after_deserialize` — rebuild caches; BPM synced from tempo-map beat 0,
   clamped 20–300.
7. V1 only: `migrate_duration_modes` (all layers forced to NoteOff).
8. `PathResolver::resolve_all` (§6).
9. Post-load validation (loader.rs:204): `validate` (WARN only),
   `validate_clips` (missing files, WARN only), `purge_orphaned_references`,
   stamp `clip.layer_id` from structural ownership, reconcile desynced
   generator identity, backfill legacy fork display names, and
   **`repair_overlapping_clips` — deletes the shorter clip of every
   overlapping pair**, logged at WARN (§9 E4).

App-side wrapper (`open_project_from_path`, project_io.rs:341) additionally:
snapshots the preset overlay before the pre-pass hook mutates it and **rolls
it back on load failure** (the live project would otherwise be stranded on the
failed candidate's presets); runs
`migrate_user_param_bindings_to_node_id` (renderer-side, needs bundled
graphs); surfaces load failure as a dialog. History-snapshot restore
(`load_project_snapshot`) runs the identical pipeline.

## 5. Migration chain

One linear chain in `migrate_if_needed`, gated on the file's `projectVersion`
(missing → assumed `"1.0.0"`, all steps run). Each step is a raw
`serde_json::Value` rewrite; several are deliberate no-ops where bidirectional
`Deserialize` impls absorb the shape change. Idempotency is per-step and
hand-argued in doc comments (each rewrite only matches the legacy shape).

| Step | What moved |
|---|---|
| 1.0 → 1.1 | Percussion fields nested into `percussionImport`; layer generator fields into `genParams` |
| 1.1 → 1.2 | Param addressing → stable `param_id` (no-op: dual Deserialize + post-load resolver) |
| 1.2 → 1.3 | Per-param exposure (no-op: handled by the 1.11 step uniformly) |
| 1.3 → 1.4 | Binding-storage unification (BUG-040 lived here: positional params of project-local generators dropped in a narrow window) |
| 1.4 → 1.5, 1.5 → 1.6 | (see migrate.rs:247–390 — graph/envelope home unification era) |
| 1.6 → 1.7 | WireframeDepthGraph → WireframeDepth rename |
| 1.7 → 1.8 | Repair generator `genParams` serialized through the effect path (`effectType` → `generatorType`; the "cleared generator" bug) |
| 1.8 → 1.9 | Audio-mod feature renames `brightness`→`centroid`, `liveliness`→`flux` |
| 1.9 → 1.10 | Send `source` → `{ layers: [...] }` shape |
| 1.10 → 1.11 | PARAM_STORAGE D4 wire migration (`param_storage_v14.rs`; "V1.4" is the param-wire shape's own name — the naming mismatch is documented at migrate.rs:73) |

`Project::default()` stamps `project_version: "1.11.0"` (project.rs:1467); the
field round-trips through load/save. `is_version_less_than` compares
numerically, three segments, missing segments = 0; non-numeric segments are
silently dropped by `filter_map` (a hypothetical `"1.4.0-beta"` parses as
`1.4`).

**There is no upper-bound check.** A file whose version is *newer* than the
build runs zero migrations and deserializes on serde's ignore-unknown-fields
default (§9 E1 — the worst edge in the crate).

## 6. Path resolution

`resolve_all` re-links video clip paths and layer video-folder paths (only
those two families). Chain per broken path: stored relative path from the
project dir → filename search across known dirs (project dir, its parent, the
parent's immediate subdirs, every layer video folder + parent), accepting a
name match only if file size matches the stored `file_size` (size check
skipped when stored size < 0, and entirely absent for directories).
Unresolved paths are counted and logged, never surfaced to the UI. On save,
`store_relative_paths` refreshes the relative forms. Audio clip paths and
other path-bearing fields are NOT visited (§9 E7).

## 7. History / recovery surface

- Every save that changes the hash journals the previous `project.json` into
  `history/` (gzip). Restore = `load_project_snapshot(archive, hash)` → full
  load pipeline → revert menu (`refresh_history_menu` after each autosave).
- Cap: 50 newest autosaves. Manual saves are never pruned — a
  manual-save-heavy project's archive grows monotonically (§9 E10).
- Dedup means "Save with no changes" is a no-op: no history entry, no
  timestamp bump.

## 8. Test coverage (what's proven vs. dark)

Proven: real-fixture loads (burn V4/V5, waypoints large, graphtestsv4
identity-reconcile), driver/envelope/mapping counts surviving migration +
roundtrip, history journal push/dedup/prune/unknown-hash, autosave state
machine (7 unit tests, time injected), snapshot-on-save capture/prune/
never-shadow-Saved, Liveschool size gate.

Dark: the Liveschool fixture is gitignored — `load_liveschool_live_show_v6`
and the size gate **silently pass when the fixture is absent**, so only
Peter's machine actually runs the real-scale proof (§9 E9). No test constructs
a future-version file, a corrupted ZIP, a truncated write, or a hash
collision. `save_project_v1` and preset/venue files have no roundtrip tests in
this crate.

## 9. Honest edges — latent-bug lenses, ranked

- **E1 — No forward-version guard (BUG-062, HIGH).** An older build opening a
  newer file: no migration step fires, serde silently drops every field it
  doesn't know, `strip_unknown_effects` deletes newer effect types, and the
  next save (or the 60s autosave, unprompted) writes the stripped project
  back — still stamped with the newer version string. Two builds ever
  coexisting (laptop + studio machine around a release) makes this a
  one-mistake show-file eater. Fix shape: compare `projectVersion` against
  the build's known ceiling before deserialize; refuse or open read-only
  with an explicit dialog.
- **E2 — Silent destructive load repairs (BUG-063, MED-HIGH).**
  `repair_overlapping_clips` deletes clips, `purge_orphaned_references`
  removes clips/mappings, `strip_unknown_effects` drops effects — all
  log-only. The user never learns the file changed; the next save persists
  the loss and the pre-repair state ages out of the 50-autosave history.
  Compounds E1 (the stripped load "repairs" clean). Fix shape: aggregate a
  load-repair report and surface any nonzero count as a dialog with a
  "keep original in history as a labeled snapshot" escape.
- **E3 — Temp file not fsynced before rename (BUG-064, MED).**
  `save_v2_archive` fsyncs the parent directory after rename but never
  `sync_all()` on the temp file itself; `zip.finish()` flushes userspace
  buffers only. On power loss (the gig scenario: venue power is not
  laptop-battery-guaranteed — see GIG_RESILIENCE_DESIGN) the rename can be
  durable while the file's data blocks aren't: a valid-looking `.manifold`
  containing garbage, having already replaced the good save. One-line fix.
- **E4 — 24-bit content hash for dedup and snapshot identity (BUG-065, LOW
  prob / HIGH cost).** A collision between the new save and `current_hash`
  skips the save ("No changes detected") while real changes are lost until
  the next edit re-fires autosave; a collision between two history entries
  makes restore return the wrong snapshot. 24 bits across a 50-entry history
  is ~7×10⁻⁵ per project lifetime — small, not show-file-grade small. Fix
  shape: full 64-bit prefix (8 hex chars is still short enough for entry
  names).
- **E5 — Two live save sources.** Manual save serializes the UI replica
  (`local_project`), autosave serializes the content-thread snapshot. Any
  divergence between the replica and the authoritative content-thread
  `Project` gets persisted by whichever path fires next — silently different
  files from the "same" state. Nothing currently proves replica convergence;
  UI_ARCHITECTURE_AUDIT describes the optimistic-apply design but no test
  compares the two serializations. Cheap probe: serialize both after a
  scripted edit session and diff. (Also: three save functions exist for two
  paths — the dead ported one should die.)
- **E6 — Pre-pass failure is WARN-only.** If the embedded-presets pre-pass
  fails to parse, the load continues and every project-local preset's params
  resolve to "no template" — the BUG-036 symptom returns, silently. The full
  deserialize usually fails too (same JSON), so the window is narrow: a file
  whose `embeddedPresets` section alone is malformed.
- **E7 — Path resolution covers video only.** Audio clip file paths (audio
  layers), imported percussion audio, and any other path-bearing fields get
  no re-link pass — moving a project folder relinks the videos and leaves
  audio broken.
- **E8 — Preset and venue files are unversioned.** `preset_file.rs` /
  `venue_file.rs` serialize current structs with no migration chain; any
  future shape change strands exported libraries. (Graph presets embedded in
  projects ride the project chain; standalone exports don't.)
- **E9 — The real-scale migration proof is machine-local.** Liveschool
  fixture tests skip-if-absent and the fixture is gitignored: CI and agent
  sessions prove migrations against small fixtures only. The
  `canonical-fixture-liveschool` memory calls it load-bearing; the test
  suite can't see it.
- **E10 — Manual-save history is unbounded.** Every manual save adds a gzip
  blob forever (only autos are pruned). A long-lived show file saved
  manually hundreds of times carries hundreds of snapshots; nothing surfaces
  or compacts this.
- **E11 — V1 detection is "not a ZIP".** Any unreadable/corrupt V2 archive
  falls through to the V1 JSON path and produces a confusing "Invalid UTF-8"
  or JSON error instead of "this archive is damaged (history may still be
  recoverable via manifest)". A corrupt-ZIP repair path (scan for readable
  history blobs) doesn't exist.

## 10. What's sound (so nobody re-litigates it)

Atomic temp+rename per save with per-writer unique temp names (concurrent
autosave + manual save can't collide); dedup-by-content before any write;
history journaling with bounded autosave growth *in blob count*; load failure
never disturbs the live project (overlay rollback, project untouched); the
autosave state machine is pure, time-injected, and covered by 7 tests; the
embedded-preset self-containment pass runs on all three save-shaped paths and
never shadows a deliberate `Saved` entry.
