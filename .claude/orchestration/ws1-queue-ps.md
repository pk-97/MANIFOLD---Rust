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
  (1) RowMod home (param_card.rs:234-296 — projection module vs param_surface/state?)
  (2) generator string-param path (ParamCardStringInfo, param_card.rs:118, :3725).

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

- [ ] **P-S1** `param_slider_shared.rs` layer split (PURE MOVE · ONE Sonnet lane · move_identity gate)
      Split `crates/manifold-ui/src/panels/param_slider_shared.rs` (3160) into
      `panels/param_surface/{builders,state,routing,geometry}.rs` per census §4 buckets:
      - builders.rs: all `build_*` + styles + `build_mod_tab_strip` + Surface helpers
      - state.rs: `ParamModState`/`AudioRowState`/`AudioCardState`/`ParamDragState` + all id-bundle structs
      - routing.rs: `*Ids::resolve`, `resolve_audio_config_click`, `enum_value_cell_actions`
      - geometry.rs: `trim_bar_rects`/`target_bar_rect`/`reposition_trim_bars`
      Re-export from a `param_surface/mod.rs` (or keep `param_slider_shared` as facade) preserving paths.
      Inline test mods follow their fns (D7a scaffold class). Does NOT touch escalation points.
      Gate: move_identity residue 0 + clippy/check + nextest(ui,app) + swatch --no-run + no_bespoke_row_infra.

- [ ] **P-S2** extract shared `RowHost` (SEMANTIC · ONE Opus lane · commit-at-checkpoints)
      Lift `ParamCardPanel`'s id-bundle vecs + `row_index` + `reindex_row` + `register_intents`
      + `row_action`/audio/enum row-actions (param_card.rs:472-636, :1876, :3756, :3529-3660, :4489)
      into a `RowHost` struct in `param_surface/`. Re-point `ParamCardPanel` to EMBED it.
      Relight rows stay OUT (ruling). RowMod + generator string-param = ESCALATE if encountered.
      Gate: no_bespoke_row_infra + card/golden/undo + row_dispatch suites (assertions unmodified)
      + widget-tree INV-1..8 green + headless PNG look-oracle. REPORT to team-lead on completion.

- [ ] **P-S3** collapse `SceneCardState` into `RowHost` (SEMANTIC · Opus lane · depends P-S2)
      Delete scene twin `scene_setup_panel.rs:582-930` + fold `properties_row_action` (:2524)
      into `RowHost::row_action`. `pid_at` dies (INV-2). SceneCardState BECOMES RowHost.
      Gate: no_bespoke_row_infra + scene flow subset (`run_ui_flows.py scene-` green)
      + `rg 'pid_at' scene_setup_panel.rs` → 0 + scene undo/golden. REPORT to team-lead on completion.

- [ ] **P-S4** `param_card.rs` render/routing/state split (PURE MOVE · Sonnet lane · after P-S2)
      `param_card/{render,routing,state}.rs` per census §1. Embeds the RowHost field from P-S2.
      Gate: move_identity residue 0 + full card/golden/undo/geometry suites + swatch --no-run.

- [ ] **P-S5** `inspector.rs` host split (PURE MOVE · Sonnet lane · independent of P-S4)
      `inspector/{render,routing,card_drag}.rs` per census §3. No MIGRATE (no bespoke row infra).
      Sequential after P-S4 in slot-0 (one slot); files disjoint from param_card so no content conflict.
      Gate: move_identity residue 0 + inspector drag/scroll/geometry suites (heavy) + swatch --no-run.

- [ ] **P-S6** D9 `--catalog` dump + D10 macros/settings-sliders-onto-hosts (SEMANTIC · Opus lane)
      Catalog = enumeration view over EXISTING dump machinery (NO new protocol, widget-tree §5 rule).
      D10: macros/settings sliders flow through `param_surface` hosts (zero unsanctioned row paths).
      Gate: `--catalog` self-test + flow suite. Draft the P-S headline amendment in close report.

## Reporting triggers (to team-lead): P-S2 done, P-S3 done, 3+ parks, cannot proceed, ~400K ctx rotation, full-queue close.
