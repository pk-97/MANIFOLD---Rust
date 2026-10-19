# Simplification Census

<!-- index: How to find dead code, unused content and over-engineering with tool-backed proof, who runs which sweep, and the ready-to-send sweep briefs. -->

**Status:** ACTIVE · the method for BUG-yn979 (Simplification pass). Results go to `docs/evidence/simplification_census_2026-10/`. Obsolete when: the census report is acted on, unless the census becomes a recurring sweep.

## 1. What this is for

The week's goal is that landings remove more than they add. The census finds what can go and proves it, so deleting lanes and Peter can trust a list made mostly by cheap models.

Trust comes from tools, not from the model. A "nothing uses X" claim is only as good as the search that would have found a use. Every record carries those searches and what they printed. A record without them is not a candidate.

Three things make "unused" hard in this codebase:

- Many things are used by string, not by Rust name: node and preset ids in preset JSON, graph JSON and saved projects; serde field and variant spellings; registry entries; UI flow scripts; script names in hooks and docs.
- `pub` hides dead code from the compiler. rustc flags unused private items, and everything unused in the binary crate (manifold-app), but never an unused `pub` item in a library crate. Most dead code in the library crates is `pub`.
- Saved projects hold names and old formats that must keep loading (section 2). They are never a measure of use: Peter may not have used a feature yet.

The census nominates. It does not delete. A deleting lane later removes a reviewed batch and lets `cargo check` confirm it, including the cascade of items that were only used by what it removed.

Out of scope: `plugins/vendor/` (third-party code), the archived Unity repo, UI design fixes that are not slop (they belong to the UX pass; the census records only the code roots of walkthrough items), and anything that needs a test run or the GPU.

## 2. Saved projects: a loading check, never a usage check

Whether Peter's projects use a node, preset or setting says nothing about whether it should exist. The corpus answers one question only: would deleting this break loading a saved project? A saved serde name, node id or old format a project carries must keep loading (or get a migration). Absence from projects is never evidence for a candidate. Online-only Dropbox projects are not downloaded.

| Source | Where | Count | Read? |
|---|---|---|---|
| Bundled presets | `crates/manifold-nodes/assets/{effect,generator,scene-modifier,reference}-presets/` | 40 effect, 55 generator, 18 scene modifier, 1 reference | yes |
| User presets | `~/Library/Application Support/MANIFOLD/presets/` | the folder does not exist | n/a |
| App prefs | `~/Library/Application Support/MANIFOLD/prefs.json` | 1 | yes |
| Test fixtures | `tests/fixtures/*.manifold`, `tests/fixtures/rt/` | 17, including Liveschool Live Show V6 LEDS (the 20-minute show, pre-3D) | yes |
| Downloads | `~/Downloads/*.manifold` | 143: mostly test and dev projects, plus older song projects saved as plain JSON | yes |
| Dropbox, local | `~/Dropbox/Videos/LATENT SPACE - Marketing Content/MANIFOLD Projects/` | 22: the Interim EP projects (Murmur, Mirage Physics, Hung Up, Right Where I Need You, Corrosion V5, Head Noise), Liveschool V6 AUDIO and LEDS, STAGNATE, WAYPOINTS, PATTERN | yes |
| Dropbox, online-only | same folder | 67 files, 2.1 GB: the Corrosion music video versions, older Liveschool and Burn versions, DIMENSION and more | **no**: opening one makes Dropbox download it, which needs Peter's yes |

`build_corpus.sh` (section 5) dumps every readable project's raw `project.json` through `project_tool json` (prebuilt in `target/debug`, read-only, reads both the ZIP and the plain-JSON format) into `/tmp/simplification_census/corpus/projects/NNN_name.json`. `index.tsv` maps each number to its source path and date; `skipped.tsv` lists the online-only files. It checks the `dataless` flag with `stat` and never opens an online-only file. The 2026-10-19 build: 182 projects, 333 MB, 67 skipped.

The older shows are mostly online-only, so the corpus under-counts old data shapes. That matters only for legacy paths and migrations (section 3.8), where the count informs Peter's call on old formats.

Two traps when searching the corpus:

- A project embeds its full graphs. Node ids appear as `"typeId": "node.x"`, generator presets as `"generatorType": "X"`. Search with the quotes, so `"node.add"` does not match `"node.add_burst"`.
- Old ids live on. 44 of the 116 old ids in `TYPE_ID_MIGRATIONS` (`crates/manifold-core/src/type_id_migration.rs`) still appear in projects, and the old shows still carry generator types such as `OscilloscopeXY`, `FluidSimulation` and `BasicShapesSnap`. A rename entry stays live while any project holds its old id.

## 3. Categories

Each category gives the claim, the oracle, the extra string searches, the known traps, and which model runs it. Haiku runs given commands. Sonnet does directed collation: it runs the listed commands per candidate and fills the record. Opus judges. The raw files named here are written by stage 0 (section 5).

### 3.1 Dead pub items

**Claim:** a `pub` fn, struct, enum, trait, type, const or static that nothing uses outside its own definition.

**Oracle:** `census_refs.py <crate> --corpus <corpus>/projects` lists every unrestricted `pub` item with ast-grep and counts whole-word hits across the whole repo, hidden directories included, sorted into product code, tests, tooling, assets, other files and mentions (docs, beads). DEAD means no hit outside the definition except mentions. Per DEAD row the sweep re-runs and records:

```
rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME "/Users/peterkiemann/MANIFOLD - Rust"
rg -l -w -F NAME /tmp/simplification_census/corpus/projects
```

**Also search:** serde attributes that name the item as a string (`default = "…"`, `with = "…"`, `serialize_with`); names built by macros (`paste`, `stringify!`, `format!("{}_…")`); `#[no_mangle]` and `extern "C"` names called from `.m`, `.swift` or `.h` files (manifold-native, the analyzer plugin); `pub use … as OTHER` re-exports (search the other name too); `build.rs` and `include!`.

**Traps:**
- Cascades. An item used only by dead items reads CRATE-ONLY or USED. List the cluster when it is obvious; the deleting lane's `cargo check` finds the rest.
- `census_refs.py` counts a type's own `impl` blocks as uses, so a whole dead module can read CRATE-ONLY. Follow every DEAD row into its module and check whether the module's types are used only by their own impls and each other; if so, the module is one row.
- The screen also counts comment and doc-comment mentions as uses. Recount low-use rows with comment-only hits excluded.
- manifold-app is a binary, so the compiler already flags its dead items. App DEAD rows are only the ones under `#[allow(dead_code)]` (section 3.3).
- Names that are common words (new, get, title, legacy) never read DEAD, because text search over-counts. That is the safe direction. Do not chase them in this pass.
- Trait methods carry no `pub` and are not listed. Traits are judged in section 3.10.

**Tier:** Sonnet. Grade A only when every search is empty.

### 3.2 Product code only tests use

**Claim:** non-test code whose only callers are tests, together with the tests that exist only for it.

**Oracle:** TEST-ONLY rows from `census_refs.py`. Then `rg -n -w NAME` lists the test callers; read each test to see whether it tests anything else.

**Traps:** testkit modules, `test_support.rs` and items under `#[cfg(test)]` are test infrastructure on purpose and are not listed. Probes and tools in `src/bin/` and `examples/` count as real use. The commonest pattern is a function whose real caller was deleted and whose own unit test kept it alive (section 9).

**Action:** delete the item and its tests together. **Tier:** Sonnet.

### 3.3 Dead-code suppressions

**Claim:** a `#[allow(dead_code)]`, `#[allow(unused…)]` or `#[expect(dead_code)]` whose stated condition has passed or never will.

**Oracle:** `raw/suppressions.txt` (each with three lines of context). For each: the reason comment; its age (`git blame -L N,N <file>`); whether the condition it names has happened (`bd show`, or `rg` for the named caller). The deleting lane removes the attribute and runs `cargo check -p <crate> --all-targets`; the compiler then names exactly what is dead.

**Traps:** `unused_assignments` and `unused_variables` in tests are not dead code. A suppressed field in a GPU uniform struct may be read by the shader; check the WGSL.

2026-10-19 counts in `crates/`: 35 `allow(dead_code)`, 5 `allow(unused…)`, 4 `expect(dead_code)`. Two in `crates/manifold-app/src/project_io.rs` read "Ported service method; its Application wrapper isn't wired yet" and "Ported drop handler; its Application wrapper isn't wired yet".

**Tier:** Sonnet. Port leftovers with no live bead go to the patterns sweep.

### 3.4 Unused dependencies

**Oracle:** `cargo machete "/Users/peterkiemann/MANIFOLD - Rust"` (`raw/machete.txt`). cargo-udeps is not installed and the toolchain is stable only. Per flagged dependency, with dashes turned into underscores:

```
rg -n '\bDEP::|use DEP\b|extern crate DEP' crates/<crate>
rg -n 'dep:DEP|"DEP/' crates/<crate>/Cargo.toml
```

**Traps:** a dependency used only inside another crate's macro expansion (serde derives, `inventory::submit!`, `bytemuck` derives) looks unused to machete; a dependency needed only by a feature (`dep:` in `[features]`). The final proof is the deleting lane's remove plus `cargo check -p <crate> --all-targets --all-features`.

2026-10-19 run: manifold-nodes-water (arrayvec, glam, half, image, log, serde), manifold-node-engine (manifold-playback, png), manifold-nodes-scene (png), manifold-spectral (log), manifold-water-whitewater (log); outside the workspace, tv-led-mirror (objc2-core-video) and the analyzer plugin (nih_plug_egui). Spot check: nodes-water's source uses none of its six; glam is optional behind `testkit` and unused there too. The water crates' manifests already have an owner: `bd show BUG-hkbdp.6.5` (Water Cargo hygiene and retirement of the T2 move scaffolding).

**Tier:** Sonnet.

### 3.5 Nodes, primitives and presets nothing uses

**Claim:** a registered node type, or a bundled effect, generator or scene-modifier preset, that is a leftover: a one-off a newer node replaced, a dev or probe artifact, or something no preset and no code uses and no family Peter is building needs. Project use is not a signal either way; a project hit only means deleting it needs a load path (section 2).

**Oracle:** `raw/node_usage.tsv` (`node_usage.sh`): for every type id and preset id in `docs/node_catalog.json` and the scene-modifier folder, the number of files holding the quoted id in bundled presets, corpus projects, Rust product code and Rust tests. Per candidate:

```
rg --hidden -g '!.git/' -n -F '"ID"' "/Users/peterkiemann/MANIFOLD - Rust"
rg -l -F '"ID"' /tmp/simplification_census/corpus/projects
```

plus the primitive's `*_TYPE_ID` const by name when it has one, and Rust that builds graphs in code (importers, preset builders such as `crates/manifold-nodes-water/src/presets/gpu_flip.rs`).

**Traps:**
- A preset row always counts its own file. The projects column is not a signal of use.
- The Rust product-code count includes the defining file, and a node's inline tests sit in that file.
- Palette atoms are authoring vocabulary. A node with zero uses is still something Peter can drop into a graph, so a node in a family he is building (the 3D geometry atoms) is `ask`; `delete` is for one-offs a newer node replaced.
- The LED generators (17 of 18 in no project) serve the LED rig: `ask`.

2026-10-19 screen: of 390 nodes, 90 are in no preset and no project; 14 of those have no Rust reference outside their own file: `node.copy_positions`, `node.cut_out_box`, `node.displace_copies`, `node.extrude_curve`, `node.multiply_vector_fields`, `node.peak`, `node.reflect_array`, `node.remove_drift_3d`, `node.revolve_curve`, `node.sample_triangle_grid`, `node.scatter_on_mesh`, `node.spawn_from_mesh`, `node.tube_from_path`, `node.wave_field_3d`.

**Tier:** Opus, working from the stage 0 table.

### 3.6 Dev and test scenes in user-facing browsers

**Claim:** a bundled preset the browser lists (`"available": true`) that exists for tests, probes or engine comparisons.

**Oracle:** `node_usage.tsv` rows with projects 0 and Rust references above 0; names that name an engine or a test setup ("GPU FLIP", "Live GPU", "Particle View", "Test"); `rg -l -F '"<id>"' crates` to see who loads each one.

The walkthrough's six water entries are bundled generator presets: WaterDamBreakGpuFlip, WaterDamBreakMatter, WaterDamBreakParticles, WaterFloatingBoxMatter, WaterSeaWallGpuFlip and WaterStillPoolMatter. Five appear in no project; tests and probes load four of them from the bundled folder (2 to 21 Rust files each). The code root is that test scenes live in the user's preset folder.

**Action:** move a test-only scene into test fixtures; keep anything a project uses. No new hiding flag. **Tier:** Opus.

### 3.7 Settings that should not reach the user

**Claim:** a node param or project setting that duplicates another (the World node's own sun beside the Sun light), applies only to another mode or type (cone angles on a sun), is an engine internal with no performer meaning (shadow bias, a "Legacy" falloff), or does nothing.

**Oracle:** code reading, the walkthrough (`docs/evidence/ux_walkthrough_2026-10-19/README.md`) and the node catalog. How often projects change a value is not evidence; the param tables in `raw/` are not used for this.

**Traps:**
- Material params set by glTF import (texture filters, UV transforms) come from the imported file; the importer still needs them.
- A cut param keeps its value as a constant, and a saved project that stores it must still load (section 2).

**Action:** `cut-param` (remove it and keep its value as a constant) or `ask`. Never "hide behind a toggle". **Tier:** Opus.

### 3.8 Silent fallbacks, legacy paths, migrations and their tests

**Claims:** (a) a fallback on a path that should never fail (`unwrap_or_default`, `unwrap_or_else(|_| …)`, `.ok()?` on internal state) that turns a bug into a silent wrong value; (b) a legacy, compat or fallback branch for input no current file carries; (c) a migration for a format no file in the wild has; (d) tests that exist only to cover (a) to (c).

**Oracle:** `raw/fallback_files.txt` (hits per file), `raw/fallback_fns.txt` (functions named for fallback, legacy, compat or migration), `raw/project_versions.txt`, `raw/type_id_migrations.tsv` (corpus files per old id). For a legacy branch, name the input shape it handles, then search the corpus and the bundled presets for that shape with `jq` or `rg`.

**Traps:**
- "legacy" in a name proves nothing. `PhysicsSettings::legacy()` serves the 168 of 182 projects that have no physics block (section 9).
- Every project format from 1.0.0 to 1.21.0 is in the corpus, and 68 files are 1.0.0, the show projects among them. No migration step can go on corpus evidence. Dropping old formats (convert the old shows once, then delete the steps below that version) is Peter's call.
- A fallback that keeps the show running on a display or audio path is right for a live instrument (section 9, `StackText::as_str`). The slop is fallbacks on load, model and edit paths.
- The online-only Dropbox projects are the oldest; corpus absence is weakest here.

**Tier:** Opus.

### 3.9 Modes, flags and knobs with one value

**Claim:** a const bool switch, an environment knob, a cargo feature or an enum mode that only ever takes one value.

**Oracle:** `raw/const_bools.txt`, `raw/env_knobs.txt` (every environment variable the crates read, with how many places read it), `raw/features.tsv` (cargo features with the number of places that name them; the declaration plus one use is a candidate), and for enum modes the corpus distribution of the stored value.

**Traps:** gates enable features from `scripts/` and `.config/`, which `features.tsv` searches. `scripts/feature_matrix.py` builds every non-default feature, which keeps dead features compiling, not used. Debug knobs named for a bug (`MANIFOLD_BUG060_TRACE`) are candidates once that bug is closed. Knobs Peter sets in his own launch commands are not in the repo: `ask`.

2026-10-19 screen: two kill switches were off, `RELIGHT_FEATURE_ENABLED` (depth relight, "3D Shading") and `SHADOWS_ENABLED` (UI shadows). Both features were deleted whole on 2026-10-20. The crates read 135 environment variables.

**Tier:** Sonnet lists them per crate group; Opus judges switches and modes.

### 3.10 Traits and registries with no or one implementor

**Oracle:** `raw/traits.tsv` (implementors, `dyn` uses, bounds, name, file) and `raw/registries.tsv` (each `inventory::collect!` type with its `inventory::submit!` count, comment lines excluded). Per candidate: `rg -n -w TRAIT` over all files, and macro-generated impls (`impl $trait for` inside `macro_rules!`).

**Traps:**
- A one-registrant registry or one-implementor trait often inverts a dependency across a crate boundary: a lower crate declares a hook and a higher crate fills it. `docs/RENDERER_CRATE_SPLIT_DESIGN.md` D4 (no-facade rule) and D12 (layering test) decide whether that seam must stay. Check before calling it slop.
- A test mock as the second implementor does not make a trait earn its place in product code.

2026-10-19 screen: no implementors at all for PlaybackNotifier, LayerLifecycleCallbacks, SelectionRegionTarget, EditingHost (one function takes `&dyn EditingHost`, so it can never be called) and PostProcessEffect. The compositor's `plugin_warmups` list is filled from the PluginPrewarm registry, which has no submissions, so it is always empty, and a test checks registrations over an empty list. Three more registries have no submissions: EffectAliasMetadata, EffectValueAliasMetadata, ParamDoc.

**Tier:** Opus.

### 3.11 Duplicate implementations of one concept

**Oracle:** `raw/free_fn_dupes.tsv` (free functions whose name is defined in three or more crates), then concept searches for each suspect across crates.

**Traps:** test helpers repeated per test module (`fixture`, `run`, `settle`) are local and cheap. Solver crates may keep their own math on purpose so they can ship on their own (`docs/RENDERER_CRATE_SPLIT_DESIGN.md`).

2026-10-19 screen: `dot` is defined in seven crates, `cross` in six, `normalize`, `length` and `sub` in five, `scale` in four (physics, fluids, whitewater, nodes-image, nodes-scene, node-engine, ui, ui-paint). Some may be inside test modules; the screen does not separate inline test modules for this list.

**Action:** merge into one owner where the layering allows. **Tier:** Opus.

### 3.12 Scripts, verbs, hooks, docs, tools and repo leftovers nothing runs or reads

**Oracle:**
- Scripts: `raw/scripts.tsv` (real Bash runs in the last 30 days of session transcripts, executable bit, static references). A real run is a transcript `"command":"…scripts/<file>` or `"command":"…dev.py <verb>`; a mention is not a run. Also `scripts/dev.py <verb> --help` for every verb: a verb that crashes is not being run.
- Hooks: `.claude/telemetry/hook-fires.jsonl`, the file `scripts/hook_census.py` reads. hook-census does not run today: the script lacks its executable bit, like 12 other scripts in `scripts/`, so `scripts/dev.py hook-census` crashes. Read the telemetry with `jq`, one command per step; the shell guard denies awk or sed pipelines that name `.claude` paths.
- Docs: `.claude/hooks/design_status.py --lifecycle-check` covers design docs and passes today. `raw/docs.tsv` gives Read calls in 30 days and inbound references by filename for every top-level doc. Read counts undercount (cat, rg and context nudges also read docs); inbound references decide.
- `tools/`: `audio_analysis/eval` (366 files), `audio_analysis/lab` (106), `rt_prototype`, `mtl4fx_repro`, `segmentation`. Imports, runs, and whether the lab replaced an eval script.
- Repo leftovers: tracked build output (`libstdin.rlib` at the repo root), stray directories (`crates/manifold-renderer` holds only an untracked `target/`), side projects (`tv-led-mirror`).

**Traps:** the nightly launchd job runs `trunk_health.py` (`~/Library/LaunchAgents/com.manifold.trunk-health.plist`). Peter runs some tools by hand (live-ui, ableton-patch, claude-pane): those are `ask`. Transcripts older than about 30 days may be cleaned up.

**Tier:** Sonnet.

### 3.13 Cut from the list

- cargo-udeps: not installed and needs nightly. machete plus remove-and-check covers it.
- `pub` items used only inside their own crate (CRATE-ONLY): narrowing visibility removes no lines.
- Generics with one instantiation: rare here and low yield; the patterns sweep notes any it meets.

## 4. The candidate record

Each sweep writes two files in `docs/evidence/simplification_census_2026-10/`:

- `<sweep>.md`: a header, then one table row per candidate.
- `<sweep>.proof.md`: one `### <id>` section per candidate with every command it ran and a one-line summary of each output.

Header: sweep name, model, main commit (from `raw/main_commit.txt`), raw files used, and what the sweep did not cover.

| id | location | category | proof | risk | grade | action | lines |
|---|---|---|---|---|---|---|---|
| engine-001 | `crates/manifold-foundation/src/uv_transform.rs` | 3.2 | 2 fns and a struct only its own tests use; 0 hits in other crates, strings, corpus; caller deleted in 61b52dcef | none | A | delete module and tests | 111 |

- **id:** `<sweep>-NNN`.
- **location:** `path:line` for an item, a path for a file, the id for content (`node.peak`, preset `WaterDamBreakMatter`).
- **category:** the section 3 number.
- **proof:** one line naming what was searched and what came back.
- **risk:** `stage` (a show can reach it: UI, presets, project load, playback, render), `prep` (authoring, import, export), `dev` (tests, probes, scripts, docs), `none`.
- **grade:** A (every search empty, strings and corpus included), B (code searches empty, one named gap: dynamic dispatch, a macro, corpus coverage), C (needs Peter).
- **action:** `delete`, `merge` (into what), `inline`, `simplify` (one line how), `cut-param`, `move-to-fixtures`, `ask`. The list is closed: no new flags, toggles or hiding modes.
- **lines:** lines removed, its tests included; `~` when estimated. For content, files removed.

Rules:

- A candidate without its proof section is not a candidate.
- Anything at `stage` risk is grade B at best; Peter confirms it in the report.
- Open beads already name some of this work (61 on 2026-10-19, listed in `raw/open_cleanup_beads.txt`; `census_refs.py` counts a `.beads/issues.jsonl` hit as a mention). When a bead names the item, cite it in the proof column and keep its fix shape; never file a second finding for it. Example: the playback DEAD rows `sync_clip_loop_state`, `track_clip_start` and `finalize_clip` are already in the playback audit hygiene bead (`bd show BUG-mmct0.12`).
- Sweeps log nothing to beads; the lead does after review.

## 5. Stages and the sweep split

**Tools.** `/tmp/simplification_census/tools/` holds the census tools: `run_screens.sh` (the stage 0 driver), `build_corpus.sh`, `census_refs.py` with `pub_items.yml`, `node_usage.sh`, `param_moves.awk`, `free_fns.yml`. They are one-offs for this census and live outside the repo; their full source is in `docs/evidence/simplification_census_2026-10/TOOLS.md`, so they survive a reboot. Nothing in `scripts/` did this job. Reused: `project_tool json` for reading projects, `docs/node_catalog.json` for ids and defaults, `design_status.py` for design docs, the hook telemetry `hook_census.py` reads, `cargo machete`.

**Stage 0: screens.** One static command, CPU only (rg, ast-grep, jq, cargo machete), four threads:

```
/tmp/simplification_census/tools/run_screens.sh
```

It writes `/tmp/simplification_census/corpus/` and `/tmp/simplification_census/raw/`. The pub-reference scan dominates (about 15 minutes for the workspace); everything else takes about 2. The 2026-10-19 screens at main 37a9c180a are still in `/tmp/simplification_census/raw/`. Re-run it when main moves past a landing that touches census crates; every record carries the commit it was screened at.

**Stage 1: sweeps.** Ten jobs. Sizes are from the 2026-10-19 screens: DEAD plus TEST-ONLY rows, then suppressions.

| Sweep | Model | Scope | Categories | Size |
|---|---|---|---|---|
| `engine-model` | Sonnet | foundation, core, editing, io | 3.1 to 3.4, 3.9 lists | 96 rows (core 70), no suppressions |
| `engine-runtime` | Sonnet | playback, compositor, audio, spectral, media, recording, profiler, native, led | 3.1 to 3.4, 3.9 lists | 73 rows (playback 60), 2 suppressions, 1 machete hit |
| `graph` | Sonnet | node-engine, nodes, nodes-image, nodes-scene | 3.1 to 3.4, 3.9 lists | 71 rows (node-engine 54), 6 suppressions, 3 machete hits |
| `water` | Sonnet | physics, fluids, nodes-water, water-rigid, water-liquid, water-gpu-flip, water-gpu-mpm, water-whitewater, water-surface | 3.1 to 3.4, 3.9 lists | 83 rows (gpu-flip 24, fluids 22, nodes-water 17), 7 machete hits |
| `app-ui` | Sonnet | app, ui, ui-paint, gpu | 3.1 to 3.4, 3.9 lists | 312 rows (ui 289, of which 201 are constants in `color.rs` and `widget_layout.rs`), 26 suppressions (app 17, gpu 7, ui 2) |
| `tooling` | Sonnet | scripts/, dev.py verbs, .claude/hooks, docs/, repo root, plugins/ (not vendor) | 3.12 | 40 of 64 scripts with no direct run in 30 days; 41 of 243 top-level docs with no read and at most 2 inbound references; 62 archived docs; hooks |
| `tools-lab` | Sonnet | tools/ | 3.12 | 389 Python files, most in audio_analysis/eval |
| `content` | Opus | nodes, presets, scene modifiers, params, project settings, the walkthrough's code roots | 3.5, 3.6, 3.7 | 90 nodes and 53 presets (12 effects, 29 generators, 12 scene modifiers) in no project; 842 stored params |
| `fallbacks` | Opus | fallbacks, legacy and compat paths, migrations, their tests | 3.8 | 355 fallback-named functions, 116 rename entries, 10 migration files |
| `patterns` | Opus | traits, registries, duplicates, switches, modes, port leftovers | 3.9 judgment, 3.10, 3.11 | 23 traits and 17 registries with at most one implementor (5 and 4 with none), 78 function names defined in three or more crates, 2 kill switches, 135 environment variables |

Run at most four sweeps at once; review attention is the throttle, not the CPU. No sweep builds, tests or touches the GPU. Sweeps read the main checkout and write only their two evidence files, so they need no worktree and make no commit; the lead commits the evidence (docs only, direct to main). Rows of one kind in one file (the theme constants in `crates/manifold-ui/src/color.rs`, for example) become one record with a per-name proof list, which brings `app-ui` to about 110 records. A sweep still over 120 records splits by crate before it starts. `patterns` starts after the five code sweeps (`engine-model`, `engine-runtime`, `graph`, `water`, `app-ui`), because it judges what they hand on.

**Stage 2: review** (section 6). **Stage 3: report** (section 7).

## 6. Review

One Opus reviewer per two sweeps, read-only, with a budget and a half-budget partial report (`docs/AGENT_ROUTING.md` section Reviews and the consult seat). For each sweep:

- **Always re-run in full:** every `stage` and `prep` row; every row in 3.4 (macro trap), 3.5, 3.6, 3.7 (string and corpus traps), 3.8 (data in the wild) and 3.10 (dynamic dispatch and inventory); every grade A delete over 200 lines.
- **Sample:** 20 percent of the remaining rows, at least five per sweep, picked at random (`shuf -n`).
- **Per checked row:** re-run the proof commands and compare; run one search the sweep did not run, rotating through the corpus, hidden directories, `plugins/`, non-Rust files, macro paths and format-built ids; then `git log -S NAME --oneline | head` and `bd list --status open | rg -i NAME` for recent intent. An open bead or design doc naming the item as upcoming turns it into `ask`.
- **Fail rule:** two wrong claims in one sweep's sample send that sweep's whole category back. The sweep is respawned once with the miss added to its trap list; a second failure means the lead reshapes the job.

The reviewer writes `docs/evidence/simplification_census_2026-10/review-<sweeps>.md`: rows checked, rows changed with their new grade or action, the failure cases, and what it did not read.

## 7. The report for Peter

The lead writes `docs/evidence/simplification_census_2026-10/REPORT.md`; Fable reviews it before Peter sees it.

- First, three numbers: lines removable at grade A, lines at grade B, and how many decisions Peter owes.
- Then by area (engine, graph, water, app and UI, content, tooling, tools), biggest win first. Each item: what it is, what it means on stage, the one-line proof, risk, action, lines.
- Then "bugs, not slop": rows where the dead or write-only code is a symptom of a missing wire or wasted hot-path work (for example engine-041, a BPM-change command nothing builds; graph-020, a uniform arena written every frame and never read; app-ui-065 and 066, view models built on every projection and never read). These are fixed or wired, never deleted or deferred by accident.
- Then Peter's decisions, one line each with the two options and what each costs.
- Then the over-engineering patterns that produced the slop, each with the rule that would stop it. Seed list from the pilot and the screens, to be confirmed or replaced by the sweeps:
  - **The caller goes, the callee stays.** `pub` hides it from the compiler. Rule: a deletion landing runs `census_refs.py` on every crate it touched.
  - **Port everything, wire some.** Unity-era setters and getters ported whole and never called. Rule: port only what a caller needs; a "not wired yet" suppression gets a bead and an expiry.
  - **Switch it off instead of deciding.** A const flag keeps a whole feature compiled, tested and maintained. Rule: a feature turned off by a const gets a dated bead to delete it or turn it back on.
  - **Extension points with nothing to extend.** Traits and registries with no implementor. Rule: no trait or registry without two implementors, or a layering reason named in the crate-split design.
  - **Test scenes in the user's browser.** Rule: a preset only tests use lives in test fixtures.
  - **Every engine knob reaches the UI.** Rule: a param reaches the UI only when a project would change it; the rest are constants.
  - **Crate splits copy dependency lists.** Rule: the landing gate runs `cargo machete` on touched crates.
  - **Tools rot unseen.** Verbs crash and nobody notices. Rule: the dev.py test runs `<verb> --help` for every verb.

## 8. Relation to the god file decomposition

The god file campaign's waves 1 to 3 are done and the crate split shipped (`docs/RENDERER_CRATE_SPLIT_DESIGN.md`). What remains sits under open epics whose titles already include a cleanup pass: BUG-bfktg (Rendering-side decomposition) and BUG-brw4f (App and UI decomposition), with children such as BUG-trh7 (render_scene.rs godfile, 9.7k lines), BUG-2rh (inspector.rs decomposition), BUG-tto (panels/inspector.rs decomposition) and BUG-9cy (scene_setup_panel.rs decomposition). The water side has its own audited cleanup under BUG-hkbdp (Renderer crate split epic), child 6, the post-split water cleanup: slop out, CPU FLIP out of the product or under one cfg, no production water file over 1.5k lines, a test cut.

- **Order: census and deletion first, splitting second.** Splitting dead code moves it into more files and costs a pure-move landing. Deleting first shrinks what is left to split, and some files may drop under their ceiling without a split.
- **Overlap.** This census is the cleanup pass those epics name. A record inside a file one of those beads covers names the bead in its proof column, so the decomposition lane picks it up. The `water` sweep reads the post-split water cleanup bead first and does not re-find its audited items.
- **Ceilings.** `crates/manifold-app/tests/godfile_regrowth.rs` holds per-file line ceilings. A deletion landing that shrinks a listed file lowers its ceiling in the same landing, or the freed room regrows.
- **History across moves.** Many files were last touched by a pure move (for example f2281f621, the scene ownership move). Use `git log --follow` and the ignore list in `.git-blame-ignore-revs`; a move commit is not evidence of recent use.

## 9. Pilot: manifold-foundation

The method was piloted on manifold-foundation (2,552 lines of source, 137 `pub` items, used by 13 crates), static tools only, at main 37a9c180a.

| Check | Result |
|---|---|
| `cargo check -p manifold-foundation --all-targets` | clean, no warnings |
| `cargo machete crates/manifold-foundation` | clean |
| suppressions | none |
| `census_refs.py` | 3 TEST-ONLY, 5 CRATE-ONLY, 129 USED |

**Found:**
- `uv_transform.rs` (111 lines): `decompose_uv_affine`, `compose_uv_affine` and `UvDecomposition` are used only by their own tests. Commit 61b52dcef ("Simplify scene material controls and make colour editing direct") deleted their only caller, `material_placement.rs` in manifold-ui, and the module survived because `pub` kept the compiler quiet. `UvDecomposition` read CRATE-ONLY: a cascade. Grade A, delete.
- `WorkKind::from_name` (3 lines): only its own round-trip test calls it. Grade A, delete with the assert.
- `RELIGHT_FEATURE_ENABLED = false` since 2026-07-18 (section 3.9). Deleted whole 2026-10-20 with the feature.

**Kept, correctly:**
- `PhysicsSettings::legacy()` and `SimRate::legacy()`: 168 of 182 projects have no physics block and load through this default.
- `RtQualitySettings`: 42 projects store it, in 8 different combinations.
- `StackText::as_str` falls back to an empty string instead of panicking on a per-frame display path. Right for a live instrument.

**False positives the pilot caught, now fixed in the tools:**
- The first `census_refs.py` treated everything after the first `#[cfg(test)]` in a file as test code. Out-of-line `#[cfg(test)] mod tests;` lines near the top of `app_render.rs` and `content_pipeline.rs` made 39 manifold-app items look TEST-ONLY. Hand checks found real callers (`toggle_audio_dock` at `app_render.rs:883`, `cell_for` at `content_pipeline.rs:562`). It now counts only inline `mod x { … }` bodies and items carrying their own `#[cfg(test)]`. Result: manifold-app 0 TEST-ONLY and 3 DEAD, all three under `#[allow(dead_code)]`, which matches the compiler.
- `rg` without `--hidden` skips `.claude/`, `.config/` and `.beads/`, where hooks and the nextest config name scripts and tests. Every search in this method uses `--hidden -g '!.git/'`.
- Script mentions in transcripts are not runs: `rt_a2_term_cost.py` is mentioned in 38 sessions and run in none. The screen counts only Bash `"command"` fields.
- Scene-modifier and LED preset ids contain spaces or differ from file names; ids come from `presetMetadata.id`, never from the file name.
- Some files in the repo are not UTF-8; the tools decode with replacement instead of crashing.
- The first settings screen read only generator params (`genParams.params`) and so called every node param inside the embedded graph "never moved". The screen now walks every `typeId` node anywhere in the project.

**Whole workspace, same screens.** `census_refs.py` over all 30 member crates gives 635 DEAD or TEST-ONLY rows, about 3,900 lines of item bodies before their tests. Hand checks while calibrating: 4 of 4 sampled manifold-ui theme constants (`MAGNET_RADIUS_PX`, `DRAG_HANDLE_HOVER`, `EFFECT_DRAG_INDICATOR_UNGROUP`, `FOOTER_BTN_HOVER`) are truly unused; 4 of 4 sampled manifold-playback rows are unused, and two of them are already named in the playback audit hygiene bead (`bd show BUG-mmct0.12`). All other numbers in section 3 are screen output, not reviewed findings.

## 10. Decisions owed

For the lead:
- The tools live in `/tmp` with their source in the evidence folder. Promote them to `scripts/` with a dev.py verb only if the census becomes recurring.

For Peter:
- Already visible from the screens, each to be confirmed by its sweep: dropping old project formats (convert the old shows once, then cut the migration steps); the LED generators; the six water test scenes in the generator browser; the World node's own sun.

## 11. Sweep briefs

Each brief below is sent as written, with the lead filling `<main commit>` from `raw/main_commit.txt`. Each sweep agent first reads this document's sections 2 to 4 and the section 3 entries its brief names.

Standing rules, restated in every brief: read-only on code; write only your two evidence files; no `cd` (absolute paths, `git -C`); no inline python (`python3 -c`, heredocs into python); use `rg`, `fd`, `ast-grep`, `jq`; never `cargo build`, `cargo test` or anything that touches the GPU; never open a file in Dropbox; no commits and no beads; when a command is denied, record it and move on; your final message is your report: candidates by grade, lines, and what you did not cover.

### 11.1 engine-model (Sonnet, medium effort)

> You are the `engine-model` sweep of the simplification census (BUG-yn979, Simplification pass). Read `/Users/peterkiemann/MANIFOLD - Rust/docs/SIMPLIFICATION_CENSUS.md` sections 2, 3.1 to 3.4, 3.9 and 4 first, then follow them exactly.
>
> Standing rules: read-only on code; write only your two evidence files; no `cd` (absolute paths, `git -C`); no inline python; use `rg`, `fd`, `ast-grep`, `jq`; never build, test or touch the GPU; never open a file in Dropbox; no commits, no beads; a denied command is recorded and skipped. Your final message is your report: candidates by grade, total lines, what you did not cover.
>
> Scope: crates foundation, core, editing, io (`/Users/peterkiemann/MANIFOLD - Rust/crates/manifold-<name>`). Screens at main `<main commit>` are in `/tmp/simplification_census/raw/`. Before step 1, read `raw/open_cleanup_beads.txt`: a row an open bead already names cites that bead (section 4 rules). Many manifold-core DEAD rows are Unity-era setters and getters; check each anyway, one row per file when a whole group is dead.
>
> 1. For each crate, take every DEAD and TEST-ONLY row of `raw/pub_refs/manifold-<name>.tsv`. For each row run, and record in your proof file:
>    `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME "/Users/peterkiemann/MANIFOLD - Rust"`
>    `rg -l -w -F NAME /tmp/simplification_census/corpus/projects`
>    plus the "also search" checks in 3.1 that apply (serde string attributes, macros, `extern "C"` names from .m, .swift and .h files for manifold-native, re-exports). For TEST-ONLY rows, list the test callers and say whether each test covers anything else.
> 2. Every `raw/suppressions.txt` entry in your crates: reason, `git -C "/Users/peterkiemann/MANIFOLD - Rust" blame -L N,N <file>` date, whether its named condition happened.
> 3. Every `raw/machete.txt` dependency in your crates: the 3.4 commands.
> 4. Your crates' rows of `raw/features.tsv` with a count of 3 or less, `raw/const_bools.txt`, and the environment variables from `raw/env_knobs.txt` your crates read (`rg -n 'env::var(_os)?\("NAME"' <crate dir>`). List each with where it is read; mark it `ask` for the patterns sweep instead of judging it.
> 5. Write `docs/evidence/simplification_census_2026-10/engine-model.md` and `engine-model.proof.md` in the record format of the census doc's section 4 (The candidate record). One row per cluster when items only use each other.
>
> Do not judge traits, duplicates, fallbacks or content. Put anything you notice there in a short "seen, not judged" list at the end of `engine-model.md`.

### 11.1b engine-runtime (Sonnet, medium effort)

> As 11.1, with sweep name `engine-runtime`, files `engine-runtime.md` and `engine-runtime.proof.md`, and scope playback, compositor, audio, spectral, media, recording, profiler, native, led. Extra traps: manifold-native is called from Objective-C and Swift by name (`extern "C"`), so search `.m`, `.mm`, `.swift` and `.h` files; MIDI, OSC and Ableton handlers can be reached by message strings (`rg -F '"/address"'`), not Rust names; the playback audit hygiene bead (`bd show BUG-mmct0.12`) already names several playback rows.

### 11.2 graph (Sonnet, medium effort)

> As 11.1, with sweep name `graph`, files `graph.md` and `graph.proof.md`, and scope node-engine, nodes, nodes-image, nodes-scene. Extra traps for this scope: primitives register through the `primitive!` macro and `inventory`, so a primitive struct is used by its registration even when no code names it; node ids are used through their string in preset JSON (`rg -F '"node.x"' "/Users/peterkiemann/MANIFOLD - Rust/crates/manifold-nodes/assets"`); glTF import code is driven by data in imported files, so an import branch with no Rust caller beyond the loader is live.

### 11.3 water (Sonnet, medium effort)

> As 11.1, with sweep name `water`, files `water.md` and `water.proof.md`, and scope physics, fluids, nodes-water, water-rigid, water-liquid, water-gpu-flip, water-gpu-mpm, water-whitewater, water-surface. First run `bd show BUG-hkbdp.6` and its children: that audited water cleanup already owns some findings; where a row matches one, name the bead in the proof column instead of re-arguing it. Extra traps: these crates are meant to ship on their own one day, so a `pub` item may be public API on purpose (`docs/RENDERER_CRATE_SPLIT_DESIGN.md`); record such rows as `ask` with that reason. CPU reference solvers and conformance testkits are test infrastructure.

### 11.4 app-ui (Sonnet, medium effort)

> As 11.1, with sweep name `app-ui`, files `app-ui.md` and `app-ui.proof.md`, and scope app, ui, ui-paint, gpu. Extra traps: manifold-app is a binary, so its DEAD rows are only items under `#[allow(dead_code)]`, which makes suppressions (step 2) the main work there; UI flow scripts in `scripts/ui-flows/*.json` name panels and actions by string; manifold-gpu items may be named from shader setup by string.

### 11.5 tooling (Sonnet, medium effort)

> You are the `tooling` sweep of the simplification census (BUG-yn979, Simplification pass). Read `/Users/peterkiemann/MANIFOLD - Rust/docs/SIMPLIFICATION_CENSUS.md` sections 3.12 and 4 first.
>
> Standing rules as in 11.1. Scope: `scripts/` (not the contents of `scripts/ui-flows/`), `scripts/dev.py` verbs, `.claude/hooks/`, `docs/` (top level and `docs/archive/`), the repo root, `plugins/` except `plugins/vendor/`, `tv-led-mirror/`. Screens at main `<main commit>` are in `/tmp/simplification_census/raw/`.
>
> 1. Scripts: start from `raw/scripts.tsv` (runs in 30 days, executable bit, references). For each script with 0 runs, also count runs through its verb: `fd -e jsonl . /Users/peterkiemann/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust --changed-within 30d -X rg -o --no-filename '"command":"[^"]{0,300}dev\.py VERB'`. Run `"/Users/peterkiemann/MANIFOLD - Rust/scripts/dev.py" VERB --help` for every verb and record which crash. Note which scripts are libraries other scripts import (`rg -n 'import NAME|from NAME' "/Users/peterkiemann/MANIFOLD - Rust/scripts"`).
> 2. Hooks: list the hook registrations in `.claude/settings.json` with `jq`, then count fires per hook over 30 days in `.claude/telemetry/hook-fires.jsonl` with `jq`. One command per step, output to `/tmp/simplification_census/`; never pipe a `.claude` path into awk or sed. A hook file no registration names is a candidate.
> 3. Docs: from `raw/docs.tsv`, every doc with 0 reads and 2 or fewer inbound references: read its first 20 lines and its status line, list who references it. Run `"/Users/peterkiemann/MANIFOLD - Rust/.claude/hooks/design_status.py" --lifecycle-check`. For `docs/archive/`, count inbound references per file.
> 4. Repo root and leftovers: `git -C "/Users/peterkiemann/MANIFOLD - Rust" ls-files` at depth 1, tracked binaries, `tv-led-mirror/`, `plugins/`. Who builds or runs each.
> 5. Write `docs/evidence/simplification_census_2026-10/tooling.md` and `tooling.proof.md` in the record format of the census doc's section 4 (The candidate record). Anything Peter may run by hand is `ask`.

### 11.6 tools-lab (Sonnet, medium effort)

> You are the `tools-lab` sweep of the simplification census (BUG-yn979, Simplification pass). Read `/Users/peterkiemann/MANIFOLD - Rust/docs/SIMPLIFICATION_CENSUS.md` sections 3.12 and 4 first.
>
> Standing rules as in 11.1. Scope: `/Users/peterkiemann/MANIFOLD - Rust/tools/`. The detector lab (`tools/audio_analysis/lab/`) is the live system; `audio_analysis/eval/` holds earlier experiments. Never touch training data, labels or caches; data files are out of scope.
>
> 1. For every Python file: who imports it (`rg -n 'import STEM|from STEM|from \.STEM'` across `tools/` and `scripts/`), whether `scripts/dev.py` names it, real runs in 30 days (`fd -e jsonl . /Users/peterkiemann/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust --changed-within 30d -X rg -o --no-filename '"command":"[^"]{0,300}FILE'`), and its last commit date (`git -C "/Users/peterkiemann/MANIFOLD - Rust" log -1 --format=%ad --date=short -- PATH`).
> 2. Group eval scripts by family (kick, snare and so on). A family whose job the lab now does (read the lab module docstrings) is one row.
> 3. `rt_prototype/`, `mtl4fx_repro/`, `segmentation/`: what each was for (its README), and whether anything references it.
> 4. Write `docs/evidence/simplification_census_2026-10/tools-lab.md` and `tools-lab.proof.md` in the record format of the census doc's section 4 (The candidate record).

### 11.7 content (Opus, high effort)

> You are the `content` sweep of the simplification census (BUG-yn979, Simplification pass). Read `/Users/peterkiemann/MANIFOLD - Rust/docs/SIMPLIFICATION_CENSUS.md` sections 2, 3.5 to 3.7 and 4, and `docs/evidence/ux_walkthrough_2026-10-19/README.md`.
>
> Standing rules as in 11.1. Inputs: `raw/node_usage.tsv`, `raw/node_param_moves.tsv`, `raw/node_param_distinct.tsv`, the corpus (`/tmp/simplification_census/corpus/`; `index.tsv` maps files to projects and dates), `docs/node_catalog.json`, the preset folders.
>
> Project use is never evidence (section 2): a node, preset or setting Peter has not used yet may still belong. Use the corpus only to say whether a deletion needs a load path.
> 1. Nodes that are leftovers (3.5): one-offs a newer node replaced, dev or probe artifacts, nodes no preset and no code uses outside a family Peter is building. For each, the 3.5 searches and what replaced it.
> 2. Presets: separate test, probe and engine-comparison scenes (3.6) from real content. For each test scene, list every Rust file that loads it.
> 3. Params (3.7): duplicates, settings for another mode or type, engine internals with no performer meaning, settings that do nothing. Find them by reading the node code and the catalog, starting from the walkthrough's complaints. Group per node; one line each on what it is on stage. Actions are `cut-param` or `ask`; never new UI machinery.
> 4. For each walkthrough pattern, name the code root when it is slop (dead param, duplicate param, test scene, leftover) and leave pure design issues to the UX pass in a short list.
> 5. Write `docs/evidence/simplification_census_2026-10/content.md` and `content.proof.md` in the record format of the census doc's section 4 (The candidate record).

### 11.8 fallbacks (Opus, high effort)

> You are the `fallbacks` sweep of the simplification census (BUG-yn979, Simplification pass). Read `/Users/peterkiemann/MANIFOLD - Rust/docs/SIMPLIFICATION_CENSUS.md` sections 2, 3.8 and 4.
>
> Standing rules as in 11.1. Inputs: `raw/fallback_files.txt`, `raw/fallback_fns.txt`, `raw/project_versions.txt`, `raw/type_id_migrations.tsv`, the corpus.
>
> 1. Migrations (`crates/manifold-io/src/migrations/`, `crates/manifold-io/src/migrate.rs`, `crates/manifold-core/src/project/load_migration.rs`, the glTF import migration, `TYPE_ID_MIGRATIONS`): for each, the version or shape it handles and how many corpus projects carry it. None can go while old projects carry its input; record what each costs (lines, tests) so Peter can weigh dropping old formats.
> 2. Legacy and compat branches: name the input shape, search the corpus and the bundled presets for it with `jq` or `rg`, record the counts.
> 3. Silent fallbacks: start from the files with the most hits on load, model and edit paths (core, io, editing, node-engine load). For each candidate say what bug it hides and what the honest behavior is (an error the caller handles, or a panic in a path that can never fail). Display and audio paths that keep the show running stay.
> 4. Tests that exist only for a candidate above go on the same row.
> 5. Write `docs/evidence/simplification_census_2026-10/fallbacks.md` and `fallbacks.proof.md` in the record format of the census doc's section 4 (The candidate record).

### 11.9 patterns (Opus, high effort)

> You are the `patterns` sweep of the simplification census (BUG-yn979, Simplification pass). Read `/Users/peterkiemann/MANIFOLD - Rust/docs/SIMPLIFICATION_CENSUS.md` sections 3.9 to 3.11 and 4, and `docs/RENDERER_CRATE_SPLIT_DESIGN.md` D4 and D12.
>
> Standing rules as in 11.1. Inputs: `raw/traits.tsv`, `raw/registries.tsv`, `raw/free_fn_dupes.tsv`, `raw/free_fns_all.tsv`, `raw/const_bools.txt`, `raw/env_knobs.txt`, `raw/features.tsv`, and the "seen, not judged" lists and `ask` switch rows in `engine-model.md`, `engine-runtime.md`, `graph.md`, `water.md` and `app-ui.md`.
>
> 1. Traits with no implementor and registries with no submissions: prove it (3.10), then list everything that exists only to serve them (fields, loops, tests).
> 2. One-implementor traits and one-submission registries: is each a layering seam the crate-split design requires? If not, inline.
> 3. Duplicates: for each product-code name defined in three or more crates, say whether it is one concept, which owner the layering allows, and the lines saved.
> 4. Switches and knobs: the two const kill switches, environment knobs named for bugs or one-off traces, features with one use. What each turns off, since when, what it costs.
> 5. Write `docs/evidence/simplification_census_2026-10/patterns.md` and `patterns.proof.md` in the record format of the census doc's section 4 (The candidate record), plus a short list of the over-engineering patterns you saw, one example each, for the report.

### 11.10 stage 0 (Haiku, or the lead in the background)

The lead can run `/tmp/simplification_census/tools/run_screens.sh` itself with `run_in_background` and follow it with Monitor; the output it needs is the closing `wc -l` table. A Haiku runs it in foreground chunks, each under ten minutes:

> Run these five commands one at a time with the Bash tool in the foreground, each with `timeout` 600000, and wait for each to finish before the next:
> 1. `STEPS=rest /tmp/simplification_census/tools/run_screens.sh`
> 2. `STEPS=pub CRATES="manifold-app manifold-core manifold-editing manifold-foundation manifold-io" /tmp/simplification_census/tools/run_screens.sh`
> 3. `STEPS=pub CRATES="manifold-playback manifold-compositor manifold-audio manifold-spectral manifold-media manifold-recording manifold-profiler manifold-native manifold-led manifold-gpu manifold-ui-paint" /tmp/simplification_census/tools/run_screens.sh`
> 4. `STEPS=pub CRATES="manifold-ui manifold-node-engine manifold-nodes manifold-nodes-image manifold-nodes-scene" /tmp/simplification_census/tools/run_screens.sh`
> 5. `STEPS=pub CRATES="manifold-physics manifold-fluids manifold-nodes-water manifold-water-rigid manifold-water-liquid manifold-water-gpu-flip manifold-water-gpu-mpm manifold-water-whitewater manifold-water-surface" /tmp/simplification_census/tools/run_screens.sh`
>
> If a command times out, report which one and stop. Then run `cat /tmp/simplification_census/raw/pub_refs/summary.txt /tmp/simplification_census/raw/main_commit.txt` and `wc -l /tmp/simplification_census/raw/*.tsv /tmp/simplification_census/raw/*.txt`, and report both outputs verbatim. Do not edit any file. No `cd`, no inline python, no sleep loops.
