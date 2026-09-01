# PROJECT_FOLDERS — project-folder semantics, Collect All and Save, breadcrumb inside the folder

**Status:** IN PROGRESS — P1–P4 landed 2026-09-02 (inventory, resolver extension, save semantics + `--resume`, Collect All and Save); live L4 verification by Peter owed.
**Prerequisites:** none — extends `manifold-io` and `manifold-app/src/project_io.rs` as they stand.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs) and section 6 (seam briefs) before starting any phase.

A project becomes a **folder** that owns its media, DAW-style. Peter, verbatim:
"I hate media libraries like Resolve, prefer the DAW UX." And the save semantics,
verbatim: "when you first save it, it makes a project unless it's already in a
project, then it's a version." Both are quoted where they decide something below.

Companion docs: `docs/PROJECT_IO_MAP.md` (current-state map — this design closes
its E7 gap), `docs/GIG_RESILIENCE_DESIGN.md` (breadcrumb + `--resume`), the beads
"Collect All and Save (media bundling)" (P2) and "Archive history pruning +
single-read save" (P3, intentionally NOT in this design — see Deferred).

## 1. Audit — what exists (verified 2026-09-01)

| Piece | Where | State |
|---|---|---|
| V2 archive writer (manifest, history journal, atomic rename) | `crates/manifold-io/src/archive.rs` (`save_v2_archive`) | Works. Keep — media does not go in here (D2). |
| Path resolution (relative store on save, re-link on load) | `crates/manifold-io/src/path_resolver.rs` (`PathResolver`) | Covers **video clips + layer video folders only** (`resolve_all`, path_resolver.rs:24). Audio/GLB/HDRI uncovered — PROJECT_IO_MAP.md section 9 (honest edges) E7. |
| Video clip path fields | `crates/manifold-core/src/video.rs:11` `file_path`, :13 `relative_file_path` | Covered. |
| Layer video folder | `crates/manifold-core/src/layer.rs:142,144` | Covered. |
| Audio clip path | `crates/manifold-core/src/clip.rs:20` `audio_file_path` | **Uncovered.** No relative form exists. |
| GLB model path | string param `"model_file"` on gltf-import nodes (`crates/manifold-renderer/src/node_graph/gltf_import/mod.rs:65`) | **Uncovered.** Lives in clip `string_params`, invisible to the io layer. |
| HDRI path | string param `"hdri_file"` (gltf_import/mod.rs:70) | **Uncovered.** Same. |
| Legacy/other path params | e.g. `"path"` in `assets/generator-presets/Skin.json:62` (a GLB fixture path) | **Uncovered.** Proves path params are not enumerable by a fixed id list — new presets add them freely. |
| String param defs | `crates/manifold-core/src/preset_definition_registry.rs:45` (`StringParamDef`) | Has `use_dropdown` flag as the flag-precedent (D5). |
| Save / Save As flow | `crates/manifold-app/src/project_io.rs:501` (`save_project_as`), `:430` (`save_project`) | rfd dialog, `.manifold` extension forced. No folder concept. |
| Breadcrumb sidecar | `crates/manifold-app/src/breadcrumb.rs:149` (`breadcrumb_path_for`) | Derived from project path — lands inside a project folder **automatically** (D7). |
| `--resume` | `crates/manifold-app/src/main.rs:291` (`parse_resume_arg`) | Requires the breadcrumb path typed by hand. |
| Autosave | `crates/manifold-app/src/autosave.rs` | Background thread, unchanged by this design. |
| Media/GLB import entry | `crates/manifold-app/src/blender_import.rs`, `ui_bridge/project.rs:667` | Files are referenced in place, never copied. |

Negative claims verified by search: no `FilePath`-typed param kind exists
(`rg 'ParamKind|WidgetKind' crates/manifold-core` — none); no path-bearing field other than the
table rows above matches
`rg 'file_path|video_path|audio_path|texture_path|mesh_path|source_path|import_path|folder_path'
crates/manifold-core`.

## 2. Decisions

**D1 — A project is a folder containing at least one `.manifold` file. No marker file, no index,
no database.** Detection is structural: `is_project_folder(dir)` = `dir` contains ≥1
`*.manifold`. "No media libraries like Resolve" (Peter). Passes the zero-new-systems test: no
new identity or addressing system — the folder IS the project.

**D2 — Media lives outside the archive, in `<Project>/Media/` subfolders by family**
(`Media/Video/`, `Media/Audio/`, `Media/Meshes/`, `Media/HDRIs/`).
Rejected: media as zip entries inside the archive — the archive is rewritten on every save (dedup →
journal → rename, `save_v2_archive` at crates/manifold-io/src/archive.rs), and pushing GBs
through that path makes every save pay media cost. Rejected:
embedding media in `project.json` — same problem squared. The folder is portable because
every collected path is stored relative (D4); copying the folder to the gig laptop works
because PathResolver already prefers the stored relative path
(path_resolver.rs:149 `try_resolve`).

**D3 — Save semantics, Ableton's, Peter's rule quoted:** "when you first save it, it makes
a project unless it's already in a project, then it's a version." (Peter, verbatim.)

- **First save** of an unsaved project (Save or Save As, no path yet): dialog picks location +
  name `MyShow` → create `MyShow/`, write `MyShow/MyShow.manifold` inside it.
- **Save** with an existing path: overwrite in place. Never re-wraps, never surprises.
- **Save As into a folder containing ≥1 `.manifold`:** a **version** — the file lands as a sibling
  inside that project folder, sharing its `Media/` by reference until Collect All and
  Save (D6).
- **Save As into a folder with no `.manifold`:** a **new project** — create `Name/Name.manifold`
  inside `Name/`. You can never Save As into a loose file; that's the point.
- **Name collision:** Save As to a name that already exists in the target folder gets
  the existing overwrite-confirm; versions are the user's choice of name (`MyShow
  v2.manifold`), never auto-suffixed.

The decision lives in ONE pure function `resolve_save_target` (section 3) so the rfd dialog is a
dumb shell around it and the rules are unit-testable.

**D4 — One asset-path inventory, one consumer shape.** New `manifold-io/src/collect.rs`:

```rust
pub enum AssetKind { Video, Audio, Mesh, Hdri }
pub struct AssetRef { pub kind: AssetKind, pub path: PathBuf, pub target: AssetTarget }
pub fn collect_asset_paths(project: &Project) -> Vec<AssetRef>;
```

Families: video library clips, audio clips (`clip.rs:20`), layer video folders, and every
string param flagged `is_file_path` on any generator/effect instance (D5).
`collect_asset_paths` is THE enumeration: Path Resolver (P2), Collect All and Save (P4),
and any future missing-file report all read it. No second list anywhere.

**Consequences, stated honestly:** the audio clip path has no relative form today
(`audio_file_path: String` only). D4 adds `relative_audio_file_path: Option<String>` beside it —
an additive-optional serialized field: old builds ignore it, new builds fill it on save.
No migration step needed.

**D5 — Path params are discoverable data, not a hardcoded id list.** Add `is_file_path: bool`
to `StringParamDef` (preset_definition_registry.rs:45, shaped like `use_dropdown` at :53)
and a `"file_path": true` marker on string params in graph-preset JSON; flag `model_file`,
`hdri_file`, and Skin's `"path"`. Enumeration walks `string_param_defs` per instance and picks
up flagged params — a preset author marking a new path param gets collection for free.
Rejected: a static `["model_file", "hdri_file", "path"]` list in the io layer — the third entry
already proves the list rots on arrival.

**D5a — Def-default-only path params materialize a per-clip override on re-link/collect (decided 2026-09-02, k3 (lead), from the P2 lane's write-back-home gap).** The lane found it mid-P2: when a flagged path param's value lives only in the preset-def default (no per-clip override), `resolve_all`/collect has no `string_params` entry to write into, so a re-link silently has no home. The rule: when re-linking or collecting such a param, the write-back **materializes** a per-clip `string_params` entry with the resolved path — never writes the preset def's `default_value` itself (defs are shared across clips and presets; mutating them would silently move other clips' media). Materialized overrides stay canonical: same precedence rules, no new home.

**D6 — Collect All and Save is copy-only, then re-point, then save.** For each external
`AssetRef` outside the project folder: copy (never move, never delete the source) into the
right `Media/` family folder, dedup identical sources by content hash (SHA-256 full), then
re-point the field to the relative form, then run the normal save path. Runs off the UI
thread with a progress dialog; on failure mid-way, copied files stay and paths that already
re-pointed stay re-pointed — every intermediate state is loadable, so there is no
half-broken-project state by construction.

**D7 — The breadcrumb rides along for free.** `breadcrumb_path_for` (breadcrumb.rs:149)
appends to the project path, so inside a project folder the breadcrumb is already inside.
No placement change needed. The only real change is in `--resume`: with **no path
argument** it resolves the breadcrumb of the last-opened project from
`LAST_OPENED_PROJECT_PREF_KEY` (project_io.rs:546) — zero new state, the crash-rejoin
path no longer needs the path typed by hand mid-panic.

**D8 — Existing projects are never force-migrated.** Loose `.manifold` files open and
save exactly as today (PROJECT_IO_MAP.md section 9 (honest edges) — do not disturb
what's sound). Wrapping into a folder happens only through D3/D6. No startup reorg of
user files, no background moves.

## 3. Design body

### 3.1 Data model + seams

All thread-residency answers are house answers: mutation through `EditingService`;
serialization through the existing save paths. The only new code is in `manifold-io`
(inventory + collect + resolve), `manifold-core` (one flag + one field), and the app seam
(`project_io.rs`).

New/changed load-bearing types:

```rust
// manifold-io/src/collect.rs (new)
pub enum AssetKind { Video, Audio, Mesh, Hdri }
pub struct AssetRef { /* kind, path, target */ }
pub fn collect_asset_paths(project: &Project) -> Vec<AssetRef>;           // D4

// manifold-core/src/preset_definition_registry.rs
pub struct StringParamDef { /* ... existing fields ..., */ pub is_file_path: bool }
```

## 3.2 Save-target resolution (D3), committed signature

```rust
// manifold-io/src/project_folder.rs (new)
pub enum SaveTarget {
    /// dir contains ≥1 .manifold → sibling version.
    Version { dir: PathBuf },
    /// dir contains no .manifold → create `dir/Stem/Stem.manifold`.
    NewProject { folder: PathBuf, file: PathBuf },
}
pub fn resolve_save_target(target_dir: &Path, file_stem: &str) -> SaveTarget;
```

The decision lives in ONE pure function so the rfd dialog is a dumb shell and the rules are unit-testable.

### 3.3 App seam

`save_project_as` (project_io.rs:501) changes one decision site: after the dialog returns a
path, call `resolve_save_target(target_dir, stem)` — the dialog, extension forcing, and the
`mark_clean` / `push_recent_project` bookkeeping are unchanged.

**The plausible-wrong architecture, forbidden by name:**
- No manifest/index/marker inside the project folder (D1, zero-new-systems; the
  `.manifold` file IS the marker).
- No `Media/` skeleton on first save — created only on Collect (D6), otherwise empty
  projects carry empty folders forever.
- No breadcrumb pruning (D7 defers cleanup to the user; breadcrumb churn is inert bytes).
- No media inside the archive (D2).

## 4. Phasing

**P1 — Asset-path inventory (`collect.rs`, D4+D5 data layer, no UI).**
Entry: `rg 'is_file_path' crates/manifold-core` zero hits today; P1 adds the flag.
Deliverables: `manifold-io/src/collect.rs` with `collect_asset_paths` + tests for each
family (video, audio, mesh, hdri).
Gate: `cargo test -p manifold-io collect` passes; `rg 'is_file_path' crates/manifold-core`
returns hits; `rg '"file_path": true' crates/manifold-renderer/assets` returns hits for
model_file / hdri_file / path (three JSON presets).

**P2 — PathResolver extension (E7 closure).** Audio clips, `model_file`, `hdri_file`, Skin `"path"` re-link
through the same chain as video. Gate: existing Liveschool roundtrip tests stay green;
new fixture test: a temp-dir project with a missing audio file + a missing GLB path re-links
when the file is moved into `Media/` — round-trip passes.

**P3 — Save semantics + breadcrumb.** Folder creation on first save; Save As
version/new-project branching; `--resume` no-arg auto-discovery.
Gate: unit tests for `resolve_save_target`; integration test asserting the folder
layout on Save As into an empty dir.

**P3 — Save semantics + breadcrumb.** Folder creation on first save; Save As
version/new-project branching; `--resume` no-arg auto-discovery.
Gate: unit tests for `resolve_save_target`; integration test asserting the folder
layout on Save As into an empty dir.

**P3 — Save semantics + breadcrumb.** Folder creation on first save; Save As
version/new-project branching; `--resume` no-arg auto-discovery.
Gate: unit tests for `resolve_save_target`; integration test asserting the folder
layout on Save As into an empty dir.
Demo: L1 — `cargo nextest run -p manifold-io -p manifold-app save_target` (the rfd dialog itself is not scriptable;
the pure decision function is the tested surface). Forbidden moves: auto-suffixing version names; adding a marker file; the rfd
dialog calling resolve_save_target from more than one call site (the seam is exactly one call in `save_project_as`).
Round-trip gate: save-as → reopen from the new folder → assert `project.last_saved_path` + breadcrumb path inside the folder.

**P4 — Collect All and Save (the user-facing command).** Copy engine, menu entry,
progress UI, dedup. Gate: round-trip test with mixed path families (video + audio + a GLB in `string_params`); file count,
byte count, and relative-path assertions pass; source-file hashes unchanged (copy-only invariant).
Demo: L1 — `cargo test -p manifold-io collect` round-trip: synthetic project referencing fixture
files → collect → assert on-disk layout + paths relative + reload through the full load pipeline →
all references resolve.
Performer-gesture line (DESIGN_DOC_STANDARD.md section 5 (phase briefs)):
P4's gesture is "Collect All and Save on a show with footage in Downloads → media lands in `Media/`, paths
relative, project opens on another machine (temp dir) with zero missing files.
Test scope: P1–P4 are io+core+app seams — run `cargo nextest run -p manifold-io`, `-p manifold-core`, and
`-p manifold-app` (the save seam). Clippy on the same crates before every commit.

## 5. Invariants & enforcement

- A project folder is detected by `.manifold` presence only, never a marker file —
  enforcement: unit test `folder_with_manifold_is_project` (P3), negative gate
  `rg '"manifest"' crates/manifold-io/src/project_folder.rs` → zero hits (P3).
- Collect never moves or deletes the source — enforcement: unit test `collect_never_moves_source` (P4): source
  file hash before == after.
- The breadcrumb sits next to its project file — enforcement: existing `breadcrumb_path_for` test stays
  untouched; the app-side breadcrumb integration test from `breadcrumb.rs` stays green (P3).
- No path param is enumerated by a hardcoded id list — enforcement: negative gate
  `rg 'model_file|hdri_file' crates/manifold-io/src` → zero hits (P1); enumeration is flag-driven only.
- `resolve_save_target` is pure (no I/O beyond reading the target dir) — enforcement: the P3
  integration test constructs projects entirely in temp dirs.

## 6. Decided — do not reopen

1. Media outside the archive, in `Media/` subfolders (D2).
2. No marker file / no database / no index of projects (D1).
3. Save As into a project folder = version; empty dir = new project (D3).
4. Collect is copy-only, never moves sources (D6).
5. Save-target rules live in one pure function (`resolve_save_target`), not scattered through dialog code (D3).

## 7. Deferred

- **History pruning + single-read save (the 59MB problem)** — bead "Archive history pruning + single-read save"
  (P3 bead). Revive trigger: after this design lands, so both don't fight over `archive.rs` in the same wave.
- **Archive-level forward-version guard** (PROJECT_IO_MAP.md section 9 (honest edges), E1) — out of scope; logged in beads.
- **Resume UI (a picker for which project to resume)** — deferred until Peter asks; only the no-arg
  `--resume` path ships (D7).
- **Breadcrumb pruning/cleanup** — deferred; inert bytes, Peter's call.