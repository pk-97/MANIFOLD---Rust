# Material inspector — understandable surface authoring

**Status:** IN PROGRESS · 2026-09-22 · GPT-6 · P1–P6 implemented; final landing validation underway.
**Prerequisites:** satisfied. The native Metal proof verifies Opaque transmission routing and separately measurable sheen/translucency contributions (BUG-1c9c, BUG-vj1p).
**Execution contract:** read [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) sections 5–6 before starting a phase. Peter authorized end-to-end implementation on 2026-09-22.

<!-- index: Material inspector: feature state, texture ownership, manifest-backed grouping, compound edits, starter looks, and bounded implementation gates. -->

Peter described the problem as “a huge wall of sliders” where users cannot tell “if something is ‘on’ or valid,” including the “GIANT list of UV transforms.” The target is a small surface section, optional material features, and placement controls beside their textures. “Opacity & Cutout” replaces the prototype's “Coverage.” This improves authoring of the existing material model; it does not introduce a shader graph or promise Blender rendering parity.

Companions: [MATERIAL_SYSTEM_DESIGN.md](MATERIAL_SYSTEM_DESIGN.md) owns material wires and runtime kinds; [GLTF_MATERIAL_EXTENSIONS_DESIGN.md](GLTF_MATERIAL_EXTENSIONS_DESIGN.md) records the extension implementation; [WIDGET_TREE_DESIGN.md](WIDGET_TREE_DESIGN.md) owns parameter projection, shared widgets and routing. Source code takes precedence over historical shader claims in those records.

## 1. Audit — what exists

Verified by source inspection on 2026-09-22 at `bb2376a3b8aa77703190343610a1578e2bba4e35`. This is a dated snapshot, not a rendered-behaviour certification. No app build, runtime reproduction or visual verification was performed for this design. Extend these seams; do not redesign them.

Paths below are repository-relative. Symbol anchors are intentional: executors must re-find them rather than trust historical line numbers.

| Piece | Source anchor | Finding and consequence |
|---|---|---|
| PBR descriptors | `crates/manifold-renderer/src/node_graph/primitives/pbr_material.rs:143` (`params`) | 89 parameters: 13 core, 25 extension, 30 affine-UV scalars, 20 sampler enums, one Baked Look bool. 50 of 89 describe texture placement/sampling. |
| Parameter types | `crates/manifold-renderer/src/node_graph/parameters.rs` (`ParamType`, `ParamDef`) | Color and vector types exist. PBR nevertheless stores colours as separate scalar channels. `ParamDef` lacks semantic grouping and dependencies. |
| Runtime material | `crates/manifold-renderer/src/node_graph/material.rs` (`Material`); `primitives/pbr_material.rs` (`run`) | CPU material output carries factors, five transforms and five samplers. Baked Look emits Unlit. No new GPU material kind is needed for the inspector. |
| Texture ownership | `crates/manifold-renderer/src/node_graph/primitives/scene_object.rs:35` (`primitive!` inputs; generated `SceneObjectNode::INPUTS`) | 17 texture input slots belong to the scene object, not the material node. A material can be shared while object maps differ. |
| Descriptor projection | `crates/manifold-renderer/src/node_graph/scene_exposure.rs` (`metadata_for_node_type`, `migrate_scene_exposures`); `crates/manifold-core/src/scene_exposure.rs` (`SceneParamMetadata`, `stamp_scene_node_exposures`) | Registry ranges/types/defaults are stamped into exposed metadata. Existing bindings are preserved. Material rows are hidden on the curated outer card but available to the Scene dock. |
| Unified UI surface | `crates/manifold-ui/src/param_surface.rs` (`RowSpec`, `ParamRow`, `ParamSurface`); `crates/manifold-app/src/ui_bridge/projection/cards.rs` (`param_surface`) | Real ParamIds, modulation/audio/mapping facts and section metadata already have one projection. `disabled` blocks all gestures; it is unsuitable for merely inactive features. |
| Scene panel | `crates/manifold-ui/src/panels/scene_setup_panel.rs` (`build_object_properties_body`); `crates/manifold-app/src/ui_bridge/projection/scene.rs` (`sync_scene_row_values`, `sections_for_doc_ids`) | Shared row builders and id-keyed value sync exist. A separate Skin row and emission-strength suffix handling need convergence, not another panel layered over them. |
| Material and source identity | `crates/manifold-renderer/src/node_graph/scene_vm.rs` (`MaterialColorRow`, `trace_scene_object`, `resolve_producer_through_group`) | Material VM currently carries node ID/scope/is-PBR, not feature or map state. Producer resolution supports a direct producer and one group-output traversal, not arbitrary nested graphs. |
| Texture readiness | `crates/manifold-renderer/src/node_graph/primitives/gltf_texture_source.rs` (`io_pending`, `warmup_pending`); `node_graph/snapshot.rs` (`ParamSnapshot`) | Loading/upload state exists privately in the runtime, but is absent from current scene snapshots. A wire proves a connection, not readiness. |
| Scalar editing | `crates/manifold-app/src/ui_bridge/project.rs` (`apply_scene_param_write`); `crates/manifold-editing/src/commands/graph/node_edit.rs` (`SetGraphNodeParamCommand`) | Bound edits target instance slots; unbound edits target graph defaults. The existing helper looks up bindings by node doc ID/name without using scope in that lookup. New compound work must not copy this ambiguity across nested scopes. |
| Undo and gestures | `crates/manifold-editing/src/command.rs` (`CompositeCommand`); `crates/manifold-ui/src/panels/scrub.rs` (`ValueRef`, `ScrubPhase`); `crates/manifold-app/src/ui_bridge/scrub.rs` (`ValueRef::Param` arm) | Composite undo and begin/move/commit exist. Scalar scrub sends live changes through `MutateProjectLive`, then one undo command. No grouped scalar RGB scrub exists. |
| Skin editing | `crates/manifold-editing/src/commands/graph/skin.rs` (`SetSceneObjectSkinSourceCommand`, `SetSceneObjectSkinTargetMapCommand`) | Commands preserve displaced graph sources. Emissive Skin temporarily changes emission factors and restores them later; look application must respect this ownership. |
| Persistence | `crates/manifold-core/src/effect_graph_def.rs` (`EffectGraphNode`, `ParamSpecDef`); `crates/manifold-renderer/src/node_graph/persistence.rs` (`from_graph`) | Node params and exposed specs serialize. Runtime serialization materializes defaults, so key presence cannot indicate that a user explicitly added a feature. |
| Independent modulation | `crates/manifold-core/src/effects/instance.rs` (`PresetInstance`) | Drivers, envelopes, audio, mappings and automation live beside the graph. Changing layout must not rename/remove slots. |
| Colour UI | `crates/manifold-ui/src/graph_canvas/interaction.rs` (Color/Vec editing); `graph_canvas/model.rs` (`format_color_hex`) | Graph vector editing exists; there is no reusable ParamSurface RGB picker. Scalar-channel grouping needs a shared row affordance. |
| Layer boundary | `crates/manifold-foundation/src/lib.rs` (crate contract) | Foundation contains primitive vocabulary, explicitly not domain models. Material schemas stay out of foundation; app adapts core semantics to UI facts. |

### Rendering dependencies found by the audit

These are source-backed findings, not reproduced bugs. Fixes require their own bounded reproduction and verification; this document does not certify shader fidelity.

| Finding | Source | Contract for this work |
|---|---|---|
| MR texture replaces scalar roughness/metallic | `primitives/shaders/render_scene.wgsl` (`resolve_mr`) | BUG-7tr9. Show “From texture”; retain authored factors with an explanation that they apply without the map. Do not silently change imported or existing looks to multiplication. |
| Transmission pass depends on Blend as well as transmission | `primitives/render_scene.rs` (`is_transmissive`); `gltf_import/object_group.rs` (`is_glass`) | BUG-1c9c. Manual transmission can miss the refraction pass. Glass UI must wait for a verified renderer contract; toggling Glass must not silently rewrite opacity mode. |
| Transmission assignment follows sheen/translucency additions | `primitives/shaders/render_scene.wgsl` (the `base_rgb` assignment in the transmission branch) | BUG-vj1p. Combined features need a verified composition fix before Glass is offered as reliable authoring. |
| Extension maps lack independent transforms | `primitives/shaders/render_scene.wgsl` (`resolve_sheen`, `resolve_iridescence`, `resolve_anisotropy` and extension samples) | Only the five base map families have independent placement. Extension maps report mesh UV/shared sampling; do not display imaginary per-map controls. |
| Baked and diagnostic views bypass shading | `primitives/pbr_material.rs` (`baked_look`); `primitives/render_scene.rs` (Solid/Wireframe/Points material substitution) | Show “Bypassed by Baked Look” or “Not shown in this preview” separately from a feature's authored state. |
| Emission has two gains | `primitives/shaders/render_scene.wgsl` (`resolve_emissive`); `primitives/render_scene.rs` (object `emission_strength`) | Material intensity and object gain remain distinct addresses, grouped with clear labels. Do not multiply them into a new stored value. |

## 2. Decisions

**D1 — Keep the existing material model.** A selected known PBR material gets Surface, Opacity & Cutout, optional features, Textures and Advanced. Phong/Cel/Unlit retain their own valid controls and never convert automatically. Rejected: a node-editor replacement, because the immediate failure is discoverability within an existing surface model.

**D2 — Feature presence, output permission and expansion are different.** Seven saved feature modes use `FollowValues = 0`, `Off = 1`, `On = 2`. FollowValues preserves legacy behaviour; On records intentional presence; Off gates evaluated output while retaining every authored value and driver. Collapse is local UI state. Rejected: zeroing stored factors to switch off, because it destroys the previous look and automation intent. Rejected: inferring explicit presence from stored keys, because serialization writes defaults.

**D3 — One descriptor authority.** Add semantic roles to the existing manifest path. A single renderer-side role catalog assigns roles to registered material parameters; it contains no duplicate ranges/defaults. Stamping copies roles into `ParamSpecDef`; the existing app projection translates them to UI descriptors. Rejected: suffix-driven UI tables, because graph cards, scene rows and imports would disagree. Consequence: additive serialized presentation metadata and an exhaustive coverage test are required.

**D4 — Explain ineffective controls without deleting authoring access.** “Off”, “From texture”, “Driven”, “Bypassed” and “Connected” mean different things. Use a nonblocking explanatory field for inactive contribution; reserve `disabled` for edits that genuinely cannot be made. An advanced scalar row remains available for preparing dormant values and editing mappings. Never claim pixels contribute merely because a mode is On.

**D5 — Texture-local UI respects split ownership.** Show each connected texture's source and supported placement beside it. Placement still changes the material and therefore every object using that material; assignment still belongs to the object. Display a shared-material notice when reuse is detected. Existing Skin commands remain the only inspector source-assignment mechanism in v1. Arbitrary texture graph rewiring and a new asset browser are deferred.

**D6 — Preserve scalar identities through compound controls.** RGB swatches edit the existing three ParamIds through one gesture/undo transaction. Advanced channels keep their individual modulation controls. No new vector storage or merged automation track. Rejected: replacing RGB scalars with Color params, because that breaks existing bindings and shows.

**D7 — Looks are small, atomic factor recipes.** Ship Matte, Coated and Brushed Metal first; Glass waits for its prerequisites. They preserve base colour, opacity mode, maps, UVs, samplers, Baked Look and object gain. No preset instance, material-library file format or procedural graph is introduced. A recipe is blocked if a changed target is wire-driven, modulated/automated/mapped, fan-out-bound beyond this material, texture-owned, or temporarily owned by emissive Skin. Reject the whole change with named conflicts; never partially apply a named look.

**D8 — Preserve arbitrary UV matrices.** The six existing affine values remain authoritative. Offer offset/rotation/scale only when the matrix is representable as `R(theta) * diag(sx, sy)` with translation. Shear, singular transforms and driven matrix members get the existing six-value Advanced editor with a reason. Merely opening the inspector never decomposes/recomposes stored values. Rejected: forcing every imported matrix into scale/rotation, because it changes valid authored mappings.

**D9 — Scope is explicit.** Edits to a shared material edit that shared material. Both the header and look action say “applies to N objects”; where users cannot be fully resolved, say “shared scope unknown” and disable looks. No silent material cloning. Arbitrary nested graph producers remain inspectable as “Graph source” with existing graph navigation, without a guessed editable address.

## 3. Authoring behaviour

### Layout and complete parameter assignment

| Section | Existing parameters and behaviour |
|---|---|
| Surface | `color_r/g/b` as a swatch, `metallic`, `roughness`. MR ownership badge sits beside both factors. |
| Opacity & Cutout | `alpha_mode`, `color_a`, `alpha_cutoff`. Label modes Solid, Cutout, Fade while keeping existing stored enum values. Cutoff appears for Cutout; opacity relevance follows actual shader behaviour. Dormant values remain in Advanced. |
| Coat | `clearcoat`, `clearcoat_roughness` plus connected coat maps. |
| Iridescence | `iridescence`, `iridescence_ior`, `iridescence_thickness_min/max`. Explain thin-film colour; minimum thickness matters only with its thickness map. |
| Emission | `emission_r/g/b`, `emission_intensity`; separately addressed scene-object `emission_strength` labelled Object gain. |
| Glass | `transmission`, `volume_thickness`, `volume_attenuation_distance/color_r/g/b`, `dispersion`; show Surface IOR by reference to the same row, never another slot. Explain thickness/IOR dependence without promising visible dispersion at every setting. |
| Sheen | `sheen_color_r/g/b`, `sheen_roughness`. |
| Anisotropy | `anisotropy_strength`, `anisotropy_rotation`. |
| Translucency | `translucency`; help text: light through thin surfaces, not volumetric subsurface scattering. |
| Textures | Five independent families: base (`uv_*`, unprefixed samplers), normal (`nrm_*`), metallic/roughness (`mr_*`), occlusion (`occ_*`), emission (`em_*`). Each owns six UV fields and four sampler fields. All other connected texture ports show source and supported/shared sampling facts. |
| Advanced surface | `ambient`, `specular`, `specular_tint_r/g/b`, `ior`, `baked_look`, plus dormant/raw representations on demand. |

Every original PBR descriptor must be assigned exactly once. References such as Glass → IOR navigate to its canonical control. Unclassified future parameters appear in Advanced and fail the schema coverage test until deliberately classified; they must not disappear.

An untouched neutral feature lives in Add Feature. A feature stays visible when its mode is explicitly Off/On, its controlling authored factor is non-neutral, one of its relevant maps is connected, or one of its members has a wire/driver/envelope/audio/mapping/automation attachment. This is a structural/authored predicate, never a test of the current animated sample; LFO zero crossings do not rearrange the panel. Non-neutral secondary settings alone remain accessible through Add Feature and are never reset.

For FollowValues, the header says “From values,” not “On.” Turning it off writes Off. Turning Off on writes On without seeding. Adding a neutral unused feature writes On and seeds only its neutral controlling values: Coat 1, Iridescence 1, Emission white with intensity 1, Transmission 1, Sheen RGB 0.5, Anisotropy 0.5, Translucency 0.5. Preserve non-neutral settings, all attachments and all maps. If any controlling value is externally owned, Add records On without seeding and explains the owner. A separate Reset values action is out of scope.

Baked Look and diagnostic preview keep the controls accessible with a section-level bypass explanation. Their presence does not rewrite feature modes. Feature Off gates emission even in Baked Look; other lighting features are already bypassed by the Unlit result.

For a shared material with distinct maps, the texture list reflects the selected object's maps. A hidden unused placement family is accessible through “Unused placement settings” in Advanced, because it may be used by another object sharing the material.

## 4. Data model and seams

The signatures below describe the implemented seams. No new thread, channel, lock, renderer backend or GPU dispatch is introduced.

### 4.1 Manifest descriptors

Add `crates/manifold-core/src/material_inspector.rs`, exported by core. Core owns persisted descriptor vocabulary; the renderer owns the single classification table in `node_graph/material_inspector.rs`.

```rust
// manifold-core::material_inspector; serde camelCase, Copy/Eq where possible.
pub enum MaterialFeature { Coat, Iridescence, Emission, Glass, Sheen, Anisotropy, Translucency }
pub enum MaterialGroup { Surface, Opacity, Feature(MaterialFeature), Advanced }
pub enum MaterialColour { Base, Specular, Emission, Sheen, Attenuation }
pub enum RgbChannel { R, G, B }
pub enum MaterialMapFamily { Base, Normal, MetallicRoughness, Occlusion, Emission }
pub enum UvComponent { M00, M01, M10, M11, Tx, Ty }
pub enum SamplerComponent { WrapU, WrapV, MagFilter, MinFilter }
pub enum MaterialParamRole {
    Scalar(MaterialGroup),
    Colour(MaterialGroup, MaterialColour, RgbChannel),
    FeatureMode(MaterialFeature),
    Placement(MaterialMapFamily, UvComponent),
    Sampler(MaterialMapFamily, SamplerComponent),
}

// New field on SceneParamMetadata and ParamSpecDef:
pub material_role: Option<MaterialParamRole>,

// manifold-renderer::node_graph::material_inspector
pub fn material_param_role(type_id: &str, param_name: &str)
    -> Option<manifold_core::material_inspector::MaterialParamRole>;
```

`ParamSpecDef.material_role` uses `#[serde(default, skip_serializing_if = "Option::is_none")]`; Default initializes None. The registry metadata walk calls this classifier. No `ParamDef` fields or primitive trait signatures change. Scene stamping/migration enriches existing specs in place without rebuilding IDs, labels authored by the user, bindings or param stores. User-authored fan-out specs stay ordinary scalar controls; compound roles require one unambiguous binding. Metadata must be refreshed on existing bindings, not only inserted for newly exposed parameters.

In `manifold-ui::param_surface`, define UI counterparts of these descriptor enums and add `RowSpec.material_role: Option<MaterialParamRole>` and `RowSpec.inactive_reason: Option<String>`. App projection performs an exhaustive core-to-UI conversion. This mirrors the existing `ParamAddr` → `SceneRowAddr` boundary; do not move domain types into foundation or introduce UI dependencies on core/renderer. Ranges/defaults/labels still come only from `ParamSpecDef`.

### 4.2 Saved feature modes and runtime gating

Append seven Enum params to `node.pbr_material` without reordering existing slots:

`coat_mode`, `iridescence_mode`, `emission_mode`, `glass_mode`, `sheen_mode`, `anisotropy_mode`, `translucency_mode`.

Each defaults to Enum 0, labels `From values`, `Off`, `On`. Existing graphs missing the fields get FollowValues through registry defaults. These are ordinary exposed enum parameters and can use existing discrete modulation; layout uses the authored mode/attachments, never the per-frame sample. Existing loading checks parameter type, not enum range. Add `GraphBuildError::InvalidMaterialFeatureMode { node_id: u32, param: String, value: u32 }` in `node_graph/graph_loader.rs` and handle its diagnostic formatting exhaustively. For these seven descriptors only, reject explicitly saved Enum values above 2 in the existing graph-loader parameter validation, before applying the override. Missing values remain valid default 0. Dynamic modulation uses the declared 0..2 range and EnumRound; bound effective mode is clamped to this range at the PBR evaluation boundary. This bounds a live signal; it is not a repair of corrupt saved data. Do not broaden enum-validation changes to unrelated nodes.

After all ordinary factors and numeric input ports have been evaluated in `PbrMaterial::run`, Off overrides only the output: coat factor = 0; iridescence factor = 0; emission RGB = 0; transmission and dispersion = 0; sheen RGB = 0; anisotropy strength = 0; translucency = 0. Glass volume settings remain present but cannot contribute without transmission. Do not write back to node params. The output Material layout and shader ABI remain unchanged. No shader branch or fusion primitive is added for these modes; they are CPU value construction, used by the existing renderer paths.

The mode-parameter count becomes 96. Colours, texture slots, port names and the original 89 parameter identities remain stable. Compatibility means old shows keep their current picture and modulation in the new app; saving with new parameters is not a promise that an older app can load the result.

### 4.3 Scene facts and UI adaptation

Extend renderer `scene_vm::MaterialColorRow` with `pub texture_slots: Vec<MaterialTextureSlot>` and `pub shared_object_count: Option<usize>`. The existing selected object identifies the target map owner; each record's port is its existing graph input name, not a new asset ID.

```rust
// manifold-renderer::node_graph::scene_vm
pub struct MaterialTextureSlot {
    pub port: String,
    pub source: MaterialTextureSource,
}
pub enum MaterialTextureSource {
    Unconnected,
    Known { scope_path: Vec<u32>, node_doc_id: u32, type_id: String },
    GraphSource,
}
```

Populate all 17 slots on structural scene-VM rebuild using the existing producer resolver. Unresolved wired producers are GraphSource, not Unconnected. Count users by scope-qualified material producer identity across the accessible scene; unresolved outgoing uses make the count None. None means incomplete resolution, never one assumed user. Read source layer/path/index from the resolved graph node at projection time. No disk probes, image decoding, graph walking or source-label allocation on each frame.

Add UI `MaterialTextureInfo { port: String, label: String, source_label: String, connected: bool, graph_source: bool }` and `MaterialInspectorInfo { object: ModifierObjectRef, object_gain: Option<ParamId>, material: ModifierObjectRef, shared_object_count: Option<usize>, textures: Vec<MaterialTextureInfo>, params: Vec<(String, ParamId)> }` in `scene_setup_panel.rs`; carry `Option<MaterialInspectorInfo>` alongside `ObjectMaterialVm`. These are app-adapted facts, not persisted state. `object_gain` retains the selected object's separately bound emission gain under Emission. `params` maps inner descriptor names to the selected material's real exposed IDs; actions never infer an address from a suffix. `ParamRow.material_attached` records stored ownership even when a driver or mapping is currently disabled. Runtime readiness is deliberately absent. Labels use “Connected” and an identifiable producer; missing layer references use the existing Skin missing-state fact. Do not infer “Ready,” “Failed” or “Loading” from a wire, filename, black output or lack of thumbnail.

Structural grouping joins real row IDs to these facts once per relevant metadata/topology/base-state change. Per-frame effective value updates retain `sync_scene_row_values` and its ParamId join. Inactive text based on effective feature modes may update without reconstructing rows. Preserve scroll, focus and drawers by material scope/node/ParamId, not current section position.

### 4.4 Compound edits and command ownership

RGB grouping adds `RowRole::ColourSwatch(MaterialColour)` and feature headers add `RowRole::MaterialFeatureToggle(MaterialFeature)` to shared row construction/routing. These variants follow the widget-tree five-step affordance recipe, including dispatch tests. The feature action emits a discrete MaterialParamsSet for Enum Off=1/On=2; it must not reuse boolean 0/1 toggle arithmetic. Add Feature uses the same action with eligible seed writes. Add `ParamRow.rgb_members: Option<[ParamId; 3]>`, populated by the app projection for the canonical R row after grouping by resolved material identity and colour role; G/B rows remain the same scalar identities in the Advanced drawer. Missing/ambiguous members leave the ordinary scalar rows visible. The descriptor supplies the three member ParamIds; the widget never manufactures a new parameter. Add these variants in `panels/scrub.rs`:

```rust
// Additional variants; all existing variants retain their semantics.
ValueRef::ParamRgb(GraphParamTarget, [ParamId; 3]),
ScrubValue::Rgb([f32; 3]),
```

Add the resolved state in `manifold-app::ui_bridge::scrub`:

```rust
ResolvedScrub::ParamRgb {
    target: GraphTarget,
    param_ids: [ParamId; 3],
    preset: PresetTypeId,
    baseline: [f32; 3],
    live: [f32; 3],
},
```

Extend `ResolvedScrub::restore` to reapply all three live bases after a content snapshot swap, using the same all-members preflight as Move. It must never overwrite only part of an RGB value. Snapshot restoration keeps the original baseline and does not create commands. App scrub handling captures all three baselines on Begin. Move validates all members before writing any, then applies the three values in one existing `ContentCommand::MutateProjectLive` closure. Commit submits one guarded command containing three `ChangeGraphParamCommand`s with the original baselines. Content-thread ownership rejection restores a still-current preview without overwriting newer edits. No-op gestures create no undo entry. Selection changes, missing IDs and externally owned members end the gesture without redirecting it to another material; use the existing scrub termination policy. The first picker is an expandable RGB swatch with the three shared scalar controls; a new colour-wheel library is not required. Rendering the swatch does not change linear stored values; any display encoding stays at the UI boundary.

Discrete Add Feature, toggle and look edits share one app-side batch builder in `ui_bridge/project.rs`:

```rust
// UI payload in panels/actions.rs; owner = existing graph target / real IDs.
pub struct MaterialParamWrite { pub param_id: ParamId, pub value: f32 }
pub enum MaterialEditKind { Feature, Look, Placement }
ProjectAction::MaterialParamsSet {
    target: GraphParamTarget,
    object: ModifierObjectRef,
    material: ModifierObjectRef,
    kind: MaterialEditKind,
    writes: Vec<MaterialParamWrite>,
    description: String,
},
```

The app resolves the target and every manifest slot, verifies locks/ownership and duplicate IDs, then builds one `ChangeMaterialParamsCommand` of bound-slot edits. Submit the unexecuted wrapper via existing `ContentCommand::ExecuteOnContent`; wait for the content snapshot, with no optimistic UI project write. Scope is resolved once to stable NodeId provenance and checked against each binding target. Only successfully stamped and unambiguous manifest rows are eligible for these compound operations. V1 compound eligibility additionally requires identity binding scale/offset, linear uninverted card response, matching declared units, and the native Float/EnumRound conversion. Custom reshaped bindings retain ordinary scalar editing with a “Custom binding” reason; never write a physical recipe value directly into a differently calibrated outer slot. This guard also applies to RGB and friendly placement. Unbound/ambiguous custom graphs retain existing scalar editing and graph navigation; do not route a batch through a guessed `apply_scene_param_write` address. Recheck preconditions on the content thread before applying the batch, using a command wrapper with `rejection_reason`/`was_applied`; a stale rejection applies zero writes.

The discrete wrapper lives in `manifold-editing::commands::material`:

```rust
pub enum MaterialEditKind { Feature, Look, Placement }
pub struct MaterialEditContext {
    pub expected_preset_id: manifold_core::PresetTypeId,
    pub object: manifold_core::scene_modifier_preset::SceneNodeRef,
    pub material: manifold_core::scene_modifier_preset::SceneNodeRef,
    pub kind: MaterialEditKind,
}
pub struct MaterialParamChange {
    pub param_id: manifold_core::effects::ParamId,
    pub expected: f32,
    pub value: f32,
}
pub struct ChangeMaterialParamsCommand { /* private prepared edits and undo state */ }
impl ChangeMaterialParamsCommand {
    pub fn new(target: manifold_core::GraphTarget, context: MaterialEditContext,
               changes: Vec<MaterialParamChange>, description: String,
               catalog_default: Option<manifold_core::effect_graph_def::EffectGraphDef>) -> Self;
}
```

Execution amendment: the command carries the prepared catalog default and expected preset ID, like existing graph commands. Core cannot read renderer catalog topology. Missing topology is an explicit rejection, never permission to skip ownership checks; a changed preset ID invalidates the prepared edit. `validate_material_edit(project, target, context, changes, catalog_default: Option<&EffectGraphDef>)` is shared by app preflight and command execution.

The app exhaustively adapts the UI edit-kind enum to the editing enum. The UI reuses `param_surface::ModifierObjectRef` (stable scope plus node ID) for both node references; the app converts to existing core `scene_modifier_preset::SceneNodeRef`. Resolve the complete scope in the target graph; no new identity scheme or bare-ID recursive first-match lookup is introduced. At execute, validate that the object still references that material, all IDs and expected base values match, and every affected binding targets only the expected material. Resolve bindings through `BindingTarget::Node { node_id, param }`, never through document-ID prefixes. The Feature policy permits existing modulation on the mode itself, but rejects wired/fan-out mode ownership; factor seeding rejects externally owned factors. Placement rejects externally owned matrix members. Look enforces D7 against the selected object’s current map wires, all affected attachments and current Skin ownership. Recheck the policy before any child command executes; reject stale state explicitly. Undo restores the captured bases through the same slots. For RGB, final live values require a distinct already-previewed baseline in the existing scrub path; do not run the discrete expected-value precondition against the old baseline after a live move.

### 4.5 Placement and looks

V1 placement offers offset, rotation and scale beside each supported texture family, with the original six scalar rows in its Advanced drawer. Friendly controls keep a local preview and commit the same six ParamIds atomically on release; scene output updates at commit. Raw matrix sliders retain their existing live scalar gestures. There is no new persisted transform. Decomposition is allowed only if both column lengths exceed `1e-8` and `abs(dot(c0,c1)) <= 1e-6 * length(c0) * length(c1)`. Use signed Y scale to preserve reflection; `theta = atan2(m10,m00)`. Keep original values unless a user commits an edit. Drivers on any matrix member disable the compound editor while leaving individual mapping controls accessible. Round-trip tests include shear, reflection, singular matrices and an imported nonidentity matrix.

Implement starter looks as an app-owned static recipe table in `ui_bridge/material_looks.rs`, patterned after the existing `crates/manifold-core/src/scene_modifier_preset.rs` recipe approach. Identify targets through the material node's existing bindings; numeric values below are recipe values, not a second descriptor catalog. Every look sets seven feature modes explicitly (all Off except listed On). Preserve feature subsettings not listed.

| Look | Writes in addition to modes |
|---|---|
| Matte | metallic 0; roughness 0.8 |
| Coated | metallic 0; roughness 0.35; Coat On; clearcoat 1; clearcoat_roughness 0.1 |
| Brushed Metal | metallic 1; roughness 0.3; Anisotropy On; anisotropy_strength 0.6 |
| Glass, prerequisite-gated | metallic 0; roughness 0.05; Glass On; transmission 1; ior 1.5 |

A look name is an action label, not a saved authoritative identity. After application display “Custom material”; do not infer a lasting Glass tag from a subset of values. A default popup explaining blocked targets is enough; no automatic driver removal or detach-map button. Shared scope is stated before the apply action. Emissive Skin blocks looks that change its emission mode/factors until its temporary ownership contract is deliberately extended.

## 5. Invariants and enforcement

The table records the required invariants; concrete regression names below match the implementation. Use the `material_inspector_` prefix so focused commands cannot silently select an unrelated suite; gates must report a nonzero test count.

| Invariant | Machine enforcement |
|---|---|
| All descriptors classified exactly once, no duplicate ranges/defaults | `material_inspector_schema_covers_pbr` compares registry parameter names to role catalog (89 before feature phase, 96 afterward); `material_inspector_existing_exposure_gains_role_without_resetting_authored_fields` |
| Old show loading does not enable/disable features | `material_inspector_feature_modes_gate_evaluated_outputs_only`, testing missing fields; app look/RGB tests cover save/reload; `material_inspector_invalid_mode_rejected` covers saved Enum 3 and dynamic bounds |
| Off gates after modulation without destroying values | `material_inspector_feature_modes_gate_evaluated_outputs_only` and `material_inspector_baked_look_emission_off_stays_gated`; RGB reload test verifies retained modulation |
| Membership stable through animation | `material_inspector_authored_feature_survives_effective_zero` and `material_inspector_external_attachment_survives_dormant_flags` |
| Scope is not a flat doc ID | `material_inspector_rejects_identity_fanout_driven_and_ambiguous_members` and `material_inspector_group_export_resolves_the_actual_material_scope` |
| One undo entry; no partial/stale recipe | `material_inspector_material_command_applies_two_writes_as_one_undo_unit`, `material_inspector_stale_batch_applies_zero_writes`, `material_inspector_rgb_gesture_undo_and_snapshot_restore` |
| Metadata migration retains ownership/modulation | Metadata enrichment preserves existing authored fields; `material_inspector_rgb_gesture_undo_and_snapshot_restore` serializes/reloads and then modulates the retained channel |
| No invented readiness or placement capability | `material_inspector_texture_families_split_connected_and_dormant_drawers` and the descriptor schema test asserts connected/graph-source states and only five independent UV families |
| Shared material scope and map ownership remain visible | `material_inspector_texture_facts_track_shared_identity_and_graph_sources` checks two objects, one material, separate maps and shared placement edits |
| No lossy UV write on view/rebuild | Foundation UV decomposition tests and `material_inspector_placement_opening_and_noop_emit_zero_actions` tests unchanged bytes/values on opening and numeric tolerance `1e-6` after explicit decomposable edits |
| UI uses shared routing | Extend existing widget-tree coverage/dispatch tests; The six `material-inspector-*` UI flows exercises mapping, modulation, type-in and undo |
| Texture-owned factors and locked recipes are explained | Editing material ownership tests plus app look conflict tests covers MR map, wires, automation, fan-out and emissive Skin |

No per-frame scan/allocation is added for grouping or texture facts. New periodic/content work would require a `MANIFOLD_RENDER_TRACE=1` acceptance run with no >20ms content-frame spike; this design schedules structural work only. No new GPU pipeline is created during authoring.

## 6. Phasing

Each phase is one bounded session and ends in a runnable, verified commit. The lead owns review and landing. Re-read relevant symbols if the base differs from the audit; a changed contract stops the affected phase for a design amendment. Do not expand this work into a shader-fidelity sweep.

### Common execution and seam inventory

Use the existing slot ring and build lock from `.claude/GIT_TREE_DISCIPLINE.md` for app changes. Read back this document's applicable decisions and the relevant source symbols before edits. Generate worker context with `scripts/codex_prepare.py` and diff checks with `scripts/codex_checks.py`. Finish through `scripts/land_branch.py`; preserve the mandatory `scripts/landing_gate.py` checks and release the slot.

Focused CPU gate: `cargo test -p CRATE material_inspector_` for each changed crate with new tests; require nonzero matching test counts. Run `cargo clippy -p CRATE --all-targets -- -D warnings` for changed Rust crates, under the build lock. GPU changes additionally run `python3 scripts/gpu_proofs_gate.py --filter material_inspector` after registering the named proofs. Never use nextest for GPU proofs.

Negative gate for each phase: `git diff --unified=0 BASE -- crates | rg '^\+.*(Arc<Mutex|Arc<RwLock|allow\(dead_code\)|#\[ignore)'` returns zero; BASE is the verified phase base. Review any new scalar UI row construction against widget-tree coverage rather than allowing a bespoke material slider. Each phase registers its flow and path trigger in `scripts/ui-flows/manifest.json`; `python3 scripts/run_ui_flows.py FLOW_NAME` must exit zero and produce the screenshot artifact. Agent gates use asserted state/numeric results; Peter judges the PNG's visual quality. No visual success claims from compilation alone.

Rerun these call-site inventories before briefing; initial counts include tests and are a snapshot, not quotas:

```sh
rg -n 'SetGraphNodeParamCommand::new' crates
rg -n 'ChangeGraphParamCommand::new' crates
rg -n 'SetSceneObjectSkinSourceCommand::new' crates
rg -n 'SetSceneObjectSkinTargetMapCommand::new' crates
rg -n 'ParamSpecDef \{|SceneParamMetadata \{|RowSpec \{' crates
rg -n 'ObjectMaterialVm|MaterialColorRow|SceneSetupParamChanged' crates
rg -n 'ValueRef::|ScrubValue::|ResolvedScrub::|GraphBuildError::' crates/manifold-app/src crates/manifold-ui/src
```

First four counts at audit: 29, 18, 11, 4. Literal/declaration inventories: `ParamSpecDef {` has 106 occurrences in 50 files; `SceneParamMetadata {` has 10 in 3; `RowSpec {` has 15 in 7. Constructors using Default may require no edit. Consequences: this is broader than a panel-only change; adapt constructors mechanically and do not alter their defaults. Only changed literal constructors and exhaustive matches need adaptation; do not refactor all command callers. Old scalar IDs, section ownership and `SurfaceVisibility::All` remain. Remove the old material quick-row/flat duplication and emission suffix detour only once their replacements have dispatch coverage.

### P1 — Descriptors and safe grouping

- **Entry/read-back:** no implementation prerequisite. Re-find `metadata_for_node_type`, `stamp_scene_node_exposures`, `ParamSpecDef`, `param_surface`, `build_object_properties_body`; read WIDGET_TREE_DESIGN and D1/D3/D4. Restate no renamed IDs, copied ranges, direct UI writes or new shader behaviour.
- **Deliverables:** core/UI role types, single renderer classifier, additive migration, grouped existing scalar rows; Opacity & Cutout terminology; Advanced recovery of every row. No fake feature toggles or RGB picker yet.
- **Gate:** focused core/renderer/app/UI tests for schema, descriptors, migration, grouped actions and zero-crossing stability; common clippy/negative/landing checks. Save/reload and modulate a previously hidden row after reload.
- **Demo/gesture:** new `material-inspector-groups` flow opens selected PBR, changes roughness, opens its mapping drawer, undoes and asserts previous value. L3 plus PNG, with a held-out imported material as well as a plain generated object. This phase's snapshot is deliberately intermediate and must not show controls for unimplemented features.

### P2 — Atomic compound editing and saved feature state

- **Entry/read-back:** P1 landed. Read `CompositeCommand`, scrub lifecycle, binding resolution and PbrMaterial run; re-run descriptor and command inventory. D2/D6 and content ownership bind this phase.
- **Deliverables:** batch command/preflight, seven appended mode params and output gates, Add Feature/Off/On headers, stable authored membership. Glass remains unavailable until its separate bug dependencies are verified. Explicitly Off groups remain visible.
- **Gate:** atomic/stale/scope/legacy/Off round-trip tests; renderer output-value tests and focused GPU proof comparing old-default material vs FollowValues at zero pixel difference on the same backend. Common checks. No per-frame project mutations from output gating.
- **Demo/gesture:** `material-inspector-features`: add Coat, modulate its amount, turn Off, turn On, undo, save/reload, modulate again. Assert retained factor/attachment and gated runtime factor; L3+PNG. One reproduction/one verification for any observed failure, not an open-ended render sweep.

### P3 — Texture facts and local advanced placement

- **Entry/read-back:** P1 landed; P2 needed for feature map grouping. Re-find scene-object ports, producer resolver, Skin commands and five shader UV families. Read D4/D5/D9. Hold arbitrary nested source resolution to current capabilities.
- **Deliverables:** VM texture/source/shared-user facts, texture cards, five local affine/sampler drawers, MR ownership explanation, same existing Skin picker under Textures, object emission gain under Emission. Remove superseded Skin/emission layout only now. No new asset picker or runtime readiness projection.
- **Gate:** texture capabilities/shared material/scope tests; bound editing and Skin undo/save/reload tests. Held-out fixture has shared material, distinct object maps and nonidentity UVs. Common checks; structural projection must not enter the per-frame value path.
- **Demo/gesture:** `material-inspector-textures`: open MR texture placement, change U offset, undo; switch between two shared-material objects and assert map identity/ownership notice. Verify unrelated map transforms remain unchanged. L3+PNG.

### P4 — RGB swatches

- **Entry/read-back:** P2 batch semantics landed. Read shared row action/builder and scrub handlers; enumerate every ValueRef/ScrubValue match. D6 forbids vector migration and bespoke row routing.
- **Deliverables:** shared colour row role, three-ID swatch/popover, RGB scrub variant and app adapter; retain advanced channel controls.
- **Gate:** RGB gesture undo, mid-drag snapshot restoration, no-op/cancel/selection-change/stale ownership tests; reload then modulate one channel independently. Common checks. A wire-driven channel must not be overwritten by the aggregate control.
- **Demo/gesture:** `material-inspector-colour`: drag a colour channel in the expanded swatch, undo once and assert all starting values; map another channel and assert its drawer remains reachable. L3+PNG.

### P5 — Starter looks

- **Entry/read-back:** P2/P3 landed. Read recipe precedent, MR ownership, Skin temporary ownership and batch rejection. Glass recipe stays absent until BUG-1c9c/BUG-vj1p have bounded runtime proofs; this is a named dependency, not worker discretion.
- **Deliverables:** static Matte/Coated/Brushed Metal recipes and atomic apply action; shared scope notice, named conflicts. Add Glass only after prerequisites pass. No persistent preset-name field.
- **Gate:** look conflict/atomic/round-trip tests; serialized before/after comparison proves maps/UVs/samplers/colour/alpha/object gain/attachments unchanged except specified writes. Common checks.
- **Demo/gesture:** `material-inspector-looks`: apply Coated, undo once, redo, reload; attempt Brushed Metal with MR texture and assert zero writes plus ownership explanation. L3+PNG.

### P6 — Friendly placement over preserved affine values

- **Entry/read-back:** P3 and batch command landed. Read matrix construction/sampling and D8's numerical contract. No transform storage redesign or automatic slot linking.
- **Deliverables:** compound offset/rotation/scale controls for decomposable, undriven transforms; explicit Advanced fallback for shear/singular/driven cases; batch update of six original slots.
- **Gate:** UV round-trip test with identity, rotation, reflection, shear, zero scale and a held-out imported transform; opening causes no writes. One undo restores exact pregesture values. Common checks; focused rendered UV proof where runtime sampling changes are involved.
- **Demo/gesture:** `material-inspector-placement`: rotate a texture, undo; select sheared imported mapping, verify Advanced reason and unchanged matrix. L3+PNG. Retain P3's fully functional affine editor if this phase is not yet landed; label status accordingly.

The two renderer bug repairs are separate workstreams, not an implicit extra phase. Before briefing them, reproduce the named behaviour, decide the compositing/pass fix from that evidence, and update their existing beads. This inspector contract must not be used as authority for speculative shader changes.

## 7. Decided — do not reopen

1. Existing PBR/material wires, texture ownership and ParamIds survive.
2. Off preserves settings and attachments; expansion never changes rendering.
3. Missing feature modes mean FollowValues, maintaining old output.
4. Metadata flows registry → stamped manifest → app projection → shared rows.
5. Texture connection, feature state and visible contribution are distinct facts.
6. MR maps retain current replacement semantics in this UI work.
7. Five independent UV families; other slots do not acquire fictitious transforms.
8. Compound edits are atomic and undoable; stale or conflicting recipes apply nothing.
9. Shared-material scope is visible; no automatic cloning, conversion or driver removal.
10. Glass convenience authoring depends on verified renderer repairs.

## 8. Deferred

| Work | Trigger to reopen |
|---|---|
| Full shader graph, material mixing/layering, new shader kinds | A concrete material cannot be expressed through current model and composable graph nodes. Separate architecture design. |
| Arbitrary nested producer tracing, Make Unique | User needs inspector editing beyond currently resolvable scene graphs; extend graph identity/edit commands first. |
| Texture asset library, thumbnails, runtime loading/error badges | A dedicated snapshot contract exposes actual runtime states without frame-path IO. |
| Independent UVs/samplers for all extension maps, linked placement | Explicit renderer capability request and compatibility design; not cosmetic UI. |
| Multiplicative MR factors | Explicit shading/import contract decision with old-show migration and parity evidence. |
| Material library persistence and procedural recipes | Starter factor looks prove insufficient; design reuse/ownership/versioning separately. |
| RT material parity and unlit UV limitations | Existing BUG-zge, BUG-yrca and BUG-dwv1 workstreams; this inspector must describe known limits without claiming their resolution. |

The prototype establishes direction. This proposal establishes implementation boundaries; neither is evidence that the new native inspector has shipped.
