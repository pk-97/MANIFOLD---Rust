//! Per-frame setup: the frame preamble (inputs, light tables, the one
//! reset-detector call, camera and jitter history), the per-object draw
//! list, the empty-frame clear and temporal-history invalidation.

use super::*;

impl RenderScene {
    /// BUG-trh7 stage 2, pass 0: the frame preamble — per-frame inputs, the
    /// light/caster tables, temporal-upscale resolution, RT toggle latches,
    /// the ONE reset-detector call, and the camera/jitter history. Returns
    /// None on the two early-frame aborts (no color output, zero dims) — the
    /// caller (evaluate) returns immediately, exactly as the inline code did.
    /// BUG-trh7 stage 2, pass 1: validate every object's required inputs,
    /// compose its model matrix + uniforms, and get-or-compile its pipeline.
    /// None = abort frame — the three structured-error magenta-clear returns.
    /// An empty draw list is returned for the caller to clear all connected
    /// outputs and invalidate temporal history.
    pub(super) fn collect_object_draws<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        port_index: &ahash::AHashMap<&'static str, manifold_node_engine::bindings::Slot>,
        single_object: Option<SceneObject>,
    ) -> Option<(Vec<ObjectDraw<'ctx>>, bool)> {
        let FramePrelude {
            objects, cam, envmap_wired, envmap_pending, env_rotation, atmosphere, render_mode, view_proj, prev_view_proj,
            jitter_ndc, prev_jitter_ndc, light_count, velocity_wired,
            ao_mask_wired, denoise_aux_ready, rt_enabled, ..
        } = pre;
        // SCENE_RENDER_MODE_DESIGN.md D4/INV-R4: the effective mode for this
        // frame's raster path. `rt_enabled` scenes ignore the wire entirely
        // (Rendered uniform set) — and because the substitution happens
        // here, the gi_materials table the RT pass builds from these draws
        // never sees a wireframe override either.
        let render_mode = effective_render_mode(render_mode, *rt_enabled);
        let instance_size = std::mem::size_of::<InstanceTransform>() as u64;

        let mut draws: Vec<ObjectDraw<'ctx>> = Vec::with_capacity(*objects);
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2a: the sole CPU gate for the
        // Pass A/B split below. Only `Blend` objects ever reach Pass B
        // (`is_glass` in `gltf_import.rs` always routes a nonzero
        // `transmission_factor` to `Blend` alpha mode, so this condition and
        // "blend_draw_calls is non-empty" are the same fact) — checking both
        // here keeps that invariant explicit rather than assumed.
        let mut has_transmission = false;

        for n in 0..*objects {
            // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D4 (P2): every object's
            // full bundle (mesh/transform/material/17 maps/instances/
            // visible) arrives on ONE `object_n: Object` port — resolved
            // through `port_index` (built once per frame, above) instead of
            // a per-lookup `NodeInputs::slot` scan, same treatment the
            // legacy per-port lookups got.
            let object_port = &self.object_port_names[n];
            let object_slot_id = port_index.get(object_port.as_ref()).copied();
            // A late object drops out of this frame; the rest of the scene
            // still renders, so one streaming mesh never stalls the show.
            if object_slot_id.is_some_and(|slot| !ctx.inputs.slot_content_ready(slot)) {
                self.report_incomplete_frame(ctx);
                continue;
            }
            let Some(object) = single_object.or_else(|| object_slot_id.and_then(|s| ctx.inputs.object_slot(s))) else {
                // Unwired `object_n` (no `node.scene_object` feeding this
                // index yet — an in-progress edit): skip this object
                // entirely, no error. Matches an unwired `visible` object's
                // "no draw, no shadow" treatment below.
                continue;
            };
            if !object.visible {
                // D4: `visible == false` is no draw AND no shadow cast — an
                // invisible object leaves no shadow. Skipping here (never
                // pushed into `draws`) removes it from both the main pass
                // and the shadow-caster pass below, which filters from
                // `draws`.
                continue;
            }
            let mesh_slot = object.mesh;
            // Never draw or trace unlanded bytes: the object drops out.
            if mesh_slot.is_some_and(|s| !ctx.inputs.slot_content_ready(s)) {
                self.report_incomplete_frame(ctx);
                continue;
            }
            let Some(vertices) = mesh_slot.and_then(|s| ctx.inputs.array_slot(s)) else {
                ctx.gpu_encoder().merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
                ctx.error(format!(
                    "object_{n}: missing required `vertices` input (its scene_object's `vertices` port is unwired); renderer fell back to magenta clear"
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return None;
            };
            // RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D6: this object's
            // `mesh_n` write generation, feeds the shadow cache key below.
            let vertices_content = mesh_slot.and_then(|s| ctx.inputs.content_version_of(s));
            if object.indices.is_some_and(|slot| !ctx.inputs.slot_content_ready(slot)) {
                self.report_incomplete_frame(ctx);
                continue;
            }
            let indices = object.indices.and_then(|slot| ctx.inputs.array_slot(slot));
            if object.indices.is_some() && indices.is_none() {
                ctx.gpu_encoder().merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
                ctx.error(format!("object_{n}: wired index buffer is unavailable"));
                return None;
            }
            let indices_content = object.indices.and_then(|slot| ctx.inputs.content_version_of(slot));
            let live_extent = object.indices.or(mesh_slot).and_then(|s| ctx.inputs.live_extent_slot(s));
            let capacity = indices.map_or_else(|| whole_triangle_vertices(vertices), |buffer| (buffer.size / 4 / 3 * 3).min(u64::from(u32::MAX)) as u32);
            let (object_vertex_count, point_count) = match &live_extent {
                Some(extent) => {
                    let bound = extent.bound.min(capacity);
                    (bound, bound)
                }
                None => (
                    capacity,
                    indices.map_or((vertices.size / std::mem::size_of::<MeshVertex>() as u64) as u32, |_| capacity),
                ),
            };
            let weights_slot = object.weights;
            if weights_slot.is_some_and(|s| !ctx.inputs.slot_content_ready(s)) {
                self.report_incomplete_frame(ctx);
                continue;
            }
            let weights = weights_slot.and_then(|s| ctx.inputs.array_slot(s));
            if weights_slot.is_some() && weights.is_none() {
                ctx.gpu_encoder().merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
                ctx.error(format!(
                    "object_{n}: wired appearance weights input is unavailable; renderer fell back to magenta clear"
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return None;
            }
            let vertex_count = (vertices.size / std::mem::size_of::<MeshVertex>() as u64) as u32;
            if let Some(weights) = weights
                && ((weights.size / std::mem::size_of::<f32>() as u64) as u32) < vertex_count
            {
                ctx.gpu_encoder().merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
                ctx.error(format!(
                    "object_{n}: appearance weights buffer is shorter than the mesh ({}, need {vertex_count}); renderer fell back to magenta clear",
                    weights.size / std::mem::size_of::<f32>() as u64
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return None;
            }
            // P4b (SCENE_MODIFIER_RT_DESIGN.md §5.2): appearance weights and
            // gain are SUPPORTED under RT — the old rejection here is
            // removed now that `rt_dynamic_coverage_and_attributes` proves
            // the coverage/brightness behavior against the raster formula.
            let Some(material) = object.material else {
                ctx.gpu_encoder().merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
                ctx.error(format!(
                    "object_{n}: missing required `material` input (its scene_object's `material` port is unwired); renderer fell back to magenta clear"
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return None;
            };
            // SCENE_RENDER_MODE_DESIGN.md D6/D7/D8: ONE match on the mode
            // substitutes the object's material at the gather site —
            // wireframe and points get the unlit line surface (D2: the
            // wire's line_color/line_brightness serve Points too), solid
            // the clay PBR material, Rendered passes the object's own material
            // through untouched. Upstream of `pipeline_for`, so the
            // ordinary per-kind pipeline cache picks the substitute's
            // shader and no shader changes.
            let material = match render_mode.mode {
                manifold_node_engine::scene::render_mode::RENDER_MODE_WIREFRAME => wireframe_material(&render_mode),
                manifold_node_engine::scene::render_mode::RENDER_MODE_SOLID if envmap_wired.is_some() || *envmap_pending => {
                    clay_material(&render_mode)
                }
                manifold_node_engine::scene::render_mode::RENDER_MODE_SOLID => clay_material_without_envmap(&render_mode),
                manifold_node_engine::scene::render_mode::RENDER_MODE_POINTS => wireframe_material(&render_mode),
                _ => material,
            };
            if material.requires_envmap() && *envmap_pending {
                self.report_incomplete_frame(ctx);
                continue;
            }
            if material.requires_envmap() && envmap_wired.is_none() {
                ctx.gpu_encoder().merge_frame_status(FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
                ctx.error(format!(
                    "{:?} material on `object_{n}` requires `envmap` input but it is unwired; renderer fell back to magenta",
                    material.kind
                ));
                if let Some(target) = ctx.outputs.texture_2d("color") {
                    let gpu = ctx.gpu_encoder();
                    gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0);
                }
                return None;
            }
            // `NodeInputs` is `Copy` (bindings.rs) — capture it by value so
            // these lookups don't hold `ctx` borrowed (`ctx.error`/
            // `ctx.outputs`/`ctx.gpu_encoder()` below still need mutably).
            let inputs = ctx.inputs;
            let base_color_map = object.base_color_map.and_then(|s| inputs.texture_2d_slot(s));
            // IMPORT_FIDELITY_DESIGN.md D3/F-P2: the four new optional
            // per-object texture ports.
            let normal_map = object.normal_map.and_then(|s| inputs.texture_2d_slot(s));
            let mr_map = object.mr_map.and_then(|s| inputs.texture_2d_slot(s));
            let legacy_roughness_map = single_object.and_then(|_| inputs.texture_2d("roughness_map"));
            let legacy_metallic_map = single_object.and_then(|_| inputs.texture_2d("metallic_map"));
            let occlusion_map = object.occlusion_map.and_then(|s| inputs.texture_2d_slot(s));
            let emissive_map = object.emissive_map.and_then(|s| inputs.texture_2d_slot(s));
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E3/E4/E5 (D1 revised).
            let sheen_color_map = object.sheen_color_map.and_then(|s| inputs.texture_2d_slot(s));
            let sheen_roughness_map =
                object.sheen_roughness_map.and_then(|s| inputs.texture_2d_slot(s));
            let iridescence_map = object.iridescence_map.and_then(|s| inputs.texture_2d_slot(s));
            let iridescence_thickness_map =
                object.iridescence_thickness_map.and_then(|s| inputs.texture_2d_slot(s));
            let anisotropy_map = object.anisotropy_map.and_then(|s| inputs.texture_2d_slot(s));
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — texture-
            // completion sweep).
            let clearcoat_map = object.clearcoat_map.and_then(|s| inputs.texture_2d_slot(s));
            let clearcoat_roughness_map =
                object.clearcoat_roughness_map.and_then(|s| inputs.texture_2d_slot(s));
            let clearcoat_normal_map =
                object.clearcoat_normal_map.and_then(|s| inputs.texture_2d_slot(s));
            let specular_map = object.specular_map.and_then(|s| inputs.texture_2d_slot(s));
            let specular_color_map =
                object.specular_color_map.and_then(|s| inputs.texture_2d_slot(s));
            let transmission_map = object.transmission_map.and_then(|s| inputs.texture_2d_slot(s));
            let volume_thickness_map =
                object.volume_thickness_map.and_then(|s| inputs.texture_2d_slot(s));
            let diffuse_transmission_map = object.diffuse_transmission_map.and_then(|s| inputs.texture_2d_slot(s));
            let diffuse_transmission_color_map = object.diffuse_transmission_color_map.and_then(|s| inputs.texture_2d_slot(s));

            // `object.transform` already defaults to `Transform::default()`
            // (identity) when scene_object's own `transform` input is
            // unwired (D2) — matches the old scattered params' defaults
            // exactly (pos 0, rot 0, scale 1).
            let t = object.transform;
            let rot_euler = if t.billboard {
                t.billboard_rot_euler(cam.pos)
            } else {
                t.rot_euler
            };
            // In Group space the part's place inside one copy is its whole
            // chain: the groups between it and the copied group.
            let in_copy = object.instances_space == manifold_node_engine::scene::scene_object::InstanceSpace::Group
                && object.instances.is_some();
            let local_model =
                if in_copy { t.world_with_rotation(rot_euler) } else { model_matrix(t.pos, rot_euler, t.scale) };
            let parent_model = object.parent_transform.map(|parent| {
                let parent_rot = if parent.billboard {
                    parent.billboard_rot_euler(cam.pos)
                } else {
                    parent.rot_euler
                };
                parent.world_with_rotation(parent_rot)
            });
            let (model, instance_inner) = instance_space_pair(
                parent_model,
                local_model,
                object.instances_space,
                object.instances.is_some(),
            );
            // GBUFFER_DESIGN.md section 2 D5 (P2): `None` at this slot (no history
            // yet — a brand-new node, or the slot right after a rebuild)
            // seeds prev = current, giving THIS object exactly-zero
            // first-frame velocity. Stored immediately after reading (not
            // gated on `velocity_wired`) — see the `prev_view_proj` comment
            // above for why continuous tracking is the correct default.
            let [prev_model_n, prev_inner_n] = self
                .prev_model
                .get(n)
                .copied()
                .flatten()
                .unwrap_or([model, instance_inner]);
            if let Some(slot) = self.prev_model.get_mut(n) {
                *slot = Some([model, instance_inner]);
            }
            let mut uniforms = build_uniforms(
                *view_proj,
                model,
                cam,
                &material,
                object.emission_strength,
                *light_count as f32,
                atmosphere,
                *prev_view_proj,
                prev_model_n,
                object.gain,
                weights.is_some(),
                render_mode.point_size,
            );
            uniforms.instance_inner = instance_inner;
            uniforms.appearance[3] = *env_rotation;
            uniforms.prev_instance_inner = prev_inner_n;
            if single_object.is_some() {
                uniforms.appearance[2] = (1u32
                    | if legacy_roughness_map.is_some() { 2 } else { 0 }
                    | if legacy_metallic_map.is_some() { 4 } else { 0 }) as f32;
            }
            // TAA/MetalFX velocity jitter exclusion (see the field's doc):
            // the fragment subtracts (cur − prev) from the baked-in-jitter
            // clip varyings. Zero whenever temporal_upscale is off.
            uniforms.velocity_jitter = [
                jitter_ndc.0,
                jitter_ndc.1,
                prev_jitter_ndc.0,
                prev_jitter_ndc.1,
            ];
            if base_color_map.is_some() {
                uniforms.texture_flags[2] = 1.0; // z = base_color_map present (matches resolve_albedo's texture_flags.z gate)
            }
            // IMPORT_FIDELITY_DESIGN.md D3/F-P2 presence flags.
            if normal_map.is_some() {
                uniforms.texture_flags[0] = 1.0; // x = normal_map present (resolve_normal's cotangent-frame gate)
            }
            if mr_map.is_some() {
                uniforms.texture_flags2[0] = 1.0; // x = mr_map present (resolve_mr's gate)
            }
            if occlusion_map.is_some() {
                uniforms.texture_flags2[1] = 1.0; // y = occlusion_map present (resolve_occlusion's gate)
            }
            if emissive_map.is_some() {
                uniforms.texture_flags2[2] = 1.0; // z = emissive_map present (resolve_emissive's gate)
            }
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E3/E4/E5 (D1 revised):
            // five new presence flags packed into the five reserved `w`/`y`
            // slots the E1 uniform migration already left inert (no struct
            // growth — same reuse doctrine as ior/specular_factor riding
            // pbr_metallic_roughness.zw and clearcoat riding alpha_params.zw).
            if sheen_color_map.is_some() {
                uniforms.texture_flags[1] = 1.0; // y = sheen_color_map present (resolve_sheen's gate)
            }
            if sheen_roughness_map.is_some() {
                uniforms.texture_flags[3] = 1.0; // w = sheen_roughness_map present
            }
            if iridescence_map.is_some() {
                uniforms.texture_flags2[3] = 1.0; // w = iridescence_map present
            }
            if iridescence_thickness_map.is_some() {
                uniforms.anisotropy_dispersion_params[3] = 1.0; // w = iridescence_thickness_map present
            }
            if anisotropy_map.is_some() {
                uniforms.transmission_volume_params[3] = 1.0; // w = anisotropy_map present
            }
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — texture-
            // completion sweep): every reserved single-flag `w` slot the E1
            // migration left is now spent (see the five ifs above), so
            // these seven new presence bits are packed as a bitmask into
            // the LAST two reserved `w` slots (`pbr_specular_tint.w`,
            // `volume_attenuation_color.w`) rather than growing the
            // uniform again — `fs_pbr`'s decode does
            // `u32(round(w)) & (1u << bit)`. Default 0.0 (no bits set) is
            // byte-identical to before this bitmask existed.
            let mut specular_family_flags: u32 = 0;
            if specular_map.is_some() {
                specular_family_flags |= 1; // bit 0 = specular_map present
            }
            if specular_color_map.is_some() {
                specular_family_flags |= 2; // bit 1 = specular_color_map present
            }
            if transmission_map.is_some() {
                specular_family_flags |= 4; // bit 2 = transmission_map present
            }
            if volume_thickness_map.is_some() {
                specular_family_flags |= 8; // bit 3 = volume_thickness_map present
            }
            if diffuse_transmission_map.is_some() {
                specular_family_flags |= 16;
            }
            if diffuse_transmission_color_map.is_some() {
                specular_family_flags |= 32;
            }
            uniforms.pbr_specular_tint[3] = specular_family_flags as f32;
            let mut clearcoat_family_flags: u32 = 0;
            if clearcoat_map.is_some() {
                clearcoat_family_flags |= 1; // bit 0 = clearcoat_map present
            }
            if clearcoat_roughness_map.is_some() {
                clearcoat_family_flags |= 2; // bit 1 = clearcoat_roughness_map present
            }
            if clearcoat_normal_map.is_some() {
                clearcoat_family_flags |= 4; // bit 2 = clearcoat_normal_map present
            }
            uniforms.volume_attenuation_color[3] = clearcoat_family_flags as f32;

            // GLB_XFAIL_BURNDOWN_DESIGN.md D3: read while `material` is
            // still in scope (it's consumed by `build_uniforms` above by
            // reference, still live here).
            let sampler_descs = [
                material.base_color_sampler,
                material.normal_sampler,
                material.mr_sampler,
                material.occlusion_sampler,
                material.emissive_sampler,
            ];

            let alpha_mode = material.alpha_mode;
            let is_blend = alpha_mode == AlphaMode::Blend;
            let is_transmissive = material.kind == MaterialKind::Pbr
                && material.transmission_factor > 0.0;
            if is_transmissive {
                has_transmission = true;
            }

            // D11: wired instances_n draws the optional live count, clamped
            // to buffer capacity (0 → that object's draw becomes a legal
            // instance-count-0 no-op); unwired draws once via the identity
            // stub (bound at Pass 2 — this object carries no self-owned
            // buffer reference).
            let instances_slot = object.instances;
            let instances = instances_slot.and_then(|s| inputs.array_slot(s));
            let instances_content = instances_slot.and_then(|s| inputs.content_version_of(s));
            let instance_capacity = instances
                .map(|buf| (buf.size / instance_size).min(u64::from(u32::MAX)) as u32);
            let instance_count = effective_instance_count(instance_capacity, object.instance_count);

            let points = color_pass_points(&render_mode);
            let pipeline = {
                let gpu = ctx.gpu_encoder();
                self.pipeline_for(gpu.device, material.kind, *velocity_wired, *ao_mask_wired, *denoise_aux_ready, is_blend || is_transmissive, points)
                    .clone()
            };

            // IMPORT_FIDELITY_DESIGN.md D8/F-P5: view-space depth of this
            // object's translation (see the `sort_depth` doc comment above).
            let world_pos = [model[3][0], model[3][1], model[3][2]];
            let sort_depth = (world_pos[0] - cam.pos[0]) * cam.fwd[0]
                + (world_pos[1] - cam.pos[1]) * cam.fwd[1]
                + (world_pos[2] - cam.pos[2]) * cam.fwd[2];

            draws.push(ObjectDraw {
                slot: n as u32,
                vertices,
                weights,
                uniforms,
                map_uniforms: material_map_uniforms(&material),
                subsurface: material.subsurface,
                subsurface_binding: [0.0; 4],
                pipeline,
                base_color_map,
                normal_map,
                mr_map,
                legacy_roughness_map,
                legacy_metallic_map,
                occlusion_map,
                emissive_map,
                sheen_color_map,
                sheen_roughness_map,
                iridescence_map,
                iridescence_thickness_map,
                anisotropy_map,
                clearcoat_map,
                clearcoat_roughness_map,
                clearcoat_normal_map,
                specular_map,
                specular_color_map,
                transmission_map,
                volume_thickness_map,
                diffuse_transmission_map,
                diffuse_transmission_color_map,
                sampler_descs,
                instances,
                instance_count,
                vertex_count: object_vertex_count,
                point_count,
                live_extent,
                indices,
                indices_content,
                live_args: None,
                vertices_content,
                mesh_revision: mesh_slot.and_then(|s| ctx.inputs.mesh_revision_of(s)),
                topology_hint: object.indices.or(object.topology).and_then(|slot| inputs.content_version_of(slot)),
                geometry_content_known: [object.mesh, object.indices, object.instances, object.topology]
                    .into_iter().flatten().all(|slot| inputs.content_version_of(slot).is_some()),
                appearance_content_known: [object.base_color_map, object.normal_map, object.mr_map, object.emissive_map, object.anisotropy_map, object.specular_map, object.specular_color_map, object.clearcoat_map, object.clearcoat_roughness_map, object.sheen_color_map, object.sheen_roughness_map, object.transmission_map, object.volume_thickness_map, object.clearcoat_normal_map, object.iridescence_map, object.iridescence_thickness_map, object.diffuse_transmission_map, object.diffuse_transmission_color_map, object.occlusion_map, object.weights]
                    .into_iter().flatten().all(|slot| inputs.content_version_of(slot).is_some()),
                rt_texture_content: [object.base_color_map, object.normal_map, object.mr_map, object.emissive_map, object.anisotropy_map, object.specular_map, object.specular_color_map, object.clearcoat_map, object.clearcoat_roughness_map, object.sheen_color_map, object.sheen_roughness_map, object.transmission_map, object.volume_thickness_map, object.clearcoat_normal_map, object.iridescence_map, object.iridescence_thickness_map, object.diffuse_transmission_map, object.diffuse_transmission_color_map, object.occlusion_map]
                    .map(|slot| slot.and_then(|slot| ctx.inputs.content_version_of(slot))),
                instances_content,
                weights_content: weights_slot.and_then(|s| inputs.content_version_of(s)),
                gain: object.gain,
                alpha_mode,
                sort_depth,
                is_transmissive,
                cast_shadows: object.cast_shadows,
                kind: material.kind,
                fill_mode: color_pass_fill_mode(&render_mode),
                points,
            });
        }

        // Points mode draws every vertex, which the triangle-count
        // arguments do not carry, so its instances stay direct.
        let trimmed = |d: &ObjectDraw| d.instances.is_some() && d.instance_count > 0 && !d.points;
        let live = draws.iter().filter(|d| d.live_extent.is_some() || trimmed(d)).count();
        if live > 0 {
            let gpu = ctx.gpu_encoder();
            let args = self.live_draw_args.prepare(gpu.device, live);
            for (slot, draw) in draws.iter_mut().filter(|d| d.live_extent.is_some() || trimmed(d)).enumerate() {
                let instances = if trimmed(draw) { 0 } else { draw.instance_count };
                match &draw.live_extent {
                    Some(extent) => {
                        let capacity = draw.indices.map_or_else(
                            || whole_triangle_vertices(draw.vertices),
                            |indices| (indices.size / 4 / 3 * 3).min(u64::from(u32::MAX)) as u32,
                        );
                        self.live_draw_args.write(gpu.native_enc, slot, extent, capacity, instances);
                    }
                    None => self.live_draw_args.write_fixed(gpu.native_enc, slot, draw.vertex_count),
                }
                draw.live_args = Some((args.clone(), slot as u64 * live_draw_args::ARGS_BYTES));
            }
            if draws.iter().any(trimmed) {
                gpu.native_enc.compute_memory_barrier_buffers();
                for (slot, draw) in draws.iter().filter(|d| d.live_extent.is_some() || trimmed(d)).enumerate() {
                    if let (true, Some(instances)) = (trimmed(draw), draw.instances) {
                        self.live_draw_args.trim(gpu.native_enc, slot, instances, draw.instance_count);
                    }
                }
            }
        }
        Some((draws, has_transmission))
    }

    /// A frame with no visible scene objects still owns every graph output.
    /// Clear those outputs so a previous frame cannot remain on screen, and
    /// discard temporal history so the first visible frame starts cold.
    pub(super) fn clear_empty_frame<'ctx, 'gpu>(&mut self, ctx: &mut EffectNodeContext<'ctx, 'gpu>) {
        self.invalidate_temporal_history();

        // Keep these values aligned with the normal MSAA pass: transparent
        // color, reversed-Z depth, zero motion/denoiser feeds, and full AO
        // owed for the background.
        const OUTPUTS: [(&str, [f64; 4]); 10] = [
            ("color", [0.0, 0.0, 0.0, 0.0]),
            ("depth", [0.0, 0.0, 0.0, 0.0]),
            ("velocity", [0.0, 0.0, 0.0, 0.0]),
            ("ao_mask", [1.0, 1.0, 1.0, 1.0]),
            ("normals", [0.0, 0.0, 0.0, 0.0]),
            ("roughness", [0.0, 0.0, 0.0, 0.0]),
            ("diffuse_albedo", [0.0, 0.0, 0.0, 0.0]),
            ("specular_albedo", [0.0, 0.0, 0.0, 0.0]),
            ("specular_hit_distance", [0.0, 0.0, 0.0, 0.0]),
            ("reactive_mask", [0.0, 0.0, 0.0, 0.0]),
        ];
        for (port, clear) in OUTPUTS {
            if let Some(target) = ctx.outputs.texture_2d(port) {
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(target, clear[0], clear[1], clear[2], clear[3]);
            }
        }
    }

    /// Drop all history that can feed a future visible frame. Resources stay
    /// cached; this is the same invalidation used by `clear_state`, without
    /// restarting the jitter sequence during a transient empty scene.
    pub(super) fn invalidate_temporal_history(&mut self) {
        self.subsurface_pass.reset();
        self.rt_history_ping = 0;
        self.rt_irr_needs_reset = true;
        self.rt_moments_valid = false;
        self.rt_prev_accumulating = false;
        self.prev_temporal_upscale = false;
        self.prev_model.fill(None);
        self.prev_view_proj = None;
        self.prev_cam_state = None;
        self.prev_jitter_ndc = None;
    }

    pub(super) fn frame_preliminaries<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        port_index: &ahash::AHashMap<&'static str, manifold_node_engine::bindings::Slot>,
    ) -> Option<FrameInit<'ctx>> {
        let objects = self.num_objects as usize;
        let lights_n = self.num_lights as usize;

        // RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P4 (R5): built ONCE per
        // frame from this frame's wired ports (`bindings.rs`'s
        // `NodeInputs::build_index`), then every per-light/per-object port
        // lookup below resolves through it in O(1) instead of the
        // `format!` + linear `iter().find` scan section 1's CPU row measured as
        // O(objects × wired_ports) ≈ O(objects²). Port NAMES themselves are
        // pre-formatted once in `rebuild()` (`object_port_names` /
        // `light_port_names`) — nothing here calls `format!`.

        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);
        let envmap_pending = ctx.inputs.port_pending("envmap");
        let envmap_wired = ctx.inputs.texture_2d("envmap").filter(|_| !envmap_pending);
        let env_rotation = match ctx.params.get("env_rotation") {
            Some(ParamValue::Float(turn)) => *turn,
            _ => 0.0,
        };
        // Scene-wide atmosphere (P3). Unwired = Atmosphere::default() = fog
        // density 0 = no fog (the shader's exp fog collapses to identity), so
        // an unwired atmosphere is byte-identical to no atmosphere.
        let atmosphere = ctx.inputs.atmosphere("atmosphere").unwrap_or_default();
        // Scene-wide render mode (SCENE_RENDER_MODE_DESIGN.md D2). Unwired =
        // RenderMode::default() = mode Rendered = the raster pass draws
        // exactly as today, byte-identical to no render_mode input.
        let render_mode = ctx.inputs.render_mode("render_mode_in").unwrap_or_else(|| {
            use manifold_node_engine::scene::render_mode::{RENDER_MODE_POINTS, RENDER_MODE_RENDERED, RenderMode};
            let mode = match ctx.params.get("render_mode") {
                // Physics View draws the scene as Rendered; the water switches itself.
                Some(ParamValue::Enum(mode)) if *mode <= RENDER_MODE_POINTS => *mode,
                _ => RENDER_MODE_RENDERED,
            };
            RenderMode { mode, ..RenderMode::default() }
        });

        // One light interpretation for surfaces and shafts: position/direction
        // plus mode, premultiplied colour plus caster slot, and range. Keep
        // stable compacted caster IDs through the RT budget; raster consumers
        // use only its first MAX_RASTER_SHADOW_CASTERS entries. Lights beyond
        // either shadow budget still contribute direct illumination.
        let mut light_data: Vec<[f32; 4]> = Vec::with_capacity(lights_n * LIGHT_VEC4_STRIDE);
        let mut light_count: u32 = 0;
        let mut casters: Vec<manifold_node_engine::scene::light::Light> =
            Vec::with_capacity(manifold_gpu::raytrace::MAX_RT_CASTERS);
        // VOLUMETRIC_LIGHT_DESIGN.md D2 (P3): every wired light (Sun AND
        // Point) contributes to the march — 3-vec4-per-light packing,
        // matching `shaft_march.wgsl`'s `shaft_lights` binding(2) layout
        // field-for-field:
        //   [i*4+0] = Sun: dir-toward-light (.xyz, matches
        //             `Light::light_dir_at`'s Sun case), .w = 0.0 (mode Sun)
        //           = Point: light world position (.xyz), .w = 1.0 (mode Point)
        //   [i*4+1] = premultiplied color.rgb, .w = caster slot (-1 =
        //             unshadowed glow, D2's honest cost)
        //   [i*4+2] = range, falloff, cos(inner), cos(outer)
        //   [i*4+3] = spot forward direction (.xyz), .w = 0
        // Reuses the SAME `slot` this loop already computed (the caster
        // table below is shared, unfiltered by mode).
        let mut shaft_light_data: Vec<[f32; 4]> = Vec::new();
        let mut shaft_light_count: u32 = 0;
        self.rendered_lights.clear();
        for i in 0..lights_n {
            let light_slot = port_index.get(self.light_port_names[i].as_ref()).copied();
            if let Some(l) = light_slot.and_then(|s| ctx.inputs.light_slot(s)) {
                self.rendered_lights.push(l.pos);
                let slot: f32 = if l.cast_shadows && casters.len() < manifold_gpu::raytrace::MAX_RT_CASTERS {
                    let s = casters.len() as f32;
                    casters.push(l);
                    s
                } else {
                    -1.0
                };
                light_count += 1;
                let packed = l.packed(slot);
                light_data.extend_from_slice(&packed);
                shaft_light_data.extend_from_slice(&packed);
                shaft_light_count += 1;
            }
        }
        // D4: `@binding(8)` must ALWAYS be bound (the fragment shaders
        // declare it unconditionally), so a zero-light scene binds one
        // zeroed entry the shader never reads (`light_count == 0`). Skipping
        // the binding on empty is the forbidden silent-fallback shape.
        if light_data.is_empty() {
            light_data.extend([[0.0f32; 4]; LIGHT_VEC4_STRIDE]);
        }
        // Same D4 always-bind discipline for `shaft_lights` — the
        // "still-empty, pad to one zeroed stub" pass moved to just before
        // the march dispatch (RAYTRACING_DESIGN.md section 5.2 P3 appends
        // emissive pseudo-lights to `shaft_light_data` LATER in this
        // function, after `opaque_draws` is built; padding here,
        // before those appends, would leave a stub 3-vec4 at index 0 that
        // `shaft_light_count` (still 0 at THIS point) never accounts for,
        // desyncing the shader's `li * LIGHT_STRIDE` indexing from the
        // real appended entries).

        // Caster table (`@binding(9)`): `MAX_RASTER_SHADOW_CASTERS` slots ×
        // `CASTER_VEC4_STRIDE` vec4, zeroed then filled per active caster.
        // Columns 0–3 = shadow_view_proj; vec4 4 = (bias, kernel_half_width,
        // texel_size, light_size). Always fully sized so `@binding(9)` is a
        // fixed bind regardless of caster count (Bytes, copy-at-encode,
        // hazard-free).
        let mut caster_table: Vec<[f32; 4]> =
            vec![[0.0; 4]; MAX_RASTER_SHADOW_CASTERS * CASTER_VEC4_STRIDE];
        for (slot, l) in casters.iter().take(MAX_RASTER_SHADOW_CASTERS).enumerate() {
            let vp = l.shadow_view_proj();
            let base = slot * CASTER_VEC4_STRIDE;
            caster_table[base] = vp[0];
            caster_table[base + 1] = vp[1];
            caster_table[base + 2] = vp[2];
            caster_table[base + 3] = vp[3];
            // D12: `Contact` has no fixed kernel width — `kernel_half_width()`
            // returns the sentinel `-1`, which `render_scene.wgsl`'s
            // `shadow_factor` reads as "run the PCSS blocker-search branch,
            // using `light_size` below" instead of the fixed PCF loop.
            let khw = l.shadow_softness.kernel_half_width() as f32;
            let light_size = match l.shadow_softness {
                manifold_node_engine::scene::light::ShadowSoftness::Contact { light_size } => light_size,
                _ => 0.0,
            };
            let texel = 1.0 / l.shadow_resolution.clamp(256, 4096) as f32;
            caster_table[base + 4] = [l.shadow_bias, khw, texel, light_size];
        }

        let dims_tex = ctx.outputs.texture_2d("color")?;
        let width = dims_tex.width;
        let height = dims_tex.height;
        if width == 0 || height == 0 {
            return None;
        }
        let aspect = width as f32 / height as f32;
        // RAYTRACING_DESIGN.md section 8.2 D22 point 1/3 (T2-B): the per-scene
        // MetalFX Temporal toggle. When on, the WHOLE render (Pass A/B,
        // shafts, the RT half-res ray pass) draws at RENDER res —
        // `native_dim * RT_TEMPORAL_RENDER_SCALE_NUM / _DEN` — into
        // `rt_temporal_color_scratch`, and MetalFX Temporal upscales that
        // scratch color to native res as this node's `color` output at the
        // very end of this fn. `width`/`height` (and everything downstream
        // that reads them — MSAA/velocity/RT-mask/shadow-snapshot sizing,
        // the jitter NDC conversion below, half-res RT dispatch dims) are
        // shadowed to render res for exactly this reason: D22 point 3, "the
        // RT half-res ray pass now keys off render res... that compounding
        // is the point". `native_width`/`native_height` are kept for the
        // two things that must stay at the TRUE canvas size: the
        // upscaler's dst dims and the final blit destination.
        let temporal_upscale_param = matches!(ctx.params.get("temporal_upscale"), Some(ParamValue::Bool(true)));
        // A live toggle cannot resize the compiled depth/velocity attachments
        // (see PresetRuntime::refresh_plan_if_forced_outputs_changed). Their
        // dimensions, rather than the pending parameter, determine whether
        // this frame uses MetalFX's reduced-resolution render path.
        let reduced_dims = (
            scale_dim(width, RT_TEMPORAL_RENDER_SCALE_NUM, RT_TEMPORAL_RENDER_SCALE_DEN),
            scale_dim(height, RT_TEMPORAL_RENDER_SCALE_NUM, RT_TEMPORAL_RENDER_SCALE_DEN),
        );
        // A live toggle does not resize the compiled graph's attachments.
        // Keep drawing at their committed resolution until the host rebuilds,
        // in BOTH directions (native -> reduced and reduced -> native).
        let temporal_upscale = (temporal_upscale_param || reduced_dims != (width, height))
            && ["depth", "velocity"].iter().all(|port| {
                ctx.outputs.texture_2d(port)
                    .is_some_and(|t| (t.width, t.height) == reduced_dims)
            });
        if temporal_upscale_param != temporal_upscale && !self.rt_temporal_unavailable_logged {
            self.rt_temporal_unavailable_logged = true;
            log::warn!(
                "node.render_scene: temporal_upscale changed but the compiled depth/velocity size has not; retaining the compiled render resolution until the scene runtime rebuilds"
            );
        }
        let native_width = width;
        let native_height = height;
        let width = if temporal_upscale {
            scale_dim(native_width, RT_TEMPORAL_RENDER_SCALE_NUM, RT_TEMPORAL_RENDER_SCALE_DEN)
        } else {
            width
        };
        let height = if temporal_upscale {
            scale_dim(native_height, RT_TEMPORAL_RENDER_SCALE_NUM, RT_TEMPORAL_RENDER_SCALE_DEN)
        } else {
            height
        };
        let mut view_proj = cam.view_proj(aspect);
        // RAYTRACING_DESIGN.md section 5.2 P4: subpixel camera jitter, applied
        // only when `temporal_upscale` is on (default off = byte-identical
        // to before this param existed). Standard TAA/MetalFX jitter
        // technique: add `jitter * clip.w` to clip.x/clip.y so the offset
        // survives the perspective divide as a CONSTANT NDC shift
        // independent of a vertex's depth. Since every point has
        // homogeneous `w = 1`, `clip.w`'s only nonzero contribution (for
        // this camera's projection matrices — `perspective_rh`/`ortho_rh`
        // in `camera.rs`) comes through the z-column, so it's enough to
        // add `jitter_ndc * view_proj[2][3]` into `view_proj[2][{0,1}]`
        // (column-major storage: `m[col][row]`, matching `mat4_mul_vec4`'s
        // `out[row] = sum_col m[col][row] * v[col]`). `jitter_offset`
        // returns PIXEL units at the render resolution (now `width`/
        // `height` above, D22); 1 pixel = `2.0 / dim` in NDC (NDC spans
        // [-1, 1] across `dim` pixels).
        self.jitter_frame_index = self.jitter_frame_index.wrapping_add(1);
        // T2-B: hoisted out of the `if` below so the final MetalFX upscale
        // call (end of this fn) can reuse the SAME jitter this frame's
        // color/depth/velocity were rendered with — MetalFX's `reset`
        // contract requires the jitter passed to `upscale()` match what was
        // baked into the frame it's upscaling.
        let mut jitter_px: (f32, f32) = (0.0, 0.0);
        if temporal_upscale {
            let (jx_px, jy_px) =
                crate::metalfx_temporal_upscaler::jitter_offset(self.jitter_frame_index, 8);
            jitter_px = (jx_px, jy_px);
            let wz = view_proj[2][3];
            view_proj[2][0] += (jx_px * 2.0 / width as f32) * wz;
            view_proj[2][1] += (jy_px * 2.0 / height as f32) * wz;
        }
        // RAYTRACING_DESIGN.md RT-D3 (P1-part-2): the scene-level toggle
        // (W0's `rt_enabled` ParamDef). Read once here, after jitter is
        // folded into `view_proj` — the RT pass's `inv_view_proj` must
        // match the SAME `view_proj` the main draw uses this frame.
        let rt_enabled = matches!(ctx.params.get("rt_enabled"), Some(ParamValue::Bool(true)));
        // RAYTRACING_DESIGN.md section 9 RD9 (T4): per-scene reflection toggle,
        // gated on rt_enabled — inert when RT is off entirely. Default ON
        // (Q3). T5 fine-tunes the spp/roughness-band constants.
        let rt_reflections = rt_enabled
            && matches!(ctx.params.get("rt_reflections"), Some(ParamValue::Bool(true)));
        // RT term toggles — per-term card params. Each defaults ON = today's
        // behavior. Inert when rt_enabled is false. Live-flip detection routes
        // through rt_irr_needs_reset to avoid the old term smearing.
        let rt_shadows_enabled = rt_enabled
            && matches!(ctx.params.get("rt_shadows"), Some(ParamValue::Bool(true)));
        let rt_ao_enabled = rt_enabled
            && matches!(ctx.params.get("rt_ao"), Some(ParamValue::Bool(true)));
        let rt_gi_enabled = rt_enabled
            && matches!(ctx.params.get("rt_gi"), Some(ParamValue::Bool(true)));
        // RT-Stage-3 P1 (BUG-mkgh): pre-blur firefly clamp toggle. Default
        // ON. Read once here (folded with rt_enabled like the other terms)
        // so the tail can gate the clamp dispatch without a second param
        // lookup after the `ctx` mutable borrows below.
        let rt_firefly_clamp_enabled = rt_enabled
            && matches!(ctx.params.get("rt_firefly_clamp"), Some(ParamValue::Bool(true)));
        // RT_QUALITY_SETTINGS_DESIGN.md D5/D6: the active quality column,
        // resolved per frame by the compositor from project settings.
        // Copied out of ctx here — the dispatch code below runs after
        // gpu_encoder() mutable borrows, where ctx is unreadable.
        let rtq = ctx.rt_quality;
        // RT-Stage-3 P4 (BUG-eytk): denoise strength/iterations copied
        // out of ctx here — same borrow-lift pattern as rtq itself (the
        // dispatch code below runs after gpu_encoder() mutable borrows,
        // where ctx is unreadable).
        let denoise_strength = rtq.denoise_strength;
        let denoise_iterations = rtq.denoise_iterations;
        // Trace dispatch dims (D4): one ray-resolution fraction for both
        // dispatches, truncating u64 math per output_canvas_scale discipline.
        let rt_trace_w = ((width as u64 * rtq.ray_res_num as u64 / rtq.ray_res_den as u64) as u32).max(1);
        let rt_trace_h = ((height as u64 * rtq.ray_res_num as u64 / rtq.ray_res_den as u64) as u32).max(1);
        // Detect toggle flips: any term that was on last frame and is now off
        // (or vice versa) needs history reset so the old signal doesn't
        // trail. Routed through rt_irr_needs_reset — the ONE existing path
        // (the negative-rg gate). First frame (None = no prior state) only
        // fires the TemporalResetDetector; toggles only matter on flip.
        let toggle_flipped = rt_enabled && (
            self.rt_prev_toggle_shadows.is_some_and(|p| p != rt_shadows_enabled)
            || self.rt_prev_toggle_ao.is_some_and(|p| p != rt_ao_enabled)
            || self.rt_prev_toggle_gi.is_some_and(|p| p != rt_gi_enabled)
            || self.rt_prev_toggle_refl.is_some_and(|p| p != rt_reflections)
        );
        self.rt_prev_toggle_shadows = Some(rt_shadows_enabled);
        self.rt_prev_toggle_ao = Some(rt_ao_enabled);
        self.rt_prev_toggle_gi = Some(rt_gi_enabled);
        self.rt_prev_toggle_refl = Some(rt_reflections);
        // Successful current-frame encoding authorizes tracing, not prior completion.
        let rt_ready = false;
        // RAYTRACING_DESIGN.md section 5.2 P2/D3, RT-D2, section 8.2 D22 (T2-B): the ONE
        // `detect_reset` call site for every temporal consumer in this node
        // (negative-`rg` gate: no second reset path). Unconditional, once
        // per frame — the detector's own contract — so the velocity-history
        // reset below sees cuts/seeks even on non-RT, non-upscale scenes.
        // BUG-vn9b (motion-blur-cut-smear): under the old
        // `will_rt_accumulate || temporal_upscale` gate the detector never
        // advanced on those scenes, so `prev_view_proj`/`prev_model`
        // survived a clip cut and the first post-cut frame smeared at full
        // max_blur_px. Consumer off→on resumes now force a reset explicitly
        // via the `*_just_resumed` latches below — that used to fall out of
        // the gated detector's time-jump.
        let topology_changed = self.mesh_topology_history.update(
            ctx.rebuild_epoch,
            // A pending object is absent this frame, so its drop and its
            // return both reset temporal history.
            self.object_port_names.iter().take(objects).map(|port| {
                port_index.get(port.as_ref()).copied()
                    .filter(|&slot| ctx.inputs.slot_content_ready(slot))
                    .and_then(|slot| ctx.inputs.object_slot(slot))
                    .map(|object| (
                        object.mesh,
                        object.mesh.and_then(|slot| ctx.inputs.mesh_revision_of(slot)).map(|r| r.topology),
                        object.topology.and_then(|slot| ctx.inputs.content_version_of(slot)),
                    ))
            }),
        );
        let reset_decision = self.rt_reset_detector.detect_reset(ctx.owner_key, &ctx.time)
            || topology_changed;
        // A cut/seek means this node's velocity history is stale too: clear
        // it so the first post-cut frame takes the "no history yet"
        // zero-velocity seeding instead of ndc deltas measured across two
        // different scenes.
        if reset_decision {
            self.prev_model.iter_mut().for_each(|m| *m = None);
            self.prev_view_proj = None;
            self.prev_cam_state = None;
        }
        let upscale_just_resumed = temporal_upscale && !self.prev_temporal_upscale;
        self.prev_temporal_upscale = temporal_upscale;
        // D22: whether the scratch/upscaler are actually live this frame —
        // `temporal_upscale` folded with hardware availability, assigned
        // inside the "Ensure cached GPU resources" block below (the first
        // point a `&GpuDevice` is available). Declared here so Pass 2 and
        // this fn's tail (after that block's mutable borrow of `ctx` ends)
        // can both read the final decision.
        let temporal_upscale_active = false;
        // RAYTRACING_DESIGN.md section 17.5 DN-F/DN-G: whether the ML
        // denoiser is actually live this frame — rt_enabled && rt_ready &&
        // denoise_feed && hardware availability, assigned in the ensure
        // block below. Declared here so `target` and this fn's tail can
        // both read the final decision (same pattern as its temporal
        // counterpart). When true, suppresses `temporal_upscale_active`
        // (DN2: the denoiser REPLACES the plain temporal scaler).
        let denoise_active = false;
        // RT-Stage-3 P1 (BUG-mkgh): whether the RT trace + accumulate
        // dispatches actually ran this frame — set true inside the
        // `rt_ready && topo-key-match` RT block, false otherwise (the
        // raster path serves the accel-not-ready/topo-transition frames).
        // Gates the firefly clamp: clamping when RT didn't render is a
        // forbidden move. Declared here (mut, assigned later) so the tail
        // can read it — same pattern as `denoise_active`.
        let rt_rendered_this_frame = false;
        // RT-Stage-3 P4 (BUG-eytk): whether the post-accumulation filter
        // ran this frame — gates the composite rebind (the composite
        // binds `rt_irr_filtered` when true, raw history slot when false).
        let irr_filtered_valid = false;
        // GBUFFER_DESIGN.md section 2 D1: lazy — `velocity` costs nothing unless a
        // consumer actually wired it (checked once per frame, cheap: a
        // step-output lookup, not a texture allocation).
        let velocity_wired = ctx.outputs.texture_2d("velocity").is_some();
        // RAYTRACING_DESIGN.md section 12 AM1: same D1 lazy rule for the
        // ao_mask aux output.
        let ao_mask_wired = ctx.outputs.texture_2d("ao_mask").is_some();
        // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): denoise G-buffer feed
        // flag. When on, the forward pass writes normals + roughness +
        // diffuse/specular albedo as additional MRT outputs. Same D1 lazy
        // enabling: `force_consumed_outputs` gates plan allocation, and the
        // pipeline selection below gates which shader variant runs.
        let denoise_feed =
            matches!(ctx.params.get("rt_denoise_feed"), Some(ParamValue::Bool(true)));
        // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): specular hit-distance
        // output — extracted early so the hit-dist extraction dispatch in
        // the RT section below can reference it without borrowing ctx.
        let spec_hit_dist_out = ctx.outputs.texture_2d("specular_hit_distance");
        // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): denoise G-buffer
        // resolve targets — read early (before the ensure block's mutable
        // ctx borrow) so denoise_aux_ready is computed once and drives
        // every downstream production decision.
        let normals_resolve_target = ctx.outputs.texture_2d("normals");
        let roughness_resolve_target = ctx.outputs.texture_2d("roughness");
        let diffuse_albedo_resolve_target = ctx.outputs.texture_2d("diffuse_albedo");
        let specular_albedo_resolve_target = ctx.outputs.texture_2d("specular_albedo");
        // DN-L (section 17.7): reactive mask — same all-or-none gate.
        let reactive_resolve_target = ctx.outputs.texture_2d("reactive_mask");
        // RAYTRACING_DESIGN.md section 17.5 DN-E/DN-G + BUG-qtkq: one engage
        // decision per evaluate; on the live-flip frame the pre-flip plan has
        // not allocated the feeds, so the whole denoise production path idles
        // one frame byte-identical to flag-off and the rebuilt plan engages
        // next frame (D17 transition shape).
        let denoise_aux_ready = denoise_feed
            && normals_resolve_target.is_some()
            && roughness_resolve_target.is_some()
            && diffuse_albedo_resolve_target.is_some()
            && specular_albedo_resolve_target.is_some()
            && reactive_resolve_target.is_some()
            && spec_hit_dist_out.is_some();
        // VOLUMETRIC_LIGHT_DESIGN.md D1/D3 (P2): the sole CPU gate for the
        // whole light-shaft feature, checked once per frame like
        // `velocity_wired`. `depth_wired` decides whether the march reads
        // the graph-allocated resolve target or the internal one (D3:
        // "shafts-on forces the internal Sample0 depth resolve even when
        // `depth` is unwired").
        let wants_shafts_now = wants_shafts(&atmosphere);
        let depth_wired = ctx.outputs.texture_2d("depth").is_some();
        // GBUFFER_DESIGN.md section 2 D5 (P2): `None` (no history yet) seeds to
        // THIS frame's own `view_proj`, so a scene's first-ever evaluate()
        // has prev == current and velocity is exactly zero — not
        // approximately. Stored immediately (not gated on `velocity_wired`)
        // so history is tracked continuously: if velocity is wired mid-
        // session, the first wired frame sees the object's REAL prior
        // motion, not a spurious first-frame zero.
        // Periodic sources identify the equivalent copy across a coordinate
        // wrap. Share that correspondence across velocity, RT reprojection
        // and motion conditioning instead of clearing valid temporal history.
        let world_offset = self.prev_cam_state.map_or([0.0; 3], |previous| {
            cam.previous_world_offset(previous.pos, previous.world_period)
        });
        let prev_view_proj = self.prev_view_proj.map_or(view_proj, |previous| {
            let translation = [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [world_offset[0], world_offset[1], world_offset[2], 1.0],
            ];
            if world_offset == [0.0; 3] { previous } else { mat4_mul(previous, translation) }
        });
        self.prev_view_proj = Some(view_proj);
        // Camera-motion magnitude for the accumulator's change gates
        // (`AccumulateParams::cam_motion`): radians of view-direction turn
        // plus translation weighted into the same units (0.3: at the
        // typical 2–4 unit object distance, orbiting contributes roughly
        // equally through both terms). First frame after load/rebuild is
        // 0 — a held camera feeds the gates exactly 0, keeping the static
        // path byte-identical.
        let cam_motion = match self.prev_cam_state {
            Some(previous) => {
                let ppos = std::array::from_fn::<_, 3, _>(|i| previous.pos[i] - world_offset[i]);
                let pfwd = previous.fwd;
                let d = (pfwd[0] * cam.fwd[0] + pfwd[1] * cam.fwd[1] + pfwd[2] * cam.fwd[2])
                    .clamp(-1.0, 1.0);
                let rot = d.acos();
                let dp = ((cam.pos[0] - ppos[0]).powi(2)
                    + (cam.pos[1] - ppos[1]).powi(2)
                    + (cam.pos[2] - ppos[2]).powi(2))
                .sqrt();
                rot + dp * 0.3
            }
            None => 0.0,
        };
        self.prev_cam_state = Some(cam);
        self.rendered_camera = Some(cam);
        // MetalFX velocity jitter exclusion (`velocity_jitter` uniform):
        // this frame's and last frame's jitter as NDC offsets. MetalFX
        // expects motion vectors jitter-free (its jitterOffset compensates
        // the current frame); both clip varyings carry jitter baked in, so
        // the fragment subtracts the delta. Zero when upscale is off.
        let jitter_ndc = (
            jitter_px.0 * 2.0 / width as f32,
            jitter_px.1 * 2.0 / height as f32,
        );
        let prev_jitter_ndc = self.prev_jitter_ndc.replace(jitter_ndc).unwrap_or(jitter_ndc);

        Some((
            FramePrelude {
                objects,
                cam,
                envmap_wired,
                envmap_pending,
                env_rotation,
                atmosphere,
                render_mode,
                light_data,
                light_count,
                casters,
                caster_table,
                native_width,
                native_height,
                width,
                height,
                aspect,
                temporal_upscale,
                view_proj,
                prev_view_proj,
                jitter_px,
                jitter_ndc,
                prev_jitter_ndc,
                cam_motion,
                rt_enabled,
                rt_reflections,
                rt_shadows_enabled,
                rt_ao_enabled,
                rt_gi_enabled,
                rt_firefly_clamp_enabled,
                rtq,
                denoise_strength,
                denoise_iterations,
                rt_trace_w,
                rt_trace_h,
                toggle_flipped,
                reset_decision,
                upscale_just_resumed,
                velocity_wired,
                ao_mask_wired,
                denoise_feed,
                denoise_aux_ready,
                spec_hit_dist_out,
                normals_resolve_target,
                roughness_resolve_target,
                diffuse_albedo_resolve_target,
                specular_albedo_resolve_target,
                reactive_resolve_target,
                wants_shafts_now,
                depth_wired,
            },
            FrameRtState {
                rt_ready,
                temporal_upscale_active,
                denoise_active,
                rt_rendered_this_frame,
                irr_filtered_valid,
            },
            (shaft_light_data, shaft_light_count),
        ))
    }
}
