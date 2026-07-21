# ws1-queue-ps.md — P-S (surface split + RowHost dedup), lane/ws1-surface

Dispatcher seat: ws1-ps-dispatch (Opus). Lane: `lane/ws1-surface`, slot-0.
Base tip: a5bcaaf1 (== origin/main @ acquire, P-I landed, D-37). Verified.
Charter: orchestrate, verify by exit code, park friction, NEVER land, NEVER push.
Queue lives ON THE LANE (D-17). Checkbox ticks ride `.claude/`-only commits (pathspec).

Headline (standing ruling, baked in): P-S is **split-by-layer with ONE RowHost dedup**.
The census's stale-thesis catch is accepted — param_card/inspector/param_slider_shared are
RELOCATE+RUNTIME (net deletion ~0); the only real MIGRATE-AND-DIE is `SceneCardState`.
Design-doc P-S headline amendment is drafted at phase close (my close report).

## Standing rulings (final — from team-lead brief)
- RowHost dedup IS in scope: P-S2 extracts a shared RowHost that ParamCardPanel embeds;
  P-S3 makes SceneCardState BECOME it (~500-800 line kill, all from the twin de-dup).
- Relight rows stay OUT of RowHost (census: not `row_index`'d — `handle_click` special-cases
  them; forcing them in is invention). Do not migrate `build_relight_rows` into RowHost.
- NAMED ESCALATION POINTS — executing seat PAUSES and asks team-lead via me, never guesses:
  (1) RowMod home (param_card.rs:237 — projection/model vs param_surface/state?)
  (2) generator string-param path (ParamCardStringInfo, param_card.rs:121, :3726).
  DISPATCHER PRE-FRAME (read-only evidence, gathered while P-S1 ran; team-lead still decides):
  - RowMod: defined in param_card.rs but consumed CROSS-LAYER — crate-root `param_surface.rs`
    already embeds it as `ParamRow.modulation: RowMod` (:95) and re-exports it via lib.rs:77;
    also consumed by param_slider_shared (`ParamModState::sync_from_config(&[RowMod])`),
    scene_setup_panel (`RowModulation`/RowMod), inspector tests. It is a shared projection/model
    VM type, NOT param_card-private runtime — evidence points to the crate-root `param_surface`
    MODEL home (with `ParamRow`), NOT RowHost's imperative state. Recommendation to team-lead when
    P-S2 escalates: RowMod stays with the model layer; RowHost does not own it.
  - ParamCardStringInfo: field of crate-root `ParamSurface.string_params` (:123), re-exported via
    lib.rs; generator-only; explicitly OUTSIDE row_index scope (param_card.rs:3726 "separate slot").
    Confirms census — stays OUT of RowHost (like relight rows). Home = model/generator surface.

## Per-slice gates (quote in commit messages; every cargo cmd via .claude/scripts/with-build-lock.sh)
- Pure moves: `python3 scripts/move_identity_check.py <commit>` → residue 0 (scaffold classes established).
- All slices: `cargo clippy -p manifold-ui -p manifold-app --tests -- -D warnings` AND plain `cargo check -p manifold-ui -p manifold-app`.
- All slices: `cargo nextest run -p manifold-ui -p manifold-app`.
- All slices (D-37 renderer-in-scope): `cargo test -p manifold-renderer --test ui_color_swatches --no-run`.
- All slices: widget-tree invariant tests — `cargo nextest run -p manifold-ui --test no_bespoke_row_infra` (+ param_surface INV-1..8 unit tests, assertions UNMODIFIED).
- P-S2 additionally: no_bespoke_row_infra green; card/golden/undo suites green; headless PNG look-oracle.
- P-S3 additionally: scene flow subset — `python3 scripts/run_ui_flows.py scene-` all green; `rg 'pid_at' scene_setup_panel.rs` → 0.
- Dispatcher independently RERUNS each lane's gates before accepting.

## Queue

- [x] **P-S0** precondition: P-I landed. SATISFIED — origin/main @ a5bcaaf1 (D-37). No action.

- [x] **P-S1** DONE — commit `1d724165`, five-file split. move_identity residue 2 (INDEPENDENTLY
      RERUN, matches lane report): both lines are the sanctioned `no_bespoke_row_infra.rs` allowlist
      filename swap `"param_slider_shared.rs"`→`"builders.rs"` — a legitimate sibling-test edit the
      split forces, REVIEWED+ACCEPTED by team-lead (the eyeball the verifier docstring names).
      Build/test gates INDEPENDENTLY GREEN: check=0, clippy(-D warnings)=0, nextest=0 (1172 passed
      incl. no_bespoke_row_infra), renderer ui_color_swatches --no-run=0 (D-37). Carry-forward lessons from this lane
      (team-lead): honest true-number-over-expected residue reporting; ast-grep/tree-sitter span
      derivation for split ranges, NEVER hand-transcribed line numbers — bake into remaining briefs.
      DISPATCHER DECISION (module-name preserved for pure-move property): split
      `crates/manifold-ui/src/panels/param_slider_shared.rs` (3160) into a DIRECTORY module of
      the SAME name — `panels/param_slider_shared/{mod,builders,state,routing,geometry}.rs` — with
      `mod.rs` `pub use`-re-exporting every item so all external `param_slider_shared::X` paths stay
      valid (renaming to `param_surface/` would churn call sites across the crate = residue, blows the
      gate; crate-root `param_surface.rs` is the model layer and imports FROM here, so the builders
      belong in panels). Census §4 buckets:
      - builders.rs: all `build_*` + styles + `build_mod_tab_strip` + Surface helpers
      - state.rs: `ParamModState`/`AudioRowState`/`AudioCardState`/`ParamDragState` + all id-bundle structs
      - routing.rs: `*Ids::resolve`, `resolve_audio_config_click`, `enum_value_cell_actions`
      - geometry.rs: `trim_bar_rects`/`target_bar_rect`/`reposition_trim_bars`
      Inline test mods follow their fns (D7a scaffold class). Does NOT touch escalation points.
      Gate: move_identity residue 0 + clippy/check + nextest(ui,app) + swatch --no-run + no_bespoke_row_infra.

- [x] **P-S2** DONE — RowHost extracted into `param_slider_shared/row_host.rs`, ParamCardPanel embeds it.
      Commits `35b55a41` (CP1: id-bundle machinery) + `bc361cbb` (CP2: row_action + helpers).
      SEAM DECISION (team-lead ACCEPTED): id/routing machinery extracted; per-row MODEL stays
      panel-owned and rides by reference — extract-the-seam, NOT fold-the-rows (folding rows/mod_state
      would be the rewrite the census flagged as risk). Two compile-forced field additions fine
      (the P-S3 twin owns both). NODE_ID_HOARD_ALLOWLIST entry for RowHost = allowlist-DATA consistent
      with INV-5 (RowHost is the sanctioned shared home). Wide `row_action` param list = named honest cost.
      NO escalation fired — RowMod + string_param_btn_ids correctly left on the panel per pre-frame.
      Gates INDEPENDENTLY GREEN: clippy(-D warnings)=0, nextest=0 (1172 passed incl. no_bespoke_row_infra
      + chrome_param_card_proof + param_surface INV + app baseline suites), swatch --no-run=0 (D-37);
      team-lead already accepted incl. look-oracle. REVIEWED + ACCEPTED by team-lead before P-S3.

- [x] **P-S3** DONE — commit `b1213b5e`. SceneCardState embeds+delegates to shared RowHost (symmetry
      ruling honored, RowHost untouched). Deletion gates INDEPENDENTLY PASS: `fn pid_at`→0,
      `mirror.*ParamCardPanel`→0. scene_setup_panel.rs 3585→3305 (~280 net deleted; honest under-estimate
      vs census ~350-450, team-lead prefers honest number). Behavioral-equivalence sound on all three legs
      (all-None osc gating, unreachable role branches, INV-4 salts pinned) — team-lead REVIEWED + ACCEPTED.
      Heavy gates INDEPENDENTLY GREEN: clippy=0, nextest=0 (1172 passed), swatch=0, scene flows 17/17
      passed (1 xfail = pre-existing BUG-239/VD-035 known-red, NOT a regression; 41/41 accounted).
      The two surfaces now share ONE row-host and cannot diverge again.
      TEAM-LEAD RULING (bake into brief): SceneCardState EMBEDS a RowHost matching ParamCardPanel's
      shape — own model fields, delegated machinery. Do NOT fold rows/mod_state into RowHost (host
      SYMMETRY beats param-count; asymmetric ownership between the two hosts = a new divergence axis).
      The param-list collapse is a post-campaign refinement if it ever bites. Delete SceneCardState's
      DUPLICATED `reindex_row`/`register_intents`/audio-helper twins (they now live on RowHost); scene
      delegates to shared `RowHost::row_action` (fold `properties_row_action`'s duplicated logic).
      `pid_at` dies (INV-2). "Mirrors ParamCardPanel" comments die WITH the mirrors.
      DELETION GATE: `rg 'reindex_row|register_intents|pid_at' scene_setup_panel.rs` → 0 duplicated twins;
      `rg 'Mirrors ParamCardPanel' scene_setup_panel.rs` → 0; scene-setup flow subset green via manifest
      runner (`run_ui_flows.py scene-`). Plus clippy/nextest(ui,app)/swatch + scene undo/golden.
      REPORT to team-lead on completion (reviews before P-S4/5 run).

- [x] **P-S4** DONE — commit `d19362a6`. param_card.rs → param_card/{mod(3292),render(1893),routing(783),
      state(571)}.rs. Team-lead ACCEPTED as-is (D-38 economy, no split): move_identity residue = the
      2-line `super::` depth fix (named D-9 wiring) + the 17-line `no_bespoke_row_infra.rs` `is_allowlisted`
      restructure — a FORCED companion edit whose slash-guard (`.replace(MAIN_SEPARATOR,"/")`) stops
      `param_card/mod.rs` from blanket-allowlisting other dir-modules' mod.rs; reviewed + correct. The
      invariant pair passing post-restructure is the proof. Lane's quoted gates green (accepted per D-38,
      no independent rebuild); free move_identity re-confirm matches team-lead's review.

- [x] **P-S5** DONE — commit `4a2aa2b5` (ran PARALLEL in slot-1/lane/ws1-surface-p5 per D-38, lock suspended).
      inspector.rs → inspector/{mod(2772),render(637),routing(416),card_drag(459)}.rs. move_identity
      residue 0 PURE MOVE PROVEN (12 visibility widenings = required wiring). Team-lead ACCEPTED on quoted
      gates (both bucket-deviation judgment calls + single tests-block call sound). Touched ONLY inspector
      files (verified — inspector not on row-infra allowlist, so no shared-file conflict with P-S4).
      MERGED into lane/ws1-surface at `e68e254f` (--no-ff, disjoint → trivial, 4 files). slot-1 RELEASED.
      POST-MERGE combined build GREEN: check=0, clippy=0, nextest=0 (1172 passed), swatch=0 — the one
      place both parallel branches first compile together.

- [ ] **P-S6** D9 `--catalog` dump + D10 macros/settings-sliders-onto-hosts (SEMANTIC · Opus lane)
      Catalog = enumeration view over EXISTING dump machinery (NO new protocol, widget-tree §5 rule).
      D10: macros/settings sliders flow through `param_surface` hosts (zero unsanctioned row paths).
      Gate: `--catalog` self-test + flow suite. Draft the P-S headline amendment in close report.

## Reporting triggers (to team-lead): P-S2 done, P-S3 done, 3+ parks, cannot proceed, ~400K ctx rotation, full-queue close.
