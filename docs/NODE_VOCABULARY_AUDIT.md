# Node Vocabulary Audit — type_id Renames, Fills, Conventions

**Status:** SHIPPED — apply pass P1–P7 complete and merged into `feat/timeline-ui-redesign` (2026-07-03). All ~110 renames (102 §4 atoms + 2 §7 legacy folds + 8 §6 presets) live in `manifold-core/src/type_id_migration.rs`; examples auto-population, cross-tool aliases, and the §8c completeness gate are wired. This doc is now the historical record + the migration-table reference. Original approval: Peter, 2026-07-02, including the six §10 label changes.
**Decided:** 2026-07-02. Companion: `NODE_CATALOG.md` (generated index), `MCP_INTERFACE_DESIGN.md` (the catalog's main future consumer).
**Prerequisites:** none — this is the FIRST item in `docs/DESIGN_BUILD_ORDER.md` (renames get dearer with every preset/binding written on old ids).
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase.

## 1. Verdict

The descriptor layer is in better shape than expected: 212 nodes, most with display labels, VJ-voice summaries, technical purposes, categories, roles, and aliases already filled. The three registers Peter asked for **already exist** — `label` (stage voice), `summary` (friendly one-liner), `purpose` (technical: formula, algorithm, conventions). No schema work needed.

The debt is exactly four things:

1. **~75 stale type_ids** that no longer match their display labels (`node.gain` shows as "Exposure", `node.radial_fold_uv` as "Kaleidoscope", `node.uv_strip_clamp` as "Edge Stretch"). §4.
2. **13 unfilled stragglers** — missing labels, 5 fully uncategorized nodes, 3 empty alias sets. §5.
3. **8 stale preset ids** + a muddled generator category scheme. §6.
4. **3 legacy nodes** needing a fold-or-hide decision. §7.

## 2. Conventions (pinned)

1. **`type_id` = `node.` + snake_case of the display label.** One name, three surfaces (palette, JSON, docs). Parenthetical label qualifiers drop from the id unless needed to disambiguate (`Add Burst (3D, radial)` → `node.add_burst_3d`).
2. **Name the visual outcome, not the implementation.** Algorithm names move to `purpose` + `aliases` (`euler_step`, `midas`, `9tap`, `ffi`, `lic`) — *except* where the algorithm is the user-facing identity (Gaussian Blur, LFO, PBR, Reinhard, One Euro Filter).
3. **Domain suffixes only for real collisions:** `_3d`, `_value`/`_image`, `_array`/`_texture` variants of the same concept.
4. **"Array" is the user word for array-typed data** in labels (Array Math, Array Feedback) — decided by Peter 2026-07-02: it matches the port type system's own name (`Array(...)`), so labels, wire tooltips, and ids all agree. "Copies" is the user word for instances (Arrange Copies, Blend Copies, per-copy).
5. **Old ids never get reused** for a different node. The migration table is permanent.
6. **`purpose` must state the math**: operation/formula, coordinate space, range conventions, boundary behavior. It's the technical register serving expert users and agents (via MCP `get_node_docs`); `summary` never contains math.
7. `system.*` ids are exempt (auto-wired plumbing, invisible to users).

## 3. Migration infrastructure (build first)

- **`crates/manifold-core/src/type_id_migration.rs`** *(new)*: one static table `&[(old, new)]` + `pub fn migrate_type_id(&str) -> &str` (identity for unknown ids). A second small table supports **param-seeding folds**: `(old_id, new_id, seed_params: &[(name, value)])` for legacy nodes folded into a parameterized successor (§7).
- **One choke point per document kind:** applied immediately after deserialization — `EffectGraphDef` nodes (all loaders route through `instantiate_def`; migrate before flatten), and `PresetTypeId` on clips (`generator_type`) + effect instances at project load. The runtime, registry, editor, and catalog only ever see new ids.
- **Not affected:** `BindingTarget` (targets `NodeId`, not type_id), OSC (`osc_prefix` is its own field), WGSL sources, param names.
- **In-repo rewrites (not migration):** bundled preset JSONs, parity/gpu tests, `hand_descriptor!` entries, `primitive!` registrations — all mechanically renamed to new ids in the same commit.
- **Tests:** (a) fixture graph written with old ids loads structurally identical to its new-id twin; (b) a project fixture with old `generator_type` values loads; (c) Liveschool canonical fixture green; (d) full workspace sweep (registry change = infrastructure per CLAUDE.md).

## 4. Rename table — atoms (~75)

Unlisted nodes keep their current id (already aligned). Aliases always gain the old id's tail as a search synonym.

### Color & Tone / Composite
| Old | New | Label |
|---|---|---|
| `node.gain` | `node.exposure` | Exposure |
| `node.color_ramp` | `node.gradient_map` | Gradient Map |
| `node.channel_mix` | `node.channel_mixer` | Channel Mixer |
| `node.clamp_texture` | `node.clamp` | Clamp |
| `node.hdr_retention_mix` | `node.hdr_mix` | HDR Mix |

### Blur / Distort / Stylize
| Old | New | Label |
|---|---|---|
| `node.blur_3d_separable` | `node.blur_3d` | Blur (3D) |
| `node.convolution_2d_9tap` | `node.custom_convolution` | Custom Convolution |
| `node.gaussian_blur_variable_width` | `node.variable_blur` | **Variable Blur** (label simplified too) |
| `node.chromatic_displace` | `node.rgb_split` | RGB Split |
| `node.mirror_axis` | `node.flip` | Flip |
| `node.mirror_fold_uv` | `node.mirror` | Mirror |
| `node.radial_fold_uv` | `node.kaleidoscope` | Kaleidoscope |
| `node.uv_strip_clamp` | `node.edge_stretch` | Edge Stretch |
| `node.affine_transform` | `node.transform` | **Transform** (label was missing) |

### Generate / Mask / Noise
| Old | New | Label |
|---|---|---|
| `node.gradient_ramp` | `node.gradient` | Gradient |
| `node.render_filled_rects` | `node.draw_rectangles` | Draw Rectangles |
| `node.render_lines` | `node.draw_lines` | Draw Lines |
| `node.render_value_overlay` | `node.value_overlay` | Value Overlay |
| `node.box_mask` | `node.rectangle_mask` | Rectangle Mask |
| `node.ellipse_mask` | `node.circle_mask` | Circle Mask |

### Math & Convert
| Old | New | Label |
|---|---|---|
| `node.abs_texture` | `node.absolute_value` | Absolute Value |
| `node.fract_texture` | `node.wrap` | Wrap |
| `node.power_texture` | `node.power` | Power |
| `node.trig_texture` | `node.sine_cosine` | Sine / Cosine |
| `node.smoothstep_texture` | `node.smoothstep` | Smoothstep |
| `node.scale_offset_texture` | `node.scale_offset_image` | Scale + Offset (image) |
| `node.affine_scalar` | `node.scale_offset_value` | Scale + Offset (value) |
| `node.array_math` | *(keep)* | **Array Math** (label changes from "List Math") |
| `node.array_feedback` | *(keep)* | Array Feedback |
| `node.array_unpack_vec2` | `node.split_xy` | Split XY |
| `node.array_connect_nearest` | `node.connect_nearest` | Connect Nearest |
| `node.pack_curve_xy` | `node.combine_xy` | Combine XY (curve) |
| `node.pack_vec4` | `node.combine_xyzw` | Combine XYZW |
| `node.pack_channels` | `node.pack_rgba` | Pack RGBA |
| `node.length_vec2` | `node.vector_length` | **Vector Length** (label clarified) |
| `node.normalize_vec2` | `node.normalize` | Normalize |
| `node.generate_range` | `node.range` | Range |
| `node.scalar_array_accumulator` | `node.sum_into_bins` | Sum Into Bins |
| `node.resolve_accumulator` | `node.resolve_scatter` | Resolve Scatter |
| `node.resolve_3d_accumulator` | `node.resolve_scatter_3d` | Resolve Scatter (3D) |
| `node.texture_dimensions` | `node.texture_size` | Texture Size |

### Fields & Coordinates
| Old | New | Label |
|---|---|---|
| `node.gradient_central_diff` | `node.edge_slope` | Edge Slope |
| `node.gradient_central_diff_3d` | `node.edge_slope_3d` | Edge Slope (3D) |
| `node.lic_integrate` | `node.flow_lines` | Flow Lines |
| `node.rotate_2d` | `node.rotate_coordinates` | **Rotate Coordinates** (label clarified — it spins the warp coordinates, not the image; avoids confusion with Rotate 3D/4D which move geometry) |
| `node.rotate_vec2_by_angle` | `node.rotate_vector` | Rotate Vector |
| `node.sin_term` | `node.sine_wave` | Sine Wave (projected) |
| `node.sample_volume_2d` | `node.slice_volume` | **Slice Volume** (label clarified — avoids collision with "Sample Volume for Particles") |

### 3D Geometry / Materials
| Old | New | Label |
|---|---|---|
| `node.camera_orbit` | `node.orbit_camera` | Orbit Camera |
| `node.consecutive_edges` | `node.edge_pairs` | Edge Pairs |
| `node.displace_mesh` | `node.push_mesh` | Push Mesh |
| `node.edges_from_grid_uv` | `node.grid_edges` | Grid Edges |
| `node.edges_from_hypercube` | `node.hypercube_edges` | Hypercube Edges (4D) |
| `node.hypercube_vertices` | `node.hypercube_points` | Hypercube Points (4D) |
| `node.generate_cube_mesh` | `node.cube_mesh` | Cube Mesh |
| `node.generate_grid_mesh` | `node.grid_mesh` | Grid Mesh |
| `node.generate_grid_uv` | `node.grid_points` | Grid Points (UV) |
| `node.generate_instance_transforms` | `node.arrange_copies` | Arrange Copies |
| `node.polytope_edges` | `node.platonic_solid_edges` | Platonic Solid Edges |
| `node.polytope_vertices` | `node.platonic_solid_points` | Platonic Solid Points |
| `node.project_3d` | `node.flatten_3d` | Flatten 3D → 2D |
| `node.project_4d` | `node.flatten_4d` | Flatten 4D → 3D |
| `node.render_3d_mesh` | `node.render_mesh` | Render Mesh |
| `node.render_instanced_3d_mesh` | `node.render_copies` | Render Copies |
| `node.triangulate_grid` | `node.make_triangles` | Make Triangles |
| `node.array_replicate_polyline_rings` | `node.repeat_outline` | Repeat Outline (rings) |
| `node.bake_equirect_envmap` | `node.bake_environment` | Bake Environment (equirect) |
| `node.blinn_specular` | `node.shininess` | Shininess (Blinn) |
| `node.fresnel_rim` | `node.rim_light` | Rim Light (Fresnel) |
| `node.lambert_directional` | `node.basic_light` | Basic Light (Lambert) |
| `node.heightmap_to_normal` | `node.surface_bumps` | Surface Bumps |

### Particles (2D + 3D)
| Old | New | Label |
|---|---|---|
| `node.apply_radial_burst_to_particles` | `node.add_burst` | Add Burst (radial) |
| `node.apply_radial_burst_3d_to_particles` | `node.add_burst_3d` | Add Burst (3D, radial) |
| `node.array_diffuse_particles` | `node.spread_out` | Spread Out (diffuse) |
| `node.diffuse_force_3d_at_particles` | `node.spread_out_3d` | Spread Out (3D diffuse) |
| `node.euler_step_particles` | `node.move_particles` | Move Particles |
| `node.euler_step_particles_3d` | `node.move_particles_3d` | Move Particles (3D) |
| `node.seed_particles` | `node.spawn_particles` | Spawn Particles |
| `node.seed_particles_from_texture` | `node.spawn_from_image` | Spawn From Image |
| `node.scatter_particles` | `node.draw_particles` | Draw Particles (scatter) |
| `node.scatter_particles_3d` | `node.draw_particles_3d` | Draw Particles (3D scatter) |
| `node.scatter_particles_camera` | `node.draw_particles_camera` | Draw Particles (camera) |
| `node.simplex_noise_force_at_particles` | `node.turbulence` | Turbulence (simplex) |
| `node.simplex_noise_force_3d_at_particles` | `node.turbulence_3d` | Turbulence (3D, simplex) |
| `node.curl_slope_force_3d` | `node.swirl_force_3d` | Swirl Force (3D, curl) |
| `node.container_bounds_3d` | `node.keep_in_box_3d` | Keep In Box (3D) |
| `node.container_repel_force_3d` | `node.push_from_walls_3d` | Push From Walls (3D) |
| `node.radial_burst_force_field` | `node.explosion_force` | Explosion Force |
| `node.wrap_particles_torus` | `node.wrap_around` | Wrap Around (torus) |
| `node.fbm_per_instance` | `node.fractal_noise_per_copy` | Fractal Noise (per copy) |
| `node.simplex_per_instance` | `node.simplex_noise_per_copy` | Simplex Noise (per copy) |
| `node.instance_position_jitter` | `node.position_jitter` | Position Jitter |
| `node.instance_rotation_jitter` | `node.rotation_jitter` | Rotation Jitter |
| `node.lerp_instance_fields` | `node.blend_copies` | Blend Copies |
| `node.sample_texture_at_particles` | `node.sample_image_at_particles` | Sample Image for Particles |
| `node.sample_texture_3d_at_particles` | `node.sample_volume_at_particles` | Sample Volume for Particles |

### Detection / Routing
| Old | New | Label |
|---|---|---|
| `node.blob_detect_ffi` | `node.blob_tracker` | Blob Tracker |
| `node.blob_overlay_render` | `node.blob_overlay` | Blob Overlay |
| `node.depth_estimate_midas` | `node.depth_map` | Depth Map |
| `node.optical_flow_estimate` | `node.optical_flow` | Optical Flow |
| `node.person_segment` | `node.person_mask` | Person Mask |
| `node.mux_array` | `node.switch_array` | Switch (Array) |
| `node.mux_scalar` | `node.switch_value` | Switch (value) |
| `node.mux_texture` | `node.switch_texture` | Switch (texture) |

**Deliberately NOT renamed (pending decomposition — renaming a to-be-deleted node is churn):** `node.cylinder_wrap_field`, `node.torus_wrap_field`, `node.digital_plants_render`, `node.nested_cubes_geometry`, `node.watercolor`. They get migration entries when their decompositions land.

## 5. Fills (missing metadata)

**Missing labels** (id stays): `node.chroma_key` → "Chroma Key" · `node.color_lut` → "Color LUT" · `node.edge_detect` → "Edge Detect" · `node.invert` → "Invert" · `node.masked_mix` → "Masked Mix" · `node.watercolor` → "Watercolor".

**Fully uncategorized (5):** `node.fbm_2d`, `node.hash_noise_field_2d`, `node.perlin_noise_2d`, `node.simplex_noise_2d`, `node.multi_blend`. The four noise atoms look superseded by the unified `node.noise` (Type param). **Verify at apply:** if they're the internal implementations behind the mux, hide them from the palette (descriptor `available`-equivalent) and document; if independently useful, fill descriptors properly. `node.multi_blend` is live (supersedes `texture_sum_5`) — needs category (Composite), role (Filter), summary, aliases.

**Empty aliases:** `node.radial_offset_field` (add: push field, radial displace, zoom warp) · `node.saturation` (add: vibrance, desaturate, Level TOP) · `node.voronoi_2d` (add: cellular, worley, cells, mosaic).

## 6. Presets

| Old id | New id | Notes |
|---|---|---|
| `EdgeGlow` | `EdgeDetect` | name is "Edge Detect" |
| `HdrBoost` | `HighlightBoost` | name is "Highlight Boost" |
| `InvertColors` | `Invert` | name is "Invert" |
| `WireframeZoo` | `Wireframe` | name is "Wireframe" |
| `SoftFocusGraph` | `SoftFocus` | "Graph" suffix is migration residue |
| `ComputeStrangeAttractor` | `StrangeAttractor` | "Compute" is implementation |
| `FluidSimulation` | `FluidSim2D` | name is "Fluid Sim 2D" |
| `FluidSimulation3D` | `FluidSim3D` | symmetry |

Same migration table (PresetTypeId choke point). `osc_prefix` unchanged, so OSC/Ableton mappings survive.

**Also:** `NodeGraphTest` → hide from picker (diagnostic). **Generator categories — regrouping decided with Peter 2026-07-02.** Rule: category = what the engine is doing (Sim = stateful, evolves frame to frame; Geometry = shapes/meshes/wireframes; Pattern = stateless shader math; Text & Media = typography/external content) — except when user search intent is content-shaped, content wins.

| Category | Presets |
|---|---|
| **Sim** | Fluid Sim 2D, Fluid Sim 3D, Oily Fluid, Metallic Glass†, Black Hole, Strange Attractor |
| **Geometry** | Tesseract, Nested Cubes, Duocylinder, Wireframe, Digital Plants, Lissajous |
| **Pattern** | Plasma, Concentric Tunnel, Basic Shapes, Star Field† |
| **Text & Media** | Text, Particle Text (a sim, but content intent wins — aliases: particles, sim), MRI Volume |

† Verify at apply: Star Field assumed stateless (→ Pattern); Metallic Glass assumed stateful feedback (→ Sim). If wrong, swap accordingly.

## 7. Legacy trio (decisions)

1. **`node.rotate_vec2_90`** → fold into `node.rotate_vector` via the param-seeding migration (`angle = 90`). Verify port parity at apply; then delete the node.
2. **`node.fluid_project_scatter_2d`** → verify it's port-identical to `node.draw_particles_camera` (catalog says it's "the older name"); if so, plain rename-table entry and delete.
3. **`node.texture_sum_5`** → stays hidden-legacy (fixed 5 ports vs multi_blend's dynamic ports — a wire-rewriting migration isn't worth it). Revisit only if it blocks something.

## 8. Purpose (technical register) — spot-audit result

Sampled purposes are genuinely good ("Per-pixel abs(input.rgb). Alpha passes through…"). Rule pinned in §2.6; the apply pass does a linear read of all 212 purposes and upgrades any that fail the "states the math" bar — expected to be a minority. No structural work.

## 8b. Two cheap wins (added after review with Peter)

1. **Auto-populate `examples` — approved.** The descriptor's `examples` field ("which presets use this node") is empty across all 212 nodes — yet it's the few-shot pointer humans and agents need most. Generated, never hand-written: the catalog build scans the 45 bundled preset graphs for node usage and emits the field. Because it's derived at generation time, it cannot go stale.
2. **Cross-tool alias pass — approved.** Aliases cover TouchDesigner well (Blur TOP, Math CHOP) but nothing else. One pass adding **Resolume, Blender, and After Effects** equivalents to matching nodes. One-time work — §8c's completeness gate keeps future nodes from shipping alias-less.

## 8c. Anti-staleness architecture (how this system stays honest)

Principle: **one source of truth (code-side descriptors, co-located with the node), everything else generated, and a build gate on emptiness.** Four drift classes, four mechanisms:

| Drift class | Mechanism | Status |
|---|---|---|
| **Existence** — node ships, catalog doesn't know | Generated index fails `cargo test` on unregenerated registry change | Already built |
| **Emptiness** — node ships with blank vocabulary (how the 5 uncategorized nodes happened) | **Completeness gate**: extend the drift test so any palette-visible node with empty label / summary / category / aliases fails the build. Vocabulary becomes a merge requirement paid by the node's author, when context is richest. Hidden/legacy/`system.*` nodes exempt. | Build in apply pass |
| **Derived data** — examples, node_catalog.json, doc tables | Never hand-write anything derivable. `examples` generated from preset scan; MCP serves the live registry at runtime (per `MCP_INTERFACE_DESIGN.md` §5) — both stale-proof by construction. | Build in apply pass |
| **Truth** — kernel changes, description now lies | No full automation exists. Mitigations: purpose lives in the same file as the node (macro field — on screen during edits); rule added to `ADDING_PRIMITIVES.md`: *touch the kernel, re-read the purpose*; parity tests flag behavior changes in the same PR, which is the natural re-read prompt. | Convention + doc line |

## 9. Apply-pass phases (Sonnet)

**Pre-flight (before phase 1 — the §4/§6 tables are a 2026-07-02 snapshot):**
regenerate the catalog (`cargo run -p manifold-renderer --bin gen_node_catalog`),
then for every OLD id in §4: `rg -c '"<old_id>"' crates/manifold-renderer/src/node_graph/primitives/`
must hit — an old id that no longer exists means the node was renamed/deleted since
this doc was written: **stop and list it, don't guess**. Also scan the catalog for
nodes added since 2026-07-02 whose ids violate §2 conventions — list them for Peter
as table additions before starting; don't silently rename unlisted nodes.

**Forbidden moves, all phases:** no partial application (an id renamed in
registration but not presets/tests is a broken build — good, the compiler is the
checklist; a rename skipped entirely is the failure); no renaming anything not in
§4/§6; no "improving" labels/summaries beyond what §4/§5/§10 specify; old ids never
reused (§2.5).

- **P1 — Migration infra (§3), landed alone.** Deliverables:
  `type_id_migration.rs` (both tables + `migrate_type_id`), choke-point wiring
  (`instantiate_def` pre-flatten; `PresetTypeId` at project load), the four §3
  tests. Gate: `cargo test -p manifold-core --lib`, fixture tests green, Liveschool
  fixture green. Table is EMPTY at this point except a test entry — infra first,
  content later.
- **P2 — Atom renames (§4).** Compiler-driven per category group: change the
  `primitive!` registration id first, then fix every red site (descriptors, bundled
  preset JSONs, parity/gpu tests), add the migration entry, commit per group. Gate
  per group: compile + lib `bundled_presets` tests only (rescoped 2026-07-03,
  Peter: an id rename can't change pixels — no parity runs, no per-phase workspace
  sweeps; the single workspace sweep lives in P7). **Negative gate at end of P2:**
  for every old id in §4, `rg -n '"<old_id>"' crates/ assets/` hits ONLY
  `type_id_migration.rs` (loop the table; zero exceptions), plus the bare-word
  prose sweep `rg -n '\bnode\.<old_tail>\b' crates/ assets/ -g '!*.wgsl'` (preset
  JSON descriptions mention ids in prose — found in apply, groups 1–5).
- **P3 — Preset renames + `NodeGraphTest` hide (§6).** Same technique, same
  negative gate for the 8 preset ids. OSC prefixes must be byte-identical before/
  after (`rg 'osc_prefix'` diff).
- **P4 — Fills (§5) + legacy folds (§7).** Each §7 item has a verify step — run it,
  record the result in the commit message; a failed verify (ports don't match) is
  an escalation, not an adaptation.
- **P5 — Generator recategorization (§6).** The grouping is DECIDED (Peter,
  2026-07-02 — the §6 table, including the two † verify-at-apply rows). Run the two
  † checks; swap per the stated rule if they come out opposite.
- **P6 — §8b/§8c:** examples auto-population, cross-tool alias pass, completeness
  gate wired into the drift test, `ADDING_PRIMITIVES.md` touch-rule line. Gate: the
  completeness gate itself fails the build when any palette-visible node has empty
  label/summary/category/aliases — prove it with a deliberate temporary violation,
  then remove it.
- **P7 — Regenerate + sweep.** NODE_CATALOG regen, docs index regen, Liveschool
  fixture green, `cargo clippy --workspace -- -D warnings`, full workspace test
  sweep. Report the final rename count vs the table (must match exactly).

## 10. Review checklist for Peter

**APPROVED 2026-07-02.** The **bolded rows** in §4 change the label too, not just the id: Variable Blur, Transform, Array Math, Vector Length, Rotate Coordinates, Slice Volume — Peter said yes to all six. ("Array" as the user word and the generator category regrouping: both decided by Peter 2026-07-02 — §2.4, §6.) Everything else is mechanical alignment — id catches up to the label you already approved by shipping it. Nothing in this doc awaits review.
