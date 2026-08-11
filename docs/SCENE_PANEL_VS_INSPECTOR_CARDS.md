# Scene panel vs inspector cards

Provenance: RT toggles card-visibility bug, 2026-08-11.

## What each surface is FOR

**Scene Setup panel** (`crates/manifold-ui/src/panels/scene_setup_panel.rs`): the full 3D scene config surface. Shows every node in the scene vocabulary (transform, material, light, camera, atmosphere, render root) grouped by section, so a user can tune the entire scene build without opening the graph editor. Reads the manifest with `SurfaceVisibility::All` — every param becomes a row. The panel applies its own section-based filtering at the UI level.

**Inspector effect card** (`crates/manifold-ui/src/panels/param_card/`): the curated performance surface. Shows only the params the preset author or the scene-exposure system marked as useful for live performance. Reads the manifest with `SurfaceVisibility::CuratedCard` — only `card_visible: true` params become rows. Renders as the same `ParamCardPanel` (with toggle buttons, sliders, modulation drawers, etc.) the inspector uses for effects.

## Where each surface sources its params

Both surfaces read from the same `PresetInstance.params` manifest, built by `build_param_manifest` (`crates/manifold-core/src/effects/instance_serde.rs`).

For scene generators (imported .glb, the scene-builder), the manifest comes from the graph's `preset_metadata.params`, which is populated at load time by `migrate_scene_exposures` (`crates/manifold-core/src/scene_exposure.rs`). That function walks every scene-vocabulary node (the `SCENE_VOCABULARY_TYPE_IDS` list) and calls `stamp_scene_node_exposures_into`, which writes one `ParamSpecDef` + one `BindingDef` per param. The `card_visible` flag on each `ParamSpecDef` is set by calling `card_visible_for(type_id, param_name)` — a hand-curated lookup table in the same file.

The projection from manifest to `ParamSurface` rows is `param_surface` (`crates/manifold-app/src/ui_bridge/projection/cards.rs`). It branches on `SurfaceVisibility`: `CuratedCard` filters to `card_visible: true` params; `All` keeps every param.

The inspector card calls `gen_params_to_surface` with `CuratedCard`. The Scene Setup panel calls it with `All`.

The per-frame value push (`sync_card_values`, same file) feeds the full manifest as an id-keyed channel; the card joins by id (`row_id_index`). A hidden (`card_visible: false`) param simply finds no row — there is no second filter to drift.

## How keyed the two surfaces stay in sync

`RENDER_SCENE_STAMPED_PARAMS` (`crates/manifold-renderer/src/node_graph/scene_exposure.rs`) defines which `node.render_scene` params get stamped. `card_visible_for` (`crates/manifold-core/src/scene_exposure.rs`) defines which stamped params appear on the curated card.

These are two separate tables in two crates. There is no mechanical coupling between them — the stamp writes `card_visible` by calling `card_visible_for`, so a param in the stamp list but NOT in `card_visible_for` gets stamped with `card_visible: false`.

## Rule for adding a new param

For a scene-vocabulary param on an existing type:

1. Add the param name to the type's arm in `card_visible_for` (`crates/manifold-core/src/scene_exposure.rs`). If it is on `node.render_scene`, also add it to `RENDER_SCENE_STAMPED_PARAMS` (`crates/manifold-renderer/src/node_graph/scene_exposure.rs`).

2. If you skip step 1 (card_visible_for), the param will appear in the Scene Setup panel ONLY — it will be invisible on the inspector card. The Scene Setup panel uses `SurfaceVisibility::All` and filters by section, so it sees every stamped param regardless of `card_visible`.

3. If you want the param visible only in the Scene Setup panel and never on the card, add it to the stamp list but omit it from `card_visible_for`. This is deliberate for params like `node.transform_3d`'s scale — they are scene-config, not performance-curation.

Existing projects auto-correct on load: the `migrate_scene_exposures` repair pass 2 re-derives `card_visible_for` for every auto-stamped exposure and overwrites a stale flag. No project re-save needed.

## Failure mode we just lived

Seven params were on `RENDER_SCENE_STAMPED_PARAMS` (stamp list) but only three were in the `node.render_scene` arm of `card_visible_for`. The four missing (`rt_denoise_feed`, `rt_shadows`, `rt_ao`, `rt_gi`) got stamped as real, addressable exposures with `card_visible: false`. The Scene Setup panel showed them as sliders (it reads `All`). The inspector card showed nothing for them (it reads `CuratedCard`, which filters by `card_visible`). The symptom was `rt_reflections` appearing as an ON/OFF toggle on the card while `rt_denoise_feed` — same stamp list, same `is_toggle` metadata — was absent. The two sources (stamp list and curation table) had drifted independently.
