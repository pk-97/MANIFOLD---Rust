# engine-model: proof

Commands are run from the scratchpad against `/Users/peterkiemann/MANIFOLD - Rust` at main 37a9c180a. `RG` below is `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME "/Users/peterkiemann/MANIFOLD - Rust"`; `CORPUS` is `rg -l -w -F NAME /tmp/simplification_census/corpus/projects` (182 projects; a loading check, never a usage check, so 0 means no load path needs to survive the deletion); `GIT` is `git -C "/Users/peterkiemann/MANIFOLD - Rust" log -S NAME --format='%h %ad %s' --date=short -- crates | head -4` (substring match, so a short name also matches longer ones). The summary after each arrow is what the command printed. A tag like `rerun TEST-ONLY` means the item is not in the stage 0 screen and got its verdict from the scratchpad rerun that ignores comment hits and a type's own `impl` headers (see `engine-model.md`, Method gap).

### engine-001

Claim: 2 fns used only by their own tests (16 hits, all in the file) and the struct only by those fns; 0 hits in other crates, strings, corpus; sole caller (material_placement.rs in manifold-ui) deleted in 61b52dcef; the pilot found the same

- `decompose_uv_affine` (TEST-ONLY fn, crates/manifold-foundation/src/uv_transform.rs:17)
  - RG `decompose_uv_affine` -> 9 hits: the definition; 7 in the same file (lines 60, 74, 89, 100, 107, 108, ...); 1 in docs (docs/SIMPLIFICATION_CENSUS.md)
  - CORPUS `decompose_uv_affine` -> 0 project files (loading check only)
  - GIT `decompose_uv_affine` -> 2 commits, newest: 61b52dcef 2026-09-22 Simplify scene material controls and make colour editing direct
- `compose_uv_affine` (TEST-ONLY fn, crates/manifold-foundation/src/uv_transform.rs:44)
  - RG `compose_uv_affine` -> 7 hits: the definition; 5 in the same file (lines 60, 78, 88, 90, 101); 1 in docs (docs/SIMPLIFICATION_CENSUS.md)
  - CORPUS `compose_uv_affine` -> 0 project files (loading check only)
  - GIT `compose_uv_affine` -> 2 commits, newest: 61b52dcef 2026-09-22 Simplify scene material controls and make colour editing direct
- `UvDecomposition` (rerun CRATE-ONLY struct, crates/manifold-foundation/src/uv_transform.rs:8)
  - RG `UvDecomposition` -> 7 hits: the definition; 5 in the same file (lines 17, 36, 44, 60, 83); 1 in docs (docs/SIMPLIFICATION_CENSUS.md)
  - CORPUS `UvDecomposition` -> 0 project files (loading check only)
  - GIT `UvDecomposition` -> 2 commits, newest: 61b52dcef 2026-09-22 Simplify scene material controls and make colour editing direct
- `rg -n 'mod uv_transform|uv_transform::' crates --glob '*.rs'` outside the file -> only `pub mod uv_transform;` at crates/manifold-foundation/src/lib.rs:30.
- `git log -1 61b52dcef` -> 2026-09-22 Simplify scene material controls and make colour editing direct (the commit the pilot names as deleting the caller).
- The 4 tests in the file cover only these two fns, nothing else.
- Lines: 112.

### engine-002

Claim: `WorkKind::from_name` is called by one assert in `names_round_trip_and_cover_every_kind` (work_count.rs:342) and nowhere else; 0 corpus; the pilot found the same

- `from_name` (TEST-ONLY fn, crates/manifold-foundation/src/work_count.rs:168)
  - RG `from_name` -> 3 hits: the definition; 1 in the same file (lines 342); 1 in docs (docs/SIMPLIFICATION_CENSUS.md)
  - CORPUS `from_name` -> 0 project files (loading check only)
  - GIT `from_name` -> 4+ commits, newest: 2b84b8383 2026-10-14 UI invalidation P1: work counters and the count gate, measuring the before
- The other two asserts in the same test (`kind.index()`) cover the declaration-order contract, so the test stays.
- Lines: 4.

### engine-003

Claim: the file is not in the module tree (no `mod mri` anywhere in manifold-core), so it is never compiled; all four of its items have 0 outside hits; the format was unhooked in 111472546 (MRI generator moved to TIFF slices)

- `MRIVOL_MAGIC` (DEAD const, crates/manifold-core/src/mri.rs:4)
  - RG `MRIVOL_MAGIC` -> 1 hit: the definition
  - CORPUS `MRIVOL_MAGIC` -> 0 project files (loading check only)
  - GIT `MRIVOL_MAGIC` -> 2 commits, newest: 111472546 2026-03-23 refactor: MRI generator loads TIFF slices as 2D textures
- `MriVolHeader` (rerun DEAD struct, crates/manifold-core/src/mri.rs:36)
  - RG `MriVolHeader` -> 2 hits: the definition; 1 in the same file (lines 65)
  - CORPUS `MriVolHeader` -> 0 project files (loading check only)
  - GIT `MriVolHeader` -> 2 commits, newest: 111472546 2026-03-23 refactor: MRI generator loads TIFF slices as 2D textures
- `VoxelFormat` (rerun CRATE-ONLY enum, crates/manifold-core/src/mri.rs:8)
  - RG `VoxelFormat` -> 6 hits: the definition; 5 in the same file (lines 17, 21, 22, 23, 48)
  - CORPUS `VoxelFormat` -> 0 project files (loading check only)
  - GIT `VoxelFormat` -> 2 commits, newest: 111472546 2026-03-23 refactor: MRI generator loads TIFF slices as 2D textures
- `bytes_per_voxel` (rerun CRATE-ONLY fn, crates/manifold-core/src/mri.rs:19)
  - RG `bytes_per_voxel` -> 2 hits: the definition; 1 in the same file (lines 73)
  - CORPUS `bytes_per_voxel` -> 0 project files (loading check only)
  - GIT `bytes_per_voxel` -> 1 commits, newest: 59c8a0901 2026-03-21 feat: MRI Volume generator — slice viewer, raymarch, cine playback
- `orphans.sh crates/manifold-core (scratchpad: for each src .rs file, rg for `mod STEM` in its dir, the parent .rs, lib.rs)` -> 2 files flagged: effects/instance_serde_tests.rs (reached by `include!`-style path from instance_serde.rs) and mri.rs (no mention at all). foundation, editing, io: none.
- `rg -n -w 'mri' crates/manifold-core --glob '*.rs'` -> no `mod mri` or `mri::` hit; only unrelated `path` words
- `tools/nifti_to_mrivol.py` and `tools/mrivol_to_tiff.py` write and read the `.mrivol` format; they belong to the tools-lab sweep (seen, not judged).
- Lines: 80.

### engine-004

Claim: the whole `beat_division_helper` module (6 DEAD rows plus `decompose` and `BeatModifier` that only the module uses) has 0 users; only the re-export at effects/mod.rs:35 names it; after it goes `BeatDivision::from_i32` has no caller either

- `STRAIGHT_COUNT` (DEAD const, crates/manifold-core/src/effects/driver.rs:397)
  - RG `STRAIGHT_COUNT` -> 1 hit: the definition
  - CORPUS `STRAIGHT_COUNT` -> 0 project files (loading check only)
  - GIT `STRAIGHT_COUNT` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `DOTTED_COUNT` (DEAD const, crates/manifold-core/src/effects/driver.rs:398)
  - RG `DOTTED_COUNT` -> 1 hit: the definition
  - CORPUS `DOTTED_COUNT` -> 0 project files (loading check only)
  - GIT `DOTTED_COUNT` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `TRIPLET_COUNT` (DEAD const, crates/manifold-core/src/effects/driver.rs:399)
  - RG `TRIPLET_COUNT` -> 1 hit: the definition
  - CORPUS `TRIPLET_COUNT` -> 0 project files (loading check only)
  - GIT `TRIPLET_COUNT` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `TOTAL_COUNT` (DEAD const, crates/manifold-core/src/effects/driver.rs:400)
  - RG `TOTAL_COUNT` -> 1 hit: the definition
  - CORPUS `TOTAL_COUNT` -> 0 project files (loading check only)
  - GIT `TOTAL_COUNT` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `to_label` (DEAD fn, crates/manifold-core/src/effects/driver.rs:410)
  - RG `to_label` -> 1 hit: the definition
  - CORPUS `to_label` -> 0 project files (loading check only)
  - GIT `to_label` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `try_compose` (DEAD fn, crates/manifold-core/src/effects/driver.rs:451)
  - RG `try_compose` -> 1 hit: the definition
  - CORPUS `try_compose` -> 0 project files (loading check only)
  - GIT `try_compose` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `beat_division_helper` (crates/manifold-core/src/effects/driver.rs:394)
  - RG `beat_division_helper` -> 4 hits: the definition; 1 in other files (crates/manifold-core/src/effects/mod.rs); 2 in docs (docs/MODEL_COMMAND_DECOMPOSITION_DESIGN.md x2)
  - CORPUS `beat_division_helper` -> 0 project files (loading check only)
  - GIT `beat_division_helper` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `BeatModifier` (rerun CRATE-ONLY enum, crates/manifold-core/src/effects/driver.rs:403)
  - RG `BeatModifier` -> 9 hits: the definition; 8 in the same file (lines 437, 440, 442, 444, 451, 453, ...)
  - CORPUS `BeatModifier` -> 0 project files (loading check only)
  - GIT `BeatModifier` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `decompose` (crates/manifold-core/src/effects/driver.rs:437)
  - RG `decompose` -> 50 hits: the definition; 2 in other files (crates/manifold-nodes-image/src/node_graph/primitives/transform_components.rs, crates/manifold-nodes-image/src/node_graph/primitives/watercolor.rs); 5 in comments (crates/manifold-node-engine/src/mesh.rs, crates/manifold-node-engine/src/freeze/classify.rs, crates/manifold-node-engine/src/atomic/mod.rs, crates/manifold-nodes-scene/src/node_graph/gltf_load.rs, ...); 40 in docs (.claude/hooks/context-nudges/gpu-shader-gotchas.md, docs/CANVAS_API_DESIGN.md, docs/MATERIAL_INSPECTOR_DESIGN.md, docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md x10, ...); 2 in beads
  - CORPUS `decompose` -> 0 project files (loading check only)
  - GIT `decompose` -> 4+ commits, newest: aa2ba5636 2026-10-14 SCENE_SURFACES P6b: the Scene tab tree, item header and grouped controls
- `from_i32` (rerun CRATE-ONLY fn, crates/manifold-core/src/types.rs:695)
  - RG `from_i32` -> 4 hits: the definition; 3 in other files (crates/manifold-core/src/effects/driver.rs x3)
  - CORPUS `from_i32` -> 0 project files (loading check only)
  - GIT `from_i32` -> 2 commits, newest: 72d708b51 2026-07-22 refactor(core): P2-E S2 — extract leaf families (param_defs/relight/group/driver/envelope/automation) from effects/mod.rs
- `rg -n -w 'BeatModifier|decompose'` over crates, scripts, tools: only driver.rs lines inside the module (the other `decompose` hits are prose in comments and a node alias string in nodes-image).
- `rg -n 'beat_division_helper' crates` -> the definition and `pub use driver::{beat_division_helper, ...}` at effects/mod.rs:35, nothing else in code (2 lines in MODEL_COMMAND_DECOMPOSITION_DESIGN.md name it); 0 corpus files.
- `from_i32` (types.rs:695, about 31 lines with its doc) is a cascade: its 3 callers are `try_compose` arms. Counted in the lines. The deleting lane's `cargo check` confirms it.
- No test in driver.rs's test module names the module or its fns.
- Lines: 105 (estimate).

### engine-005

Claim: 16 Unity-port setters and getters on `ProjectSettings`, each with exactly one hit (its definition) across the repo, 0 in strings, docs, corpus; the siblings that are used (`set_bpm`, `set_frame_rate`, `set_master_opacity`) stay

- `set_output_height` (DEAD fn, crates/manifold-core/src/settings.rs:365)
  - RG `set_output_height` -> 1 hit: the definition
  - CORPUS `set_output_height` -> 0 project files (loading check only)
  - GIT `set_output_height` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `set_time_sig_numerator` (DEAD fn, crates/manifold-core/src/settings.rs:371)
  - RG `set_time_sig_numerator` -> 1 hit: the definition
  - CORPUS `set_time_sig_numerator` -> 0 project files (loading check only)
  - GIT `set_time_sig_numerator` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `set_time_sig_denominator` (DEAD fn, crates/manifold-core/src/settings.rs:374)
  - RG `set_time_sig_denominator` -> 1 hit: the definition
  - CORPUS `set_time_sig_denominator` -> 0 project files (loading check only)
  - GIT `set_time_sig_denominator` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `set_video_player_pool_size` (DEAD fn, crates/manifold-core/src/settings.rs:380)
  - RG `set_video_player_pool_size` -> 1 hit: the definition
  - CORPUS `set_video_player_pool_size` -> 0 project files (loading check only)
  - GIT `set_video_player_pool_size` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `set_max_layers` (DEAD fn, crates/manifold-core/src/settings.rs:383)
  - RG `set_max_layers` -> 1 hit: the definition
  - CORPUS `set_max_layers` -> 0 project files (loading check only)
  - GIT `set_max_layers` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `set_default_recording_layer` (DEAD fn, crates/manifold-core/src/settings.rs:386)
  - RG `set_default_recording_layer` -> 1 hit: the definition
  - CORPUS `set_default_recording_layer` -> 0 project files (loading check only)
  - GIT `set_default_recording_layer` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `set_osc_send_port` (DEAD fn, crates/manifold-core/src/settings.rs:389)
  - RG `set_osc_send_port` -> 1 hit: the definition
  - CORPUS `set_osc_send_port` -> 0 project files (loading check only)
  - GIT `set_osc_send_port` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `get_frame_duration` (DEAD fn, crates/manifold-core/src/settings.rs:402)
  - RG `get_frame_duration` -> 1 hit: the definition
  - CORPUS `get_frame_duration` -> 0 project files (loading check only)
  - GIT `get_frame_duration` -> 4+ commits, newest: e34002adc 2026-09-21 Unify project preparation and paced playback measurements
- `time_to_frame` (DEAD fn, crates/manifold-core/src/settings.rs:405)
  - RG `time_to_frame` -> 1 hit: the definition
  - CORPUS `time_to_frame` -> 0 project files (loading check only)
  - GIT `time_to_frame` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `frame_to_time` (DEAD fn, crates/manifold-core/src/settings.rs:408)
  - RG `frame_to_time` -> 1 hit: the definition
  - CORPUS `frame_to_time` -> 0 project files (loading check only)
  - GIT `frame_to_time` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `has_any_master_effect` (DEAD fn, crates/manifold-core/src/settings.rs:413)
  - RG `has_any_master_effect` -> 1 hit: the definition
  - CORPUS `has_any_master_effect` -> 0 project files (loading check only)
  - GIT `has_any_master_effect` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `find_master_effect` (DEAD fn, crates/manifold-core/src/settings.rs:421)
  - RG `find_master_effect` -> 1 hit: the definition
  - CORPUS `find_master_effect` -> 0 project files (loading check only)
  - GIT `find_master_effect` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `find_master_effect_group` (DEAD fn, crates/manifold-core/src/settings.rs:431)
  - RG `find_master_effect_group` -> 1 hit: the definition
  - CORPUS `find_master_effect_group` -> 0 project files (loading check only)
  - GIT `find_master_effect_group` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `add_video_library_path` (DEAD fn, crates/manifold-core/src/settings.rs:440)
  - RG `add_video_library_path` -> 1 hit: the definition
  - CORPUS `add_video_library_path` -> 0 project files (loading check only)
  - GIT `add_video_library_path` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `remove_video_library_path` (DEAD fn, crates/manifold-core/src/settings.rs:445)
  - RG `remove_video_library_path` -> 1 hit: the definition
  - CORPUS `remove_video_library_path` -> 0 project files (loading check only)
  - GIT `remove_video_library_path` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `clear_video_library_paths` (DEAD fn, crates/manifold-core/src/settings.rs:448)
  - RG `clear_video_library_paths` -> 1 hit: the definition
  - CORPUS `clear_video_library_paths` -> 0 project files (loading check only)
  - GIT `clear_video_library_paths` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- All 16 sit in the block headed `Clamped setters matching Unity ProjectSettings.cs`. The fields they clamp are public and the editing commands write them directly (manifold-editing/src/commands/settings.rs:490, 1104 set `project.settings.output_height`), so nobody calls the setters.
- `git log -S` for `set_output_height`, `find_master_effect`, `add_video_library_path`: introduced b59ad6d2e 2026-03-18 (Tier 0 parity remediation) and never changed since: never had a caller.
- Lines: 70.

### engine-006

Claim: 5 Unity-port methods on `RecordingProvenance`, each with only its definition as a hit; private helper `get_source_at_beat` (recording.rs:209) is used only by `try_restore_recorded_tempo_lane`; the `get_source_at_beat` hits in app and playback are an unrelated closure parameter

- `try_get_recorded_tempo_lane` (DEAD fn, crates/manifold-core/src/recording.rs:122)
  - RG `try_get_recorded_tempo_lane` -> 1 hit: the definition
  - CORPUS `try_get_recorded_tempo_lane` -> 0 project files (loading check only)
  - GIT `try_get_recorded_tempo_lane` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `try_restore_recorded_tempo_lane` (DEAD fn, crates/manifold-core/src/recording.rs:172)
  - RG `try_restore_recorded_tempo_lane` -> 1 hit: the definition
  - CORPUS `try_restore_recorded_tempo_lane` -> 0 project files (loading check only)
  - GIT `try_restore_recorded_tempo_lane` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `is_recorded_tempo_lane_equivalent` (DEAD fn, crates/manifold-core/src/recording.rs:230)
  - RG `is_recorded_tempo_lane_equivalent` -> 1 hit: the definition
  - CORPUS `is_recorded_tempo_lane_equivalent` -> 0 project files (loading check only)
  - GIT `is_recorded_tempo_lane_equivalent` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `try_get_recorded_project_bpm` (DEAD fn, crates/manifold-core/src/recording.rs:263)
  - RG `try_get_recorded_project_bpm` -> 1 hit: the definition
  - CORPUS `try_get_recorded_project_bpm` -> 0 project files (loading check only)
  - GIT `try_get_recorded_project_bpm` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `tempo_change_count` (DEAD fn, crates/manifold-core/src/recording.rs:296)
  - RG `tempo_change_count` -> 1 hit: the definition
  - CORPUS `tempo_change_count` -> 0 project files (loading check only)
  - GIT `tempo_change_count` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `get_source_at_beat` (crates/manifold-core/src/recording.rs:209)
  - RG `get_source_at_beat` -> 10 hits: the definition; 1 in the same file (lines 192); 8 in other files (crates/manifold-playback/src/tempo_recorder.rs x6, crates/manifold-app/src/content_thread.rs x2)
  - CORPUS `get_source_at_beat` -> 0 project files (loading check only)
  - GIT `get_source_at_beat` -> 3 commits, newest: 972fe5d1c 2026-10-14 UI invalidation P3b: the content side doors close
- Kept: `has_recorded_tempo_lane` (used by `set_recorded_tempo_lane`), `TempoMap::ensure_default_at_beat_zero` (7 other callers).
- Introduced b59ad6d2e 2026-03-18, never changed since.
- Lines: 114.

### engine-007

Claim: two `SelectionRegion` constructors from the LayerId migration (528761529), 1 hit each (the definition); 0 corpus

- `new_with_ids` (DEAD fn, crates/manifold-core/src/selection.rs:20)
  - RG `new_with_ids` -> 1 hit: the definition
  - CORPUS `new_with_ids` -> 0 project files (loading check only)
  - GIT `new_with_ids` -> 1 commits, newest: 528761529 2026-03-22 refactor: migrate UIState and SelectionRegion to stable LayerId identity
- `set_with_ids` (DEAD fn, crates/manifold-core/src/selection.rs:66)
  - RG `set_with_ids` -> 1 hit: the definition
  - CORPUS `set_with_ids` -> 0 project files (loading check only)
  - GIT `set_with_ids` -> 2 commits, newest: c25987664 2026-03-23 refactor: remove fragile index-based layer identity from SelectionRegion and LayerHeader
- `SelectionRegion` is a UI-state type; the live code builds it by struct literal (see `duplicate_clips_with_region` in editing, itself dead, engine-026).
- Lines: 66.

### engine-008

Claim: 5 `Timeline` methods, 1 hit each; `get_active_clips_at_beat_ref` is named in 4 docs (stale: the only caller of the `_ref` form is gone, the app calls `get_active_clips_at_beat` and `get_clips_in_beat_window_ref`)

- `register_clip_in_lookup` (DEAD fn, crates/manifold-core/src/timeline.rs:122)
  - RG `register_clip_in_lookup` -> 1 hit: the definition
  - CORPUS `register_clip_in_lookup` -> 0 project files (loading check only)
  - GIT `register_clip_in_lookup` -> 1 commits, newest: b65e55fe2 2026-03-13 Phase 1: Rust port of MANIFOLD core engine
- `layer_id_index_map` (DEAD fn, crates/manifold-core/src/timeline.rs:42)
  - RG `layer_id_index_map` -> 1 hit: the definition
  - CORPUS `layer_id_index_map` -> 0 project files (loading check only)
  - GIT `layer_id_index_map` -> 2 commits, newest: 7d7148466 2026-03-30 refactor(types): eliminate f32↔f64 round-trips on Beats/Seconds typed time
- `get_active_clips_at_beat_ref` (DEAD fn, crates/manifold-core/src/timeline.rs:415)
  - RG `get_active_clips_at_beat_ref` -> 10 hits: the definition; 8 in docs (docs/CLIP_BOUNDARY_FRAME_DESIGN.md, docs/CORE_ENGINE_MAP.md, docs/DESIRED_STATE_RECONCILIATION_DESIGN.md x5, docs/CORE_ENGINE_FINDINGS.md); 1 in beads
  - CORPUS `get_active_clips_at_beat_ref` -> 0 project files (loading check only)
  - GIT `get_active_clips_at_beat_ref` -> 4+ commits, newest: 441414484 2026-10-09 Deliver arrangement clip starts across playback intervals
- `insert_existing_layer` (DEAD fn, crates/manifold-core/src/timeline.rs:482)
  - RG `insert_existing_layer` -> 1 hit: the definition
  - CORPUS `insert_existing_layer` -> 0 project files (loading check only)
  - GIT `insert_existing_layer` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `get_start_beat` (DEAD fn, crates/manifold-core/src/timeline.rs:490)
  - RG `get_start_beat` -> 1 hit: the definition
  - CORPUS `get_start_beat` -> 0 project files (loading check only)
  - GIT `get_start_beat` -> 1 commits, newest: f6a17a82b 2026-03-17 feat: port timeline infrastructure from Unity — CoordinateMapper, UIState, cursors, perf HUD, overview strip, split resize
- `rg -n 'get_active_clips_at_beat' crates` -> timeline.rs defs (400, 415), a doc line, and one caller of the non-`_ref` form at manifold-app/src/ui_bridge/context.rs:55.
- BUG-gg64 (mute/solo while paused; CLOSED) and the docs CLIP_BOUNDARY_FRAME_DESIGN.md:18, DESIRED_STATE_RECONCILIATION_DESIGN.md (SHIPPED 2026-08-20), CORE_ENGINE_MAP.md:79 and CORE_ENGINE_FINDINGS.md:294 mention the name as history. None plans to use it.
- Docs to fix in the same landing: CLIP_BOUNDARY_FRAME_DESIGN.md:18 says playback calls it; it does not.
- Lines: 56.

### engine-009

Claim: 5 DEAD `Layer` methods plus `collect_active_clips_at_beat_mut` (the screen counted a doc-comment mention as a use); only a landing doc and a comment name any of them

- `find_clip_index` (DEAD fn, crates/manifold-core/src/layer.rs:802)
  - RG `find_clip_index` -> 1 hit: the definition
  - CORPUS `find_clip_index` -> 0 project files (loading check only)
  - GIT `find_clip_index` -> 1 commits, newest: d6cb34799 2026-03-14 Phase 2: Complete domain core — commands, EditingService, LiveClipManager
- `set_midi_note` (DEAD fn, crates/manifold-core/src/layer.rs:955)
  - RG `set_midi_note` -> 1 hit: the definition
  - CORPUS `set_midi_note` -> 0 project files (loading check only)
  - GIT `set_midi_note` -> 4+ commits, newest: b52225c32 2026-03-21 refactor: native Rust transformation — typed IDs, Edition 2024, perf, code health
- `clear_clips` (DEAD fn, crates/manifold-core/src/layer.rs:978)
  - RG `clear_clips` -> 1 hit: the definition
  - CORPUS `clear_clips` -> 0 project files (loading check only)
  - GIT `clear_clips` -> 2 commits, newest: 1901cb9c2 2026-09-09 chore: suppression discipline + delete 11 verified-dead core fns
- `set_gen_param_base` (DEAD fn, crates/manifold-core/src/layer.rs:1058)
  - RG `set_gen_param_base` -> 2 hits: the definition; 1 in docs (docs/landings/2026-07-05-param-storage-p2.md)
  - CORPUS `set_gen_param_base` -> 0 project files (loading check only)
  - GIT `set_gen_param_base` -> 2 commits, newest: 079cda262 2026-06-02 Preset unification Phase C2: one ChangeGraphParamCommand for both kinds
- `get_gen_param` (DEAD fn, crates/manifold-core/src/layer.rs:846)
  - RG `get_gen_param` -> 2 hits: the definition; 1 in docs (docs/landings/2026-07-05-param-storage-p2.md)
  - CORPUS `get_gen_param` -> 0 project files (loading check only)
  - GIT `get_gen_param` -> 4+ commits, newest: 26dc2b02e 2026-06-07 Preset fork collapse A1: merge the two definition stores into one (#1, fixes #5)
- `collect_active_clips_at_beat_mut` (rerun DEAD fn, crates/manifold-core/src/layer.rs:637)
  - RG `collect_active_clips_at_beat_mut` -> 2 hits: the definition; 1 in comments (crates/manifold-core/src/layer.rs)
  - CORPUS `collect_active_clips_at_beat_mut` -> 0 project files (loading check only)
  - GIT `collect_active_clips_at_beat_mut` -> 1 commits, newest: dcce35701 2026-03-17 feat: scheduling/sync parity phases 3-5 — engine borrow fix, LiveClipManager, tick orchestration
- `set_gen_param_base` and `get_gen_param` are the positional gen-param API kept at the param-storage P2 landing (docs/landings/2026-07-05-param-storage-p2.md:30); nothing calls them now.
- The newest commit that changes the count of the text `clear_clips` is 1901cb9c2 (2026-09-09, delete 11 verified-dead core fns); this fn survived that pass.
- Lines: 49.

### engine-010

Claim: 3 `TimelineClip` methods, 1 hit each (the definition); `set_scale` hits are substring-only in `git log -S`

- `overlaps_with` (DEAD fn, crates/manifold-core/src/clip.rs:199)
  - RG `overlaps_with` -> 1 hit: the definition
  - CORPUS `overlaps_with` -> 0 project files (loading check only)
  - GIT `overlaps_with` -> 1 commits, newest: b65e55fe2 2026-03-13 Phase 1: Rust port of MANIFOLD core engine
- `set_scale` (DEAD fn, crates/manifold-core/src/clip.rs:388)
  - RG `set_scale` -> 1 hit: the definition
  - CORPUS `set_scale` -> 0 project files (loading check only)
  - GIT `set_scale` -> 4+ commits, newest: b90af55d8 2026-10-15 UI_INVALIDATION P5c: the other windows. The graph editor gets its own frame planner: EditorRegion (preview column, card lane, ove
- `set_loop_duration_beats` (DEAD fn, crates/manifold-core/src/clip.rs:393)
  - RG `set_loop_duration_beats` -> 1 hit: the definition
  - CORPUS `set_loop_duration_beats` -> 0 project files (loading check only)
  - GIT `set_loop_duration_beats` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- Lines: 13.

### engine-011

Claim: 3 `TimelineClip` methods called only by their own unit tests (1, 4 and 3 test hits); `warp_ratio` was replaced by the tempo-map ratio in f3bc86cb6 (playback computes `local_bpm / recorded_bpm` inline) while its own doc comment and 3 design docs still describe it as the live ratio

- `set_in_point` (TEST-ONLY fn, crates/manifold-core/src/clip.rs:224)
  - RG `set_in_point` -> 2 hits: the definition; 1 in the same file (lines 493)
  - CORPUS `set_in_point` -> 0 project files (loading check only)
  - GIT `set_in_point` -> 1 commits, newest: 6c5ac86c1 2026-03-17 feat: scheduling/sync parity phases 1-2 — scheduler live slots, active window, data model clamping
- `warp_ratio` (TEST-ONLY fn, crates/manifold-core/src/clip.rs:276)
  - RG `warp_ratio` -> 12 hits: the definition; 4 in the same file (lines 535, 536, 543, 546); 7 in docs (docs/landings/2026-07-17-waveform-tempo-map-breakpoints.md, docs/ABLETON_SHOW_SYNC_DESIGN.md x2, docs/archive/AUDIO_LAYER_DESIGN.md, docs/CORE_ENGINE_F5_SCOPING.md, ...)
  - CORPUS `warp_ratio` -> 0 project files (loading check only)
  - GIT `warp_ratio` -> 4+ commits, newest: f3bc86cb6 2026-08-17 audio-warp: beat-anchored source position (playback + waveform + mixdown) — exact under tempo maps, kills settings.bpm stalenes
- `start_absolute_tick_resolved` (TEST-ONLY fn, crates/manifold-core/src/clip.rs:296)
  - RG `start_absolute_tick_resolved` -> 4 hits: the definition; 3 in the same file (lines 637, 647, 657)
  - CORPUS `start_absolute_tick_resolved` -> 0 project files (loading check only)
  - GIT `start_absolute_tick_resolved` -> 1 commits, newest: 6c5ac86c1 2026-03-17 feat: scheduling/sync parity phases 1-2 — scheduler live slots, active window, data model clamping
- Tests that go: `test_set_in_point_clamps_negative`, `test_warp_ratio_passthrough_when_no_clip_bpm`, `test_warp_ratio_project_over_clip`, and the three `test_start_absolute_tick_resolved_*`. Each tests only the fn it names. The fields `start_absolute_tick` and `has_start_absolute_tick` stay (serialized).
- Seen, not judged: the live ratio is written inline at manifold-playback/src/audio_layer_playback.rs:379 (`local_bpm_at_beat / clip_bpm`); `warp_ratio(project_bpm)` was the project-bpm form.
- Docs naming `warp_ratio` as live: ABLETON_SHOW_SYNC_DESIGN.md:34,131, AUDIO_CLIP_DETECTION_DESIGN.md:100, CORE_ENGINE_F5_SCOPING.md:60. Tooling sweep territory.
- Lines: 88 (estimate).

### engine-012

Claim: 2 `VideoLibrary` methods, 1 hit each; 0 corpus; `rebuild_lookup` stays (3 other callers)

- `find_clip_by_path` (DEAD fn, crates/manifold-core/src/video.rs:99)
  - RG `find_clip_by_path` -> 1 hit: the definition
  - CORPUS `find_clip_by_path` -> 0 project files (loading check only)
  - GIT `find_clip_by_path` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- `remove_missing_clips` (DEAD fn, crates/manifold-core/src/video.rs:105)
  - RG `remove_missing_clips` -> 1 hit: the definition
  - CORPUS `remove_missing_clips` -> 0 project files (loading check only)
  - GIT `remove_missing_clips` -> 1 commits, newest: b59ad6d2e 2026-03-18 feat: implement Tier 0 manifold-core parity remediation (all 7 phases)
- Lines: 19.

### engine-013

Claim: 3 `PresetInstance` helpers; `clone_deep` is a bare `self.clone()`; `has_envelopes` and `has_automation_lanes` are named only in each other's doc comments (the screen counted the comment as a use of `has_envelopes`)

- `has_automation_lanes` (DEAD fn, crates/manifold-core/src/effects/instance.rs:1476)
  - RG `has_automation_lanes` -> 1 hit: the definition
  - CORPUS `has_automation_lanes` -> 0 project files (loading check only)
  - GIT `has_automation_lanes` -> 2 commits, newest: 5f5ed1918 2026-07-22 refactor(core): P2-E S5 — extract instance.rs from effects/mod.rs
- `clone_deep` (DEAD fn, crates/manifold-core/src/effects/instance.rs:505)
  - RG `clone_deep` -> 1 hit: the definition
  - CORPUS `clone_deep` -> 0 project files (loading check only)
  - GIT `clone_deep` -> 2 commits, newest: 5f5ed1918 2026-07-22 refactor(core): P2-E S5 — extract instance.rs from effects/mod.rs
- `has_envelopes` (rerun DEAD fn, crates/manifold-core/src/effects/instance.rs:1449)
  - RG `has_envelopes` -> 2 hits: the definition; 1 in comments (crates/manifold-core/src/effects/instance.rs)
  - CORPUS `has_envelopes` -> 0 project files (loading check only)
  - GIT `has_envelopes` -> 4+ commits, newest: 5f5ed1918 2026-07-22 refactor(core): P2-E S5 — extract instance.rs from effects/mod.rs
- Lines: 14.

### engine-014

Claim: 3 `PresetInstance` getters whose only callers are asserts inside instance.rs tests (2, 2 and 3 hits)

- `find_envelope` (TEST-ONLY fn, crates/manifold-core/src/effects/instance.rs:1444)
  - RG `find_envelope` -> 3 hits: the definition; 2 in the same file (lines 2085, 2125)
  - CORPUS `find_envelope` -> 0 project files (loading check only)
  - GIT `find_envelope` -> 4+ commits, newest: 90825e579 2026-07-22 refactor(core): P2-E S6 — distribute effects test mod beside subject modules
- `has_drivers` (TEST-ONLY fn, crates/manifold-core/src/effects/instance.rs:501)
  - RG `has_drivers` -> 3 hits: the definition; 2 in the same file (lines 1698, 1711)
  - CORPUS `has_drivers` -> 0 project files (loading check only)
  - GIT `has_drivers` -> 4+ commits, newest: 90825e579 2026-07-22 refactor(core): P2-E S6 — distribute effects test mod beside subject modules
- `user_param_count` (TEST-ONLY fn, crates/manifold-core/src/effects/instance.rs:703)
  - RG `user_param_count` -> 4 hits: the definition; 3 in the same file (lines 1945, 1961, 2215)
  - CORPUS `user_param_count` -> 0 project files (loading check only)
  - GIT `user_param_count` -> 4+ commits, newest: 90825e579 2026-07-22 refactor(core): P2-E S6 — distribute effects test mod beside subject modules
- Named gap: the tests (the `duplicated()` test and the user-binding tests) check other behaviour; only their observer line changes, so this is an edit, not a test deletion.
- `user_added_bindings()` is the underlying iterator and stays used.
- Lines: 15.

### engine-015

Claim: 2 Unity-port fns ('Port of Unity PercussionBeatGrid ...'), 1 hit each, introduced in 004688e6e (2026-03-19) and never called

- `onset_to_peak_seconds_clamped` (DEAD fn, crates/manifold-core/src/percussion_analysis.rs:161)
  - RG `onset_to_peak_seconds_clamped` -> 1 hit: the definition
  - CORPUS `onset_to_peak_seconds_clamped` -> 0 project files (loading check only)
  - GIT `onset_to_peak_seconds_clamped` -> 1 commits, newest: 004688e6e 2026-03-19 feat: port PercussionImportOrchestrator (GAP-AUDIO-4)
- `as_generator_binding` (DEAD fn, crates/manifold-core/src/percussion_analysis.rs:535)
  - RG `as_generator_binding` -> 1 hit: the definition
  - CORPUS `as_generator_binding` -> 0 project files (loading check only)
  - GIT `as_generator_binding` -> 1 commits, newest: 004688e6e 2026-03-19 feat: port PercussionImportOrchestrator (GAP-AUDIO-4)
- Lines: 17.

### engine-016

Claim: `PercussionPipelineSettings::reset_to_defaults` is `*self = Self::default()`, 1 hit (definition); the hits for the same name elsewhere are none

- `reset_to_defaults` (DEAD fn, crates/manifold-core/src/percussion_settings.rs:398)
  - RG `reset_to_defaults` -> 1 hit: the definition
  - CORPUS `reset_to_defaults` -> 0 project files (loading check only)
  - GIT `reset_to_defaults` -> 1 commits, newest: 004688e6e 2026-03-19 feat: port PercussionImportOrchestrator (GAP-AUDIO-4)
- Lines: 5.

### engine-017

Claim: `AudioSetup::device_display_name`, 1 hit (definition), newest commit touching the name is 177f5459a (2026-06-17)

- `device_display_name` (DEAD fn, crates/manifold-core/src/audio_setup.rs:406)
  - RG `device_display_name` -> 1 hit: the definition
  - CORPUS `device_display_name` -> 0 project files (loading check only)
  - GIT `device_display_name` -> 1 commits, newest: 177f5459a 2026-06-17 feat(audio): UID device identity + named/grouped channels in UI (Phase 2 + 3)
- Lines: 5.

### engine-018

Claim: 3 `PresetTypeId` aliases with 1 hit each (WATERCOLOR also in a PRIMITIVE_LIBRARY_DESIGN.md note asking for the const); callers spell the ids as strings; 0 corpus files

- `DIGITAL_PLANTS` (DEAD const, crates/manifold-core/src/preset_type_id.rs:134)
  - RG `DIGITAL_PLANTS` -> 1 hit: the definition
  - CORPUS `DIGITAL_PLANTS` -> 0 project files (loading check only)
  - GIT `DIGITAL_PLANTS` -> 4+ commits, newest: c93325022 2026-09-09 core+primitives: delete 4 covered generator twins, add active_elements helper
- `WATERCOLOR` (DEAD const, crates/manifold-core/src/preset_type_id.rs:92)
  - RG `WATERCOLOR` -> 2 hits: the definition; 1 in docs (docs/PRIMITIVE_LIBRARY_DESIGN.md)
  - CORPUS `WATERCOLOR` -> 0 project files (loading check only)
  - GIT `WATERCOLOR` -> 4+ commits, newest: 016f5b85f 2026-10-09 P2 residual: manifold-nodes-image compiles and passes alone; compiler-demanded wiring, testkit-only widenings and gate ownership
- `NODE_GRAPH_TEST` (DEAD const, crates/manifold-core/src/preset_type_id.rs:96)
  - RG `NODE_GRAPH_TEST` -> 1 hit: the definition
  - CORPUS `NODE_GRAPH_TEST` -> 0 project files (loading check only)
  - GIT `NODE_GRAPH_TEST` -> 4+ commits, newest: 322fbf219 2026-06-06 Phase 1: collapse EffectTypeId + GeneratorTypeId into one PresetTypeId
- Deleting the alias does not delete the presets: `DigitalPlants.json` is a bundled preset (used by manifold-nodes/src/bin/freeze_profile.rs by string) and is a content-sweep matter. `NodeGraphTest` is a catalog test fixture (docs/NODE_CATALOG.md:994), referenced by string in io/src/migrations/param_storage_v14.rs:89.
- Lines: 7.

### engine-019

Claim: `PresetTypeId::is_unknown` has one caller, an assert in preset_type_id.rs's own test (the 12 other hits are an unrelated test helper in manifold-audio); `SOFT_FOCUS_GRAPH` is used by one test in manifold-nodes (binding_seed_tests.rs:27)

- `is_unknown` (TEST-ONLY fn, crates/manifold-core/src/preset_type_id.rs:46)
  - RG `is_unknown` -> 14 hits: the definition; 1 in the same file (lines 424); 12 in other files (crates/manifold-audio/src/trained/spec/tests.rs x12)
  - CORPUS `is_unknown` -> 0 project files (loading check only)
  - GIT `is_unknown` -> 4+ commits, newest: 854a72ad5 2026-10-11 Detector Runtime P2: the generic TrainedDetector on contract v2
- `SOFT_FOCUS_GRAPH` (TEST-ONLY const, crates/manifold-core/src/preset_type_id.rs:100)
  - RG `SOFT_FOCUS_GRAPH` -> 2 hits: the definition; 1 in other files (crates/manifold-nodes/tests/contracts/runtime/binding_seed_tests.rs)
  - CORPUS `SOFT_FOCUS_GRAPH` -> 0 project files (loading check only)
  - GIT `SOFT_FOCUS_GRAPH` -> 4+ commits, newest: e09e078ba 2026-07-22 refactor(preset_runtime): P3-R — move 11 test mods out to tests/*.rs (W3-D1)
- Named gap: the `SOFT_FOCUS_GRAPH` edit is in another crate's test (manifold-nodes/tests/contracts/runtime/binding_seed_tests.rs:27).
- Lines: 10.

### engine-020

Claim: `AbletonParamMapping::map_to_param_range` is called by its one test (ableton_mapping.rs:322); its inner `map_value` is called by nothing else; the live trim math is written inline at manifold-playback/src/ableton_bridge.rs:826

- `map_to_param_range` (TEST-ONLY fn, crates/manifold-core/src/ableton_mapping.rs:188)
  - RG `map_to_param_range` -> 2 hits: the definition; 1 in the same file (lines 322)
  - CORPUS `map_to_param_range` -> 0 project files (loading check only)
  - GIT `map_to_param_range` -> 1 commits, newest: 006ed6175 2026-04-04 feat(osc): Ableton Live OSC bridge — backend infrastructure
- `map_value` (ableton_mapping.rs:179) then has only its own tests as callers; it is the same formula as ableton_bridge.rs:826 and macro_bank.rs:575. Seen, not judged: the patterns sweep decides whether the bridge should call `map_value` instead.
- Lines: 22.

### engine-021

Claim: `PresetKind::is_effect` has no `.is_effect()` caller; the 2 'test' hits are a local variable named `is_effect` in manifold-nodes/tests/gpu_proofs/alpha_contract.rs:122 (the screen mis-filed it as TEST-ONLY)

- `is_effect` (TEST-ONLY fn, crates/manifold-core/src/preset_def.rs:35)
  - RG `is_effect` -> 3 hits: the definition; 2 in other files (crates/manifold-nodes/tests/gpu_proofs/alpha_contract.rs x2)
  - CORPUS `is_effect` -> 0 project files (loading check only)
  - GIT `is_effect` -> 4+ commits, newest: 9109d5fe5 2026-10-14 UI invalidation P3a: change stamps at the gateway and one content reaction pass
- `rg -n '\.is_effect\(' crates` -> 0 hits
- Lines: 3.

### engine-022

Claim: `StageLayout::next_output_id` is called by its one test (stage.rs:645-646); the app mints ids elsewhere; added in the multi-display P1 commit 0cb5114f1 and never called

- `next_output_id` (TEST-ONLY fn, crates/manifold-core/src/stage.rs:222)
  - RG `next_output_id` -> 3 hits: the definition; 2 in the same file (lines 645, 646)
  - CORPUS `next_output_id` -> 0 project files (loading check only)
  - GIT `next_output_id` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- MULTI_DISPLAY_DESIGN.md is IN PROGRESS (P3-P5 not built) and does not name this fn; a P3 UI would need an id minter, but it can write the 4 lines then. Grade A on the code; if Peter wants to keep multi-display scaffolding whole, engine-037 and engine-045 are where to say so.
- Lines: 24 (estimate).

### engine-023

Claim: `Project::fork_preset` is called by its test only; `ForkPresetCommand` now mints with `mint_forked_preset_id` ('Bloom 2'), so the module doc in editing/commands/preset.rs:7 that says the command wraps `fork_preset` is stale

- `fork_preset` (rerun TEST-ONLY fn, crates/manifold-core/src/project/presets.rs:251)
  - RG `fork_preset` -> 8 hits: the definition; 2 in the same file (lines 294, 309); 1 in comments (crates/manifold-editing/src/commands/preset.rs); 4 in docs (docs/MODEL_COMMAND_DECOMPOSITION_DESIGN.md, docs/archive/PRESET_FORK_INVENTORY.md, docs/archive/PRESET_INSTANCE_COLLAPSE_PLAN.md x2)
  - CORPUS `fork_preset` -> 0 project files (loading check only)
  - GIT `fork_preset` -> 4+ commits, newest: 002b09e3d 2026-07-22 refactor(core): P-S4b — distribute project test mod beside subjects + test_support fixtures (pure move)
- `mint_embedded_preset_id` stays: app_lifecycle.rs:781 calls it.
- The other test in that module, `mint_forked_preset_id_starts_at_2_and_probes_past_collisions`, stays.
- Lines: 62 (estimate).

### engine-024

Claim: `Color::CLEAR` const has only a prose word in a compositor comment and a design doc; no `Color::CLEAR` use

- `CLEAR` (rerun DEAD const, crates/manifold-core/src/color.rs:31)
  - RG `CLEAR` -> 3 hits: the definition; 1 in comments (crates/manifold-compositor/src/generator_renderer.rs); 1 in docs (docs/LED_STRIPS_DESIGN.md)
  - CORPUS `CLEAR` -> 0 project files (loading check only)
  - GIT `CLEAR` -> 4+ commits, newest: f17d1d4f0 2026-09-30 tests: prune non-renderer suites per the 2026-09-29 audit (3258 -> 3039)
- `rg -n '\bCLEAR\b' crates --glob '*.rs'` -> 2 hits: the definition and the word CLEAR in prose in a compositor comment (generator_renderer.rs:439); no `Color::CLEAR` use
- Lines: 7.

### engine-025

Claim: 3 `EffectClipboard` methods with 1 hit each (+ an archived doc line for `get_paste_clones`); the live path is `copy_selection` and `paste_payload`; 3e09ce40a (2026-09-26, modifier-card unification) removed their app call sites (the diff deletes `effect_clipboard.copy_single`, `copy_all` and `get_paste_clones` calls)

- `copy_single` (DEAD fn, crates/manifold-editing/src/clipboard.rs:27)
  - RG `copy_single` -> 1 hit: the definition
  - CORPUS `copy_single` -> 0 project files (loading check only)
  - GIT `copy_single` -> 3 commits, newest: 3e09ce40a 2026-09-26 Unify modifier cards and make card and group editing reliable
- `copy_all` (DEAD fn, crates/manifold-editing/src/clipboard.rs:33)
  - RG `copy_all` -> 1 hit: the definition
  - CORPUS `copy_all` -> 0 project files (loading check only)
  - GIT `copy_all` -> 3 commits, newest: 3e09ce40a 2026-09-26 Unify modifier cards and make card and group editing reliable
- `get_paste_clones` (DEAD fn, crates/manifold-editing/src/clipboard.rs:97)
  - RG `get_paste_clones` -> 2 hits: the definition; 1 in docs (docs/archive/BUG_BACKLOG_CLOSED.md)
  - CORPUS `get_paste_clones` -> 0 project files (loading check only)
  - GIT `get_paste_clones` -> 4+ commits, newest: 3e09ce40a 2026-09-26 Unify modifier cards and make card and group editing reliable
- Lines: 17.

### engine-026

Claim: 6 `EditingService` helpers ported from C# in be4c7f06d (2026-03-18) with 1 hit each and never called, plus 3 more (`cut_clips`, `split_clips_for_region_move`, `toggle_mute_clips`) that the screen could not flag because the UI-host traits in manifold-ui and manifold-app define methods with the same names; no call goes through `EditingService::` for any of the 9

- `get_effective_selected_clips` (DEAD fn, crates/manifold-editing/src/service.rs:1311)
  - RG `get_effective_selected_clips` -> 1 hit: the definition
  - CORPUS `get_effective_selected_clips` -> 0 project files (loading check only)
  - GIT `get_effective_selected_clips` -> 1 commits, newest: be4c7f06d 2026-03-18 feat: implement Tier 1 manifold-editing + manifold-playback parity remediation
- `select_all_clip_ids` (DEAD fn, crates/manifold-editing/src/service.rs:1345)
  - RG `select_all_clip_ids` -> 1 hit: the definition
  - CORPUS `select_all_clip_ids` -> 0 project files (loading check only)
  - GIT `select_all_clip_ids` -> 1 commits, newest: be4c7f06d 2026-03-18 feat: implement Tier 1 manifold-editing + manifold-playback parity remediation
- `compute_region_from_clip_selection` (DEAD fn, crates/manifold-editing/src/service.rs:1358)
  - RG `compute_region_from_clip_selection` -> 1 hit: the definition
  - CORPUS `compute_region_from_clip_selection` -> 0 project files (loading check only)
  - GIT `compute_region_from_clip_selection` -> 1 commits, newest: be4c7f06d 2026-03-18 feat: implement Tier 1 manifold-editing + manifold-playback parity remediation
- `split_clips_at_beat_batch` (DEAD fn, crates/manifold-editing/src/service.rs:1429)
  - RG `split_clips_at_beat_batch` -> 1 hit: the definition
  - CORPUS `split_clips_at_beat_batch` -> 0 project files (loading check only)
  - GIT `split_clips_at_beat_batch` -> 1 commits, newest: be4c7f06d 2026-03-18 feat: implement Tier 1 manifold-editing + manifold-playback parity remediation
- `get_current_grid_step` (DEAD fn, crates/manifold-editing/src/service.rs:1484)
  - RG `get_current_grid_step` -> 1 hit: the definition
  - CORPUS `get_current_grid_step` -> 0 project files (loading check only)
  - GIT `get_current_grid_step` -> 1 commits, newest: be4c7f06d 2026-03-18 feat: implement Tier 1 manifold-editing + manifold-playback parity remediation
- `duplicate_clips_with_region` (DEAD fn, crates/manifold-editing/src/service.rs:1493)
  - RG `duplicate_clips_with_region` -> 1 hit: the definition
  - CORPUS `duplicate_clips_with_region` -> 0 project files (loading check only)
  - GIT `duplicate_clips_with_region` -> 1 commits, newest: be4c7f06d 2026-03-18 feat: implement Tier 1 manifold-editing + manifold-playback parity remediation
- `cut_clips` (crates/manifold-editing/src/service.rs:1403)
  - RG `cut_clips` -> 5 hits: the definition; 4 in other files (crates/manifold-app/src/input_handler.rs x2, crates/manifold-app/src/input_host.rs, crates/manifold-ui/src/timeline_input_host.rs)
  - CORPUS `cut_clips` -> 0 project files (loading check only)
  - GIT `cut_clips` -> 3 commits, newest: 6370fc9bb 2026-07-04 P1.6: keyboard layer — nudge/cross-layer/split-at-playhead/zoom-to-selection (B14, doc-default bindings)
- `split_clips_for_region_move` (crates/manifold-editing/src/service.rs:1419)
  - RG `split_clips_for_region_move` -> 9 hits: the definition; 5 in other files (crates/manifold-app/src/editing_host.rs, crates/manifold-ui/src/timeline_editing_host.rs, crates/manifold-ui/src/interaction_overlay.rs x3); 3 in docs (docs/UI_INVALIDATION_DESIGN.md x3)
  - CORPUS `split_clips_for_region_move` -> 0 project files (loading check only)
  - GIT `split_clips_for_region_move` -> 4+ commits, newest: 50aad27c1 2026-10-14 UI invalidation P4b: frame planner beside the old flags, gesture previews, selection split
- `toggle_mute_clips` (crates/manifold-editing/src/service.rs:1454)
  - RG `toggle_mute_clips` -> 6 hits: the definition; 4 in other files (crates/manifold-ui/src/timeline_input_host.rs, crates/manifold-app/src/input_host.rs, crates/manifold-app/src/input_handler.rs x2); 1 in beads
  - CORPUS `toggle_mute_clips` -> 0 project files (loading check only)
  - GIT `toggle_mute_clips` -> 3 commits, newest: 6370fc9bb 2026-07-04 P1.6: keyboard layer — nudge/cross-layer/split-at-playhead/zoom-to-selection (B14, doc-default bindings)
- Cascade for the patterns sweep: `get_current_grid_step` is the only user of the `EditingHost` trait (service.rs:17), which has no implementor, so the trait is then unused.
- The 3 masked ones: `rg -n "(EditingService|Self)::NAME\b" crates` -> 0 hits for `cut_clips`, `split_clips_for_region_move` and `toggle_mute_clips`; every other hit of those names is a same-named method on the `TimelineInputHost` or `TimelineEditingHost` traits (manifold-ui/src/timeline_input_host.rs:125,180; timeline_editing_host.rs:218) and their impls in manifold-app (input_host.rs:537,780; editing_host.rs:495) or test hosts. The app impls do not call the service fns: editing_host.rs:506 calls `EditingService::split_clips_at_region_boundaries` instead.
- Scan scope: only service.rs was scanned for names shared with a trait method (43 pub fns; 24 share a name with another fn). Of the shared names, `copy_clips`, `delete_clips`, `duplicate_clips`, `nudge_clips`, `create_clip_at_position`, `enforce_non_overlap`, `execute_batch` have a service-side caller and `move_clip_to_layer` and `paste_clips` are called from inside the file; the other crates were not scanned this way.
- BUG-7facp (audio clip Mute does not silence playback; CLOSED) describes `EditingService::toggle_mute_clips` as applying MuteClipCommand; no code path calls it (the app impl at input_host.rs:780 is the trait method). UI_INVALIDATION_DESIGN.md:45,108,359 name `split_clips_for_region_move` as the TimelineEditingHost method, not the service fn.
- Lines: 204.

### engine-027

Claim: `SetRelightParamCommand::from_current` has no caller (`::from_current(` finds nothing); relight is switched off by RELIGHT_FEATURE_ENABLED (engine-046) but the fn is dead either way

- `from_current` (DEAD fn, crates/manifold-editing/src/commands/effects.rs:644)
  - RG `from_current` -> 1 hit: the definition
  - CORPUS `from_current` -> 0 project files (loading check only)
  - GIT `from_current` -> 4+ commits, newest: ddb8562f4 2026-10-14 P3 C1: live tempo lasts one tick and nothing writes the map outside a recording session (stale Link tempo after peers v
- `rg -n -w 'from_current' crates --glob '*.rs'` -> 1 hit: the definition (a plain substring search also finds unrelated fn names such as `sync_project_bpm_from_current_beat`)
- Lines: 11.

### engine-028

Claim: `with_forced_def_write` has no caller (removed in 05d1fa10e, 2026-09-12); the field `force_def_write` is therefore always false and its two `&& !self.force_def_write` conditions never differ; BUG-6dh6 (modulation and OSC writes bypass the coupled-row path) names it only as a landed fix

- `with_forced_def_write` (DEAD fn, crates/manifold-editing/src/commands/graph/node_edit.rs:823)
  - RG `with_forced_def_write` -> 2 hits: the definition; 1 in beads
  - CORPUS `with_forced_def_write` -> 0 project files (loading check only)
  - GIT `with_forced_def_write` -> 2 commits, newest: 05d1fa10e 2026-09-12 Integrate unified scene-modifier presets and shared card editing
- `force_def_write` (crates/manifold-editing/src/commands/graph/node_edit.rs:790)
  - RG `force_def_write` -> 5 hits: the definition; 4 in the same file (lines 814, 824, 971, 992)
  - CORPUS `force_def_write` -> 0 project files (loading check only)
  - GIT `force_def_write` -> 1 commits, newest: 59951cce9 2026-09-06 Fix coupled-row write: stamped secondaries now dual-write card slot + def
- Named gap: BUG-6dh6 (OPEN, P2, modulation/OSC bypass around the coupled-row path) mentions 'with_forced_def_write (59951cce9, landed bf83042ce)' as history in a comment. Its remaining work is a binding-write hook; if that fix wants a forced def write it can add one back. Cite BUG-6dh6 when the lane lands this.
- This is also a 3.9 case: a bool that only ever takes one value.
- Lines: 18.

### engine-029

Claim: `AddSceneObjectCommand::with_physics_world` is called only by 4 tests; so `physics_body_metadata` is always None in the product and `physics_world_for_scope`, `execute_physics` and `append_physics_scene_object` (physics.rs:6-135) are unreachable except from those tests; e98ddbffe (2026-09-27, scene-object physics made explicit) deleted the two app call sites

- `with_physics_world` (TEST-ONLY fn, crates/manifold-editing/src/commands/graph/scene.rs:172)
  - RG `with_physics_world` -> 6 hits: the definition; 4 in other files (crates/manifold-editing/src/commands/graph/scene/tests.rs x4); 1 in beads
  - CORPUS `with_physics_world` -> 0 project files (loading check only)
  - GIT `with_physics_world` -> 4+ commits, newest: e98ddbffe 2026-09-27 Make scene object physics an explicit content-owned choice
- `physics_world_for_scope` (crates/manifold-editing/src/commands/graph/scene.rs:182)
  - RG `physics_world_for_scope` -> 2 hits: the definition; 1 in the same file (lines 397)
  - CORPUS `physics_world_for_scope` -> 0 project files (loading check only)
  - GIT `physics_world_for_scope` -> 1 commits, newest: ed326c019 2026-09-23 Build Box3D tick sampling, fast contacts, and linked authoring
- `execute_physics` (crates/manifold-editing/src/commands/graph/scene.rs:212)
  - RG `execute_physics` -> 2 hits: the definition; 1 in the same file (lines 399)
  - CORPUS `execute_physics` -> 0 project files (loading check only)
  - GIT `execute_physics` -> 4+ commits, newest: 04f7882ee 2026-10-10 Keep native geometry and sampling policy inside water
- `append_physics_scene_object` (crates/manifold-editing/src/commands/graph/scene/physics.rs:6)
  - RG `append_physics_scene_object` -> 4 hits: the definition; 1 in other files (crates/manifold-editing/src/commands/graph/scene.rs); 2 in docs (docs/PHYSICS_ENGINE_BOUNDARY_DESIGN.md x2)
  - CORPUS `append_physics_scene_object` -> 0 project files (loading check only)
  - GIT `append_physics_scene_object` -> 2 commits, newest: de4ae1be9 2026-09-28 Clear landing checks for grouped imports and fluid assets
- Why ask: Add Object with physics is the authoring path for physics bodies. BUG-nwjg8 (Physics boundary P6a: rigid add, enable, split, duplicate; OPEN, P3) routes 'rigid add, enable, split and duplicate' through the shared physics transaction under docs/PHYSICS_ENGINE_BOUNDARY_DESIGN.md P6a. Peter decides whether the old path goes or is the base for P6a. `first_free_physics_body_slot` stays (duplicate.rs uses it).
- Lines: with_physics_world 9, physics_world_for_scope about 30, execute_physics about 110, append_physics_scene_object 130, two Option fields and the dispatch, plus 4 tests (tests.rs:1125, 2532, 2826, 2921) not measured.
- Lines: 300 (estimate).

### engine-030

Claim: `ChangeGeneratorTypeCommand` has no constructor outside tests (the screen missed it: its `impl` lines and a compositor comment counted as uses); 36249aff4 (2026-10-09) replaced its app caller with `generator_change::build_change`; the 3 `snapshot_gen_*` helpers exist only to feed it in 2 tests

- `ChangeGeneratorTypeCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/settings.rs:677)
  - RG `ChangeGeneratorTypeCommand` -> 12 hits: the definition; 2 in the same file (lines 694, 716); 4 in other files (crates/manifold-editing/src/commands/layer.rs x2, crates/manifold-editing/tests/command_roundtrips.rs x2); 2 in comments (crates/manifold-compositor/src/generator_renderer.rs, crates/manifold-editing/tests/command_roundtrips.rs); 3 in docs (docs/LED_STRIPS_DESIGN.md, docs/archive/PRESET_BROWSER_AUDITION_DESIGN.md x2)
  - CORPUS `ChangeGeneratorTypeCommand` -> 0 project files (loading check only)
  - GIT `ChangeGeneratorTypeCommand` -> 4+ commits, newest: 36249aff4 2026-10-09 Unify scene camera controls and simplify scene authoring surfaces
- `snapshot_gen_params` (TEST-ONLY fn, crates/manifold-core/src/layer.rs:854)
  - RG `snapshot_gen_params` -> 5 hits: the definition; 4 in other files (crates/manifold-editing/tests/command_roundtrips.rs x4)
  - CORPUS `snapshot_gen_params` -> 0 project files (loading check only)
  - GIT `snapshot_gen_params` -> 2 commits, newest: 5620b428b 2026-05-22 generators: clear stale generator_graph override on type change
- `snapshot_gen_drivers` (TEST-ONLY fn, crates/manifold-core/src/layer.rs:861)
  - RG `snapshot_gen_drivers` -> 3 hits: the definition; 2 in other files (crates/manifold-editing/tests/command_roundtrips.rs x2)
  - CORPUS `snapshot_gen_drivers` -> 0 project files (loading check only)
  - GIT `snapshot_gen_drivers` -> 2 commits, newest: 5620b428b 2026-05-22 generators: clear stale generator_graph override on type change
- `snapshot_gen_envelopes` (TEST-ONLY fn, crates/manifold-core/src/layer.rs:866)
  - RG `snapshot_gen_envelopes` -> 3 hits: the definition; 2 in other files (crates/manifold-editing/tests/command_roundtrips.rs x2)
  - CORPUS `snapshot_gen_envelopes` -> 0 project files (loading check only)
  - GIT `snapshot_gen_envelopes` -> 2 commits, newest: 5620b428b 2026-05-22 generators: clear stale generator_graph override on type change
- Tests that go: `change_generator_type_undo_roundtrip` (command_roundtrips.rs:582-621), `change_generator_type_clears_and_restores_graph_override` (638-709), and `change_generator_type_on_led_layer_assigns_and_undo_restores` in commands/layer.rs (about 50 lines).
- Named gap: LED_STRIPS_DESIGN.md:618 plans a test that runs `ChangeGeneratorTypeCommand` on an LED lane; the replacement is `generator_change::build_change` (manifold-app/src/generator_change.rs). Check that function has an equivalent LED-lane test before deleting the layer.rs test.
- Lines: struct+impl 71, docs 5, helpers about 19, tests about 164.
- Lines: 259 (estimate).

### engine-031

Claim: 3 command structs with no constructor outside the audit table in tests/command_roundtrips.rs (1 test hit each); 0 hits in app, ui, playback; added in the March port (d6cb34799 / f10b7e67f) and never wired; the 8 other commands in the file all have callers outside the file and its tests

- `ToggleGroupCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/effect_groups.rs:568)
  - RG `ToggleGroupCommand` -> 4 hits: the definition; 2 in the same file (lines 575, 591); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs)
  - CORPUS `ToggleGroupCommand` -> 0 project files (loading check only)
  - GIT `ToggleGroupCommand` -> 1 commits, newest: d6cb34799 2026-03-14 Phase 2: Complete domain core — commands, EditingService, LiveClipManager
- `ChangeGroupWetDryCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/effect_groups.rs:750)
  - RG `ChangeGroupWetDryCommand` -> 4 hits: the definition; 2 in the same file (lines 757, 773); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs)
  - CORPUS `ChangeGroupWetDryCommand` -> 0 project files (loading check only)
  - GIT `ChangeGroupWetDryCommand` -> 1 commits, newest: d6cb34799 2026-03-14 Phase 2: Complete domain core — commands, EditingService, LiveClipManager
- `ReorderRackCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/effect_groups.rs:803)
  - RG `ReorderRackCommand` -> 5 hits: the definition; 2 in the same file (lines 811, 822); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 1 in comments (crates/manifold-editing/src/commands/effect_groups.rs)
  - CORPUS `ReorderRackCommand` -> 0 project files (loading check only)
  - GIT `ReorderRackCommand` -> 2 commits, newest: da177f375 2026-10-15 UI_INVALIDATION P6: every command runs through the change audit
- Each row in `command_roundtrips.rs` (2561, 2573, 2832) is the only test for it.
- Lines: 229 (estimate).

### engine-032

Claim: 3 clip command structs with no constructor outside tests (`SlipClipCommand` lost its app constructors in 480acf63e (2026-07-08); `ClipEffectsCommand` was added in the March port d6cb34799 and `SwapVideoCommand` in the same port, with no app constructor since); the screen missed them because `impl` lines count as uses

- `SwapVideoCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/clip.rs:721)
  - RG `SwapVideoCommand` -> 11 hits: the definition; 2 in the same file (lines 731, 753); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 1 in comments (crates/manifold-editing/src/commands/clip.rs); 6 in docs (docs/TIMELINE_INGEST_DESIGN.md x6)
  - CORPUS `SwapVideoCommand` -> 0 project files (loading check only)
  - GIT `SwapVideoCommand` -> 2 commits, newest: d00e47bda 2026-07-05 feat(timeline-ingest): P4 replace audio file (ReplaceAudioFileCommand + inspector gesture)
- `SlipClipCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/clip.rs:869)
  - RG `SlipClipCommand` -> 5 hits: the definition; 2 in the same file (lines 875, 885); 2 in other files (crates/manifold-editing/tests/command_roundtrips.rs x2)
  - CORPUS `SlipClipCommand` -> 0 project files (loading check only)
  - GIT `SlipClipCommand` -> 4+ commits, newest: 480acf63e 2026-07-08 fix(ui): unify slider right-click reset as its own gesture — kills per-panel lottery
- `ClipEffectsCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/clip.rs:916)
  - RG `ClipEffectsCommand` -> 4 hits: the definition; 2 in the same file (lines 922, 937); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs)
  - CORPUS `ClipEffectsCommand` -> 0 project files (loading check only)
  - GIT `ClipEffectsCommand` -> 1 commits, newest: d6cb34799 2026-03-14 Phase 2: Complete domain core — commands, EditingService, LiveClipManager
- Named gap: TIMELINE_INGEST_DESIGN.md (lines 38, 82, 209, 264-268) names `SwapVideoCommand` as the shape precedent for `ReplaceAudioFileCommand`; that audio command exists, so the precedent is history. No bead or open design plans a video swap UI.
- Lines: 168 (estimate).

### engine-033

Claim: 2 expose-toggle command structs with no constructor outside tests (7 and 1 test sites); newest commits to change the name count are 05d1fa10e (2026-09-12, shared card editing) and da177f375 (2026-10-15, audit table); design docs name it as the pattern, not as pending work

- `ToggleEffectParamExposeCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/effects.rs:786)
  - RG `ToggleEffectParamExposeCommand` -> 29 hits: the definition; 2 in the same file (lines 861, 882); 14 in other files (crates/manifold-editing/tests/command_roundtrips.rs x7, crates/manifold-app/tests/user_param_bindings_e2e.rs x7); 3 in comments (crates/manifold-editing/tests/command_roundtrips.rs, crates/manifold-app/tests/user_param_bindings_e2e.rs, crates/manifold-node-engine/src/snapshot.rs); 8 in docs (docs/CARD_TARGET_UNIFICATION.md x2, docs/UI_UX_SYSTEM_DESIGN.md, docs/archive/BINDINGS_UNIFICATION_PLAN.md x2, docs/SCENE_MODIFIER_FRAMEWORK_DESIGN.md x3); 1 in beads
  - CORPUS `ToggleEffectParamExposeCommand` -> 0 project files (loading check only)
  - GIT `ToggleEffectParamExposeCommand` -> 4+ commits, newest: 05d1fa10e 2026-09-12 Integrate unified scene-modifier presets and shared card editing
- `ToggleParamExposeCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/effects.rs:1089)
  - RG `ToggleParamExposeCommand` -> 6 hits: the definition; 2 in the same file (lines 1097, 1108); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 2 in docs (docs/archive/PARAM_STORAGE_DESIGN.md x2)
  - CORPUS `ToggleParamExposeCommand` -> 0 project files (loading check only)
  - GIT `ToggleParamExposeCommand` -> 2 commits, newest: da177f375 2026-10-15 UI_INVALIDATION P6: every command runs through the change audit
- Named gap: CARD_TARGET_UNIFICATION.md (status TARGETING-COMPLETE, 'Stage B+' remaining), UI_UX_SYSTEM_DESIGN.md:217 and BUG-6vv7 (Scene Loop remove leaves orphan params, CLOSED) name `ToggleEffectParamExposeCommand`. I did not find which command the card's expose button builds now; the lane should name it before deleting.
- Test lines are the roughest estimate in this file: 7 tests in command_roundtrips.rs from line 1429.
- Lines: 424 (estimate).

### engine-034

Claim: `RemoveEnvelopeCommand` has no constructor outside the audit table (1 test hit); the automation `RemoveLaneCommand` copies its shape (comments in automation.rs:409, 591)

- `RemoveEnvelopeCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/envelopes.rs:57)
  - RG `RemoveEnvelopeCommand` -> 6 hits: the definition; 2 in the same file (lines 63, 73); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 2 in comments (crates/manifold-editing/src/commands/automation.rs x2)
  - CORPUS `RemoveEnvelopeCommand` -> 0 project files (loading check only)
  - GIT `RemoveEnvelopeCommand` -> 3 commits, newest: 3abc998e0 2026-10-15 UI_INVALIDATION P6: Command::touches has no default
- Named gap: AUTOMATION_LANES_DESIGN is an active feature area; the lane command mirrors this one only in comments.
- Lines: 58 (estimate).

### engine-035

Claim: `SetLayerAnalysisOnlyCommand` has no constructor outside the audit table (1 test hit); added with the analysis-only output state (986a00e61, 2026-06-19)

- `SetLayerAnalysisOnlyCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/layer.rs:903)
  - RG `SetLayerAnalysisOnlyCommand` -> 5 hits: the definition; 2 in the same file (lines 909, 915); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 1 in comments (crates/manifold-editing/src/commands/layer.rs)
  - CORPUS `SetLayerAnalysisOnlyCommand` -> 0 project files (loading check only)
  - GIT `SetLayerAnalysisOnlyCommand` -> 3 commits, newest: 3abc998e0 2026-10-15 UI_INVALIDATION P6: Command::touches has no default
- Named gap: I did not find the commit that removed its app caller (if it ever had one); `git log -S` shows only edits to the struct.
- Lines: 52 (estimate).

### engine-036

Claim: `SetSelectionRegionCommand` has no constructor outside its own test (1 hit); selection region is UI state and the app never sends it as a command

- `SetSelectionRegionCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/selection.rs:9)
  - RG `SetSelectionRegionCommand` -> 5 hits: the definition; 4 in the same file (lines 14, 23, 40, 59)
  - CORPUS `SetSelectionRegionCommand` -> 0 project files (loading check only)
  - GIT `SetSelectionRegionCommand` -> 2 commits, newest: da177f375 2026-10-15 UI_INVALIDATION P6: every command runs through the change audit
- Named gap: selection.rs has a second `impl SetSelectionRegionCommand` block (line 40) whose purpose I did not read in full.
- Lines: 50 (estimate).

### engine-037

Claim: 9 multi-display stage commands built in the P1 commit 0cb5114f1 (2026-07-03) with no constructor outside their own tests; MULTI_DISPLAY_DESIGN.md is IN PROGRESS and P3-P5 (the UI) are not built

- `AddDisplayPlacementCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:23)
  - RG `AddDisplayPlacementCommand` -> 5 hits: the definition; 3 in the same file (lines 27, 33, 394); 1 in comments (crates/manifold-editing/src/commands/stage.rs)
  - CORPUS `AddDisplayPlacementCommand` -> 0 project files (loading check only)
  - GIT `AddDisplayPlacementCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `RemoveDisplayPlacementCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:60)
  - RG `RemoveDisplayPlacementCommand` -> 4 hits: the definition; 3 in the same file (lines 65, 71, 411)
  - CORPUS `RemoveDisplayPlacementCommand` -> 0 project files (loading check only)
  - GIT `RemoveDisplayPlacementCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `RenameDisplayPlacementCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:96)
  - RG `RenameDisplayPlacementCommand` -> 4 hits: the definition; 3 in the same file (lines 102, 112, 438)
  - CORPUS `RenameDisplayPlacementCommand` -> 0 project files (loading check only)
  - GIT `RenameDisplayPlacementCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `SetDisplayPlacementTransformCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:134)
  - RG `SetDisplayPlacementTransformCommand` -> 4 hits: the definition; 3 in the same file (lines 142, 160, 443)
  - CORPUS `SetDisplayPlacementTransformCommand` -> 0 project files (loading check only)
  - GIT `SetDisplayPlacementTransformCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `SetDisplayPhysicalSizeCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:184)
  - RG `SetDisplayPhysicalSizeCommand` -> 4 hits: the definition; 3 in the same file (lines 190, 200, 450)
  - CORPUS `SetDisplayPhysicalSizeCommand` -> 0 project files (loading check only)
  - GIT `SetDisplayPhysicalSizeCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `SetDisplayNativeResolutionCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:221)
  - RG `SetDisplayNativeResolutionCommand` -> 4 hits: the definition; 3 in the same file (lines 227, 237, 455)
  - CORPUS `SetDisplayNativeResolutionCommand` -> 0 project files (loading check only)
  - GIT `SetDisplayNativeResolutionCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `SetDisplayEnabledCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:259)
  - RG `SetDisplayEnabledCommand` -> 4 hits: the definition; 3 in the same file (lines 265, 275, 460)
  - CORPUS `SetDisplayEnabledCommand` -> 0 project files (loading check only)
  - GIT `SetDisplayEnabledCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `SetDisplayIdentityCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:297)
  - RG `SetDisplayIdentityCommand` -> 4 hits: the definition; 3 in the same file (lines 303, 317, 465)
  - CORPUS `SetDisplayIdentityCommand` -> 0 project files (loading check only)
  - GIT `SetDisplayIdentityCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `SetDisplayAdvancedCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/stage.rs:340)
  - RG `SetDisplayAdvancedCommand` -> 4 hits: the definition; 3 in the same file (lines 346, 352, 473)
  - CORPUS `SetDisplayAdvancedCommand` -> 0 project files (loading check only)
  - GIT `SetDisplayAdvancedCommand` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- Why ask: this is scaffolding for a feature Peter is building toward (two LED totems on stage). Deleting forces a rewrite in P3. Option A keep until P3 lands, option B delete and rebuild from the design.
- Lines: 415 (estimate).

### engine-038

Claim: 7 Session Mode commands (P3, 9a069aa4e, 2026-07-03) with no constructor outside their own tests; SESSION_MODE_DESIGN.md is IN PROGRESS: P4 (UI) and P5 not built, and the doc lists these commands by name

- `AddSceneCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/session_commands.rs:38)
  - RG `AddSceneCommand` -> 5 hits: the definition; 2 in the same file (lines 43, 52); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 1 in docs (docs/SESSION_MODE_DESIGN.md)
  - CORPUS `AddSceneCommand` -> 0 project files (loading check only)
  - GIT `AddSceneCommand` -> 1 commits, newest: 9a069aa4e 2026-07-03 feat(session): P3 — grid-editing commands + timeline↔session capture/paste
- `RemoveSceneCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/session_commands.rs:81)
  - RG `RemoveSceneCommand` -> 7 hits: the definition; 2 in the same file (lines 88, 99); 2 in other files (crates/manifold-editing/tests/command_roundtrips.rs x2); 2 in docs (docs/archive/BUG_BACKLOG_CLOSED.md, docs/SESSION_MODE_DESIGN.md)
  - CORPUS `RemoveSceneCommand` -> 0 project files (loading check only)
  - GIT `RemoveSceneCommand` -> 2 commits, newest: 3abc998e0 2026-10-15 UI_INVALIDATION P6: Command::touches has no default
- `RenameSceneCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/session_commands.rs:145)
  - RG `RenameSceneCommand` -> 5 hits: the definition; 2 in the same file (lines 151, 161); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 1 in docs (docs/SESSION_MODE_DESIGN.md)
  - CORPUS `RenameSceneCommand` -> 0 project files (loading check only)
  - GIT `RenameSceneCommand` -> 1 commits, newest: 9a069aa4e 2026-07-03 feat(session): P3 — grid-editing commands + timeline↔session capture/paste
- `ReorderSceneCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/session_commands.rs:193)
  - RG `ReorderSceneCommand` -> 6 hits: the definition; 2 in the same file (lines 198, 207); 2 in other files (crates/manifold-editing/tests/command_roundtrips.rs x2); 1 in docs (docs/SESSION_MODE_DESIGN.md)
  - CORPUS `ReorderSceneCommand` -> 0 project files (loading check only)
  - GIT `ReorderSceneCommand` -> 2 commits, newest: 3abc998e0 2026-10-15 UI_INVALIDATION P6: Command::touches has no default
- `SetSlotCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/session_commands.rs:227)
  - RG `SetSlotCommand` -> 8 hits: the definition; 2 in the same file (lines 234, 245); 4 in other files (crates/manifold-editing/tests/command_roundtrips.rs x4); 1 in docs (docs/SESSION_MODE_DESIGN.md)
  - CORPUS `SetSlotCommand` -> 0 project files (loading check only)
  - GIT `SetSlotCommand` -> 2 commits, newest: 3abc998e0 2026-10-15 UI_INVALIDATION P6: Command::touches has no default
- `CaptureRangeToSceneCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/session_commands.rs:309)
  - RG `CaptureRangeToSceneCommand` -> 11 hits: the definition; 2 in the same file (lines 321, 333); 3 in other files (crates/manifold-editing/tests/command_roundtrips.rs x3); 3 in comments (crates/manifold-editing/tests/command_roundtrips.rs x2, crates/manifold-editing/src/commands/session_commands.rs); 2 in docs (docs/SESSION_MODE_DESIGN.md x2)
  - CORPUS `CaptureRangeToSceneCommand` -> 0 project files (loading check only)
  - GIT `CaptureRangeToSceneCommand` -> 1 commits, newest: 9a069aa4e 2026-07-03 feat(session): P3 — grid-editing commands + timeline↔session capture/paste
- `PasteSlotToTimelineCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/session_commands.rs:460)
  - RG `PasteSlotToTimelineCommand` -> 6 hits: the definition; 2 in the same file (lines 472, 483); 2 in other files (crates/manifold-editing/tests/command_roundtrips.rs x2); 1 in docs (docs/SESSION_MODE_DESIGN.md)
  - CORPUS `PasteSlotToTimelineCommand` -> 0 project files (loading check only)
  - GIT `PasteSlotToTimelineCommand` -> 1 commits, newest: 9a069aa4e 2026-07-03 feat(session): P3 — grid-editing commands + timeline↔session capture/paste
- Why ask: the design names all seven; they are the model half of a performance surface. Option A keep until P4, option B delete.
- Lines: 617 (estimate).

### engine-039

Claim: 2 audio-send commands with no constructor outside tests; the audio setup docs name them as the pattern for upcoming work (LIVE_AUDIO_TRIGGERS_DESIGN.md:210 'mirrors SetAudioSendAnalysisCommand', AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md:640 'SetAudioSendChannelsCommand already carries any channel vec')

- `SetAudioSendChannelsCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/audio_setup.rs:148)
  - RG `SetAudioSendChannelsCommand` -> 5 hits: the definition; 3 in the same file (lines 154, 160, 489); 1 in docs (docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md)
  - CORPUS `SetAudioSendChannelsCommand` -> 0 project files (loading check only)
  - GIT `SetAudioSendChannelsCommand` -> 4+ commits, newest: da177f375 2026-10-15 UI_INVALIDATION P6: every command runs through the change audit
- `SetAudioSendAnalysisCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/audio_setup.rs:289)
  - RG `SetAudioSendAnalysisCommand` -> 6 hits: the definition; 3 in the same file (lines 295, 301, 497); 1 in comments (crates/manifold-editing/src/commands/stage.rs); 1 in docs (docs/LIVE_AUDIO_TRIGGERS_DESIGN.md)
  - CORPUS `SetAudioSendAnalysisCommand` -> 0 project files (loading check only)
  - GIT `SetAudioSendAnalysisCommand` -> 3 commits, newest: da177f375 2026-10-15 UI_INVALIDATION P6: every command runs through the change audit
- Why ask: two live-audio design docs rely on them; I did not check whether those designs are active.
- Lines: 82 (estimate).

### engine-040

Claim: `SetLaneEnabledCommand` has no production UI caller (AUTOMATION_AUTHORING_AUDIT.md item 6 says so); AUTOMATION_LANES_DESIGN.md:202 lists it as part of the lane command set

- `SetLaneEnabledCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/automation.rs:473)
  - RG `SetLaneEnabledCommand` -> 7 hits: the definition; 3 in the same file (lines 481, 498, 1296); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 2 in docs (docs/AUTOMATION_AUTHORING_AUDIT.md, docs/AUTOMATION_LANES_DESIGN.md)
  - CORPUS `SetLaneEnabledCommand` -> 0 project files (loading check only)
  - GIT `SetLaneEnabledCommand` -> 2 commits, newest: 3abc998e0 2026-10-15 UI_INVALIDATION P6: Command::touches has no default
- Why ask: the audit names the missing UI as a gap to fix, not a command to remove.
- Lines: 82 (estimate).

### engine-041

Claim: `RescaleBeatsForBpmChangeCommand` is built only by the audit table, while CORE_ENGINE_MAP.md:176 says a BPM change rescales the timeline through it; no app, playback or ui code builds it (last app-side touch 290615ba6, 2026-10-09)

- `RescaleBeatsForBpmChangeCommand` (rerun TEST-ONLY struct, crates/manifold-editing/src/commands/settings.rs:1275)
  - RG `RescaleBeatsForBpmChangeCommand` -> 5 hits: the definition; 2 in the same file (lines 1282, 1314); 1 in other files (crates/manifold-editing/tests/command_roundtrips.rs); 1 in docs (docs/CORE_ENGINE_MAP.md)
  - CORPUS `RescaleBeatsForBpmChangeCommand` -> 0 project files (loading check only)
  - GIT `RescaleBeatsForBpmChangeCommand` -> 3 commits, newest: da177f375 2026-10-15 UI_INVALIDATION P6: every command runs through the change audit
- Why ask: this may be a wiring hole, not slop. Either the doc is stale (BPM change deliberately no longer rescales) or a BPM edit silently keeps clips at their old beats. Peter decides; file a bug if the second.
- Lines: 76 (estimate).

### engine-042

Claim: 2 Tier-2 parity fns with 1 hit each (introduced 5641ff9a3, 2026-03-18, never called); `ProjectInfo` and its `Display` impl (manifest.rs:66-90) are used only by `get_project_info`

- `is_valid_project_file` (DEAD fn, crates/manifold-io/src/archive.rs:262)
  - RG `is_valid_project_file` -> 1 hit: the definition
  - CORPUS `is_valid_project_file` -> 0 project files (loading check only)
  - GIT `is_valid_project_file` -> 1 commits, newest: 5641ff9a3 2026-03-18 feat: implement Tier 2 manifold-io parity remediation
- `get_project_info` (DEAD fn, crates/manifold-io/src/archive.rs:271)
  - RG `get_project_info` -> 1 hit: the definition
  - CORPUS `get_project_info` -> 0 project files (loading check only)
  - GIT `get_project_info` -> 1 commits, newest: 5641ff9a3 2026-03-18 feat: implement Tier 2 manifold-io parity remediation
- `ProjectInfo` (rerun CRATE-ONLY struct, crates/manifold-io/src/manifest.rs:70)
  - RG `ProjectInfo` -> 7 hits: the definition; 1 in the same file (lines 78); 3 in other files (crates/manifold-io/src/archive.rs x3); 1 in comments (crates/manifold-io/src/manifest.rs); 1 in docs (docs/PROJECT_IO_MAP.md)
  - CORPUS `ProjectInfo` -> 0 project files (loading check only)
  - GIT `ProjectInfo` -> 1 commits, newest: 5641ff9a3 2026-03-18 feat: implement Tier 2 manifold-io parity remediation
- `read_manifest` stays (loader.rs:45, app_lifecycle.rs:1057, tests).
- Lines: 52.

### engine-043

Claim: `load_project_snapshot` (a 3-line wrapper over `load_project_snapshot_with(path, hash, |_| {})`) is called by 3 sites in tests/history_snapshots.rs; the app calls only the `_with` form (app_lifecycle.rs:1097, 1126); PROJECT_IO_MAP.md names it twice

- `load_project_snapshot` (rerun TEST-ONLY fn, crates/manifold-io/src/loader.rs:100)
  - RG `load_project_snapshot` -> 7 hits: the definition; 3 in other files (crates/manifold-io/tests/history_snapshots.rs x3); 1 in comments (crates/manifold-io/src/loader.rs); 2 in docs (docs/PROJECT_IO_MAP.md x2)
  - CORPUS `load_project_snapshot` -> 0 project files (loading check only)
  - GIT `load_project_snapshot` -> 2 commits, newest: b2f787251 2026-07-06 fix(params): dead-LFO-on-reload — register project-local presets BEFORE layer deserialize + keep-dont-drop backstop
- `load_project_snapshot_with` (crates/manifold-io/src/loader.rs:107)
  - RG `load_project_snapshot_with` -> 8 hits: the definition; 1 in the same file (lines 101); 2 in other files (crates/manifold-app/src/app_lifecycle.rs x2); 4 in docs (docs/archive/PARAM_STORAGE_BOUNDARIES_DESIGN.md x3, docs/archive/BUG_BACKLOG_CLOSED.md)
  - CORPUS `load_project_snapshot_with` -> 0 project files (loading check only)
  - GIT `load_project_snapshot_with` -> 1 commits, newest: b2f787251 2026-07-06 fix(params): dead-LFO-on-reload — register project-local presets BEFORE layer deserialize + keep-dont-drop backstop
- Found only by the rerun that ignores comment hits (a doc comment on `load_project_snapshot_with` names it).
- Named gap: PROJECT_IO_MAP.md:96,144 describe it as the restore entry point.
- Lines: 9 (estimate).

### engine-044

Claim: `PRESET_FILE_EXTENSION` is used once, to name a temp file in the module's own test; user_library.rs names preset files `{id}.json` (user_library.rs:102) and the only other `manifoldpreset` literal is a bin (render_import.rs:1067); 0 corpus

- `PRESET_FILE_EXTENSION` (TEST-ONLY const, crates/manifold-io/src/preset_file.rs:20)
  - RG `PRESET_FILE_EXTENSION` -> 2 hits: the definition; 1 in the same file (lines 172)
  - CORPUS `PRESET_FILE_EXTENSION` -> 0 project files (loading check only)
  - GIT `PRESET_FILE_EXTENSION` -> 1 commits, newest: f116fdbe6 2026-06-06 Preset collapse Phase 6a: delete the Generator trait + Rust-factory fork
- Lines: 3.

### engine-045

Claim: the whole module has no user outside itself: the 4 DEAD or TEST-ONLY rows plus `serialize_venue`, `deserialize_venue` and `VenueFileError` that only they use; MULTI_DISPLAY_DESIGN.md (IN PROGRESS) decides venue profiles as 'exportable/importable as a standalone venue file'

- `export_venue` (TEST-ONLY fn, crates/manifold-io/src/venue_file.rs:58)
  - RG `export_venue` -> 2 hits: the definition; 1 in the same file (lines 123)
  - CORPUS `export_venue` -> 0 project files (loading check only)
  - GIT `export_venue` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `import_venue` (TEST-ONLY fn, crates/manifold-io/src/venue_file.rs:64)
  - RG `import_venue` -> 2 hits: the definition; 1 in the same file (lines 124)
  - CORPUS `import_venue` -> 0 project files (loading check only)
  - GIT `import_venue` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `VENUE_FILE_EXTENSION` (TEST-ONLY const, crates/manifold-io/src/venue_file.rs:22)
  - RG `VENUE_FILE_EXTENSION` -> 2 hits: the definition; 1 in the same file (lines 122)
  - CORPUS `VENUE_FILE_EXTENSION` -> 0 project files (loading check only)
  - GIT `VENUE_FILE_EXTENSION` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `serialize_venue` (rerun CRATE-ONLY fn, crates/manifold-io/src/venue_file.rs:48)
  - RG `serialize_venue` -> 5 hits: the definition; 4 in the same file (lines 59, 109, 114, 140)
  - CORPUS `serialize_venue` -> 0 project files (loading check only)
  - GIT `serialize_venue` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `deserialize_venue` (rerun CRATE-ONLY fn, crates/manifold-io/src/venue_file.rs:53)
  - RG `deserialize_venue` -> 5 hits: the definition; 4 in the same file (lines 66, 110, 132, 141)
  - CORPUS `deserialize_venue` -> 0 project files (loading check only)
  - GIT `deserialize_venue` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- `VenueFileError` (rerun CRATE-ONLY enum, crates/manifold-io/src/venue_file.rs:26)
  - RG `VenueFileError` -> 12 hits: the definition; 11 in the same file (lines 35, 45, 48, 49, 53, 54, ...)
  - CORPUS `VenueFileError` -> 0 project files (loading check only)
  - GIT `VenueFileError` -> 1 commits, newest: 0cb5114f1 2026-07-03 feat(multi-display): P1 — stage core model + derive_stage + venue import/export
- Why ask: same multi-display scaffolding as engine-037. 144 lines in the file plus `pub mod venue_file;` in lib.rs. The file has 4 tests (round trip, disk round trip, malformed JSON, empty layout).
- Lines: 145.

### engine-046

Claim: const bool `RELIGHT_FEATURE_ENABLED = false` since 2026-07-18 (blame 50a9da35d9); read at core effects/instance.rs:270, ui param_card/mod.rs:3380, ui param_card/render.rs (6 places), and in 3 test files (manifold-nodes topology_hash_tests.rs and runtime_chain_fusion.rs, manifold-node-engine topology_hash_tests.rs); kept for the patterns sweep

- `RELIGHT_FEATURE_ENABLED`
  - RG `RELIGHT_FEATURE_ENABLED` -> 30 hits: 16 in other files (crates/manifold-foundation/src/lib.rs, crates/manifold-foundation/src/feature_flags.rs, crates/manifold-core/src/effects/instance.rs, crates/manifold-node-engine/src/runtime/topology_hash_tests.rs, ...); 10 in comments (crates/manifold-core/src/effects/instance.rs, crates/manifold-ui/src/panels/param_card/render.rs x4, crates/manifold-nodes/tests/contracts/runtime/topology_hash_tests.rs, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/runtime_chain_fusion.rs x4); 4 in docs (docs/SIMPLIFICATION_CENSUS.md x2, docs/evidence/simplification_census_2026-10/graph.md, docs/evidence/simplification_census_2026-10/graph.proof.md)
  - CORPUS `RELIGHT_FEATURE_ENABLED` -> 0 project files (loading check only)
  - GIT `RELIGHT_FEATURE_ENABLED` -> 4+ commits, newest: bba59955a 2026-10-08 P1 review fixes: tests whose subject is a real node or preset are restored with their original nodes and live renderer-side (tone
- Peter's call per the census pilot: delete depth relight or turn it back on. Not judged here. engine-027 (`from_current`) is a dead helper in the same feature.
- Lines: 0.

### engine-047

Claim: env knob `MANIFOLD_DEVICE_LOG` read once (device_guard.rs:82), documented in the module doc, set by nothing in the repo; for the patterns sweep

- `MANIFOLD_DEVICE_LOG`
  - RG `MANIFOLD_DEVICE_LOG` -> 2 hits: 1 in other files (crates/manifold-foundation/src/device_guard.rs); 1 in comments (crates/manifold-foundation/src/device_guard.rs)
  - CORPUS `MANIFOLD_DEVICE_LOG` -> 0 project files (loading check only)
  - GIT `MANIFOLD_DEVICE_LOG` -> 1 commits, newest: 390a13ca4 2026-10-14 No real devices outside the live app: one process-wide gate, default shut
- Lines: 0.

### engine-048

Claim: env knob `MANIFOLD_WORK_TRACE=1` read once (work_count.rs:268); named by UI_INVALIDATION_DESIGN.md as a live-run gate and by app.rs:462 and perf_soak.rs:541 comments; for the patterns sweep

- `MANIFOLD_WORK_TRACE`
  - RG `MANIFOLD_WORK_TRACE` -> 9 hits: 1 in other files (crates/manifold-foundation/src/work_count.rs); 3 in comments (crates/manifold-foundation/src/work_count.rs, crates/manifold-app/src/app.rs, crates/manifold-app/src/perf_soak.rs); 3 in docs (docs/UI_INVALIDATION_DESIGN.md x3); 2 in beads
  - CORPUS `MANIFOLD_WORK_TRACE` -> 0 project files (loading check only)
  - GIT `MANIFOLD_WORK_TRACE` -> 2 commits, newest: c571e674c 2026-10-15 UI_INVALIDATION P6 close-out: allocation residue, ceilings, planner rule, supersession sweep. UI thread: the clock is its own pro
- Lines: 0.

### engine-049

Claim: cargo feature `bench-timing` gates 2 wall-clock benchmarks (params.rs:647, audio_trigger.rs:378); features.tsv count 7 (declaration, 2 cfg uses, comments, a fixture and docs); no gate command passes it, `feature_matrix.py` only builds every non-default feature; for the patterns sweep

- `bench-timing`
  - RG `bench-timing` -> 11 hits: 5 in other files (scripts/fixtures/gate-p1.json, crates/manifold-core/src/params.rs, crates/manifold-core/src/audio_trigger.rs, crates/manifold-core/Cargo.toml x2); 2 in comments (crates/manifold-core/src/params.rs, crates/manifold-core/src/audio_trigger.rs); 2 in docs (docs/archive/BUG_BACKLOG_CLOSED.md x2); 2 in beads
  - CORPUS `bench-timing` -> 0 project files (loading check only)
  - GIT `bench-timing` -> 1 commits, newest: ec14d046d 2026-07-14 fix(test-gate): flaky-gate class /142/144
- Lines: 0.

### step-2-suppressions

- `rg -n 'allow\((dead_code|unused)|expect\(dead_code' crates/manifold-foundation crates/manifold-core crates/manifold-editing crates/manifold-io --glob '*.rs'` -> 0 hits (the only `allow` attributes in the four crates are `clippy::too_many_arguments`, which are not dead-code suppressions). `raw/suppressions.txt` has no entry for these crates either.
- Result: no suppression candidates.

### step-3-machete

- `raw/machete.txt` lists nodes-water, spectral, whitewater, node-engine, nodes-scene, the analyzer plugin, vendored examples and tv-led-mirror. None of manifold-foundation, manifold-core, manifold-editing, manifold-io appear. Nothing to check with the 3.4 commands.

### step-4-features-consts-env

- `raw/features.tsv`: rows for my crates are core `bench-timing` (7), core `change-audit` (15), editing `change-audit` (15), editing `gpu-proofs` (1033); none has a count of 3 or less. `bench-timing` is listed as engine-049 anyway because nothing runs it. foundation and io declare no features (`sed -n '/^\[features\]/,/^\[/p' crates/manifold-{foundation,io}/Cargo.toml` -> empty).
- `raw/const_bools.txt`: one row in my crates, `RELIGHT_FEATURE_ENABLED` (engine-046). `rg -n -w RELIGHT_FEATURE_ENABLED crates --glob '*.rs'` -> read in core instance.rs:270, ui param_card/mod.rs:3380, ui param_card/render.rs (6 conditions), manifold-node-engine topology_hash_tests.rs:100, and 5 conditions in manifold-nodes tests; set once in feature_flags.rs:16.
- Environment: `rg -n 'env::var(_os)?\(' crates/manifold-{foundation,core,editing,io}` (product code) -> `MANIFOLD_DEVICE_LOG` (foundation/src/device_guard.rs:82) and `MANIFOLD_WORK_TRACE` (foundation/src/work_count.rs:268). Everything else is `std::env::temp_dir()` or `env!("CARGO_MANIFEST_DIR")` in tests, `build.rs` reading `CARGO_MANIFEST_DIR`, and `env!("MANIFOLD_CORE_SOURCE_IDENTITY")` set by core's build.rs. engine-047 and engine-048.

### kept-after-check

Screen rows checked and not made candidates (each is a small observer or test helper the tests need, with the reason):
- `is_registered` (core preset_type_registry.rs:201, 3 lines): used by 5 asserts in the module's tests and by manifold-nodes bundled_presets.rs:230 test `every_bundled_preset_appears_in_effect_type_registry`, which pins that every bundled effect shows in the picker registry. Keep.
- `new_simple` (core percussion_analysis.rs:381, 3 lines): constructor used by 5 tests in core, playback (percussion_orchestrator.rs:1421, percussion_planner.rs:224) and editing (clip_detection.rs:138); all are test code. Keep as a test convenience; deleting costs 5 call-site edits.
- `has_audio_mods` (core instance.rs:1332) and `is_layer_fed` (core audio_setup.rs:199): observer predicates used by 2 and 6 test sites across editing and core; `is_layer_fed` is also named in a journey_proof comment. Keep.
- `saved_id` and `forked_id` (editing preset.rs:206, 82): read the minted id after `execute`; 2 and 3 test sites in preset.rs and preset/modifier.rs, which cover fork behaviour. Keep.
- `has_clipboard` (editing service.rs:638): used by 3 asserts including tests/service_integration.rs, which is an external test crate and needs the accessor. Keep.
- `assert_setter_cases` (editing commands/setter_roundtrip.rs:118): test-support module (`pub(crate) mod setter_roundtrip;`, also pulled into tests/command_roundtrips.rs by `#[path]`), used by 5 test modules. Test infrastructure; not listed by rule.

### seen-not-judged-proofs

- `LayerLifecycleCallbacks` (editing command.rs:140) is a DEAD row of kind trait: `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F LayerLifecycleCallbacks` -> 2 hits, the definition and the census doc. Traits go to the patterns sweep.

### module-follow-up

- `module_check.py FILE...` (scratchpad; imports the rerun tool `census_refs_nocomment.py`): lists every unrestricted `pub` item in each file with ast-grep, then for each item runs `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME "/Users/peterkiemann/MANIFOLD - Rust"` and drops the hits in the item's own file, comment hits, a type's own `impl` header lines and doc and bead paths.
- Pass 1, the 44 files holding a DEAD or TEST-ONLY row: the files with no item used outside the file are `foundation/src/uv_transform.rs` (3 items), `io/src/venue_file.rs` (6) and `core/src/mri.rs`. For `mri.rs` the 2 outside hits it shows, for `data_byte_size` and `frame_voxels`, are common words in unrelated crates and a Python tool; the file is not in the module tree at all. The other 41 files each have live items.
- Pass 2, the 49 files holding only CRATE-ONLY rows: no file has all items unused outside it. The lowest are `core/src/group_edit.rs` (2 of 6 items used elsewhere; the rest are helper types of those two fns), `core/src/effects/instance_serde.rs`, `io/src/migrate.rs` and `io/src/migrations/mod.rs`.
- Inner module: `beat_division_helper` (driver.rs:394) is the one whole inner module with no user (engine-004).
- Masked names: ast-grep for `pub fn $N($$$A) $$$R` in `crates/manifold-editing/src/service.rs` gives 43 pub fns, 25 without `self`. `rg -n "(EditingService|Self)::NAME\b" crates` per static fn -> 0 hits for `compute_region_from_clip_selection`, `duplicate_clips_with_region`, `get_current_grid_step`, `select_all_clip_ids`, `split_clips_at_beat_batch`, `split_clips_for_region_move` and `toggle_mute_clips`. `cut_clips` (an instance method) was found the same way: its only other hits are the UI host trait. All nine are engine-026.

