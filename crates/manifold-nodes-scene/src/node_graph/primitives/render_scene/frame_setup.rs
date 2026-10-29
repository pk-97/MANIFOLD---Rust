//! Per-frame setup: the frame preamble (inputs, light tables, the one
//! reset-detector call, camera and jitter history), the per-object draw
//! list, the empty-frame clear and temporal-history invalidation.

use super::*;
use manifold_gpu::BufferUse;

/// Bytes of one `InstanceTransform` record.
const INSTANCE_RECORD_BYTES: u64 = std::mem::size_of::<InstanceTransform>() as u64;

/// `slot`'s buffer, reallocated only when it holds fewer than `bytes`.
fn grown_buffer(
    slot: &mut Option<manifold_gpu::GpuBuffer>,
    device: &manifold_gpu::GpuDevice,
    bytes: u64,
) -> manifold_gpu::GpuBuffer {
    if slot.as_ref().is_none_or(|buffer| buffer.size < bytes) {
        *slot = Some(device.create_buffer(bytes, BufferUse::Gpu));
    }
    slot.clone().expect("buffer allocated")
}

impl RenderScene {
    /// BUG-trh7 stage 2, pass 0: the frame preamble — per-frame inputs, the
    /// light/caster tables, temporal-upscale resolution, RT toggle latches,
    /// the ONE reset-detector call, and the camera/jitter history. Returns
    /// None on the two early-frame aborts (no color output, zero dims) — the
    /// caller (evaluate) returns immediately, exactly as the inline code did.
    /// BUG-trh7 stage 2, pass 1: validate every object's required inputs,
    /// compose its model matrix + uniforms, and get-or-compile its pipeline.
    /// None = abort frame: a structured error and a magenta clear.
    /// An empty draw list is returned for the caller to clear all connected
    /// outputs and invalidate temporal history.
    pub(super) fn collect_object_draws<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        port_index: &ahash::AHashMap<&'static str, manifold_node_engine::bindings::Slot>,
        single_object: Option<SceneObject>,
    ) -> Option<(Vec<ObjectDraw<'ctx>>, Vec<FieldObject<'ctx>>, bool)> {
        let FramePrelude {
            objects,
            cam,
            envmap_wired,
            envmap_pending,
            env_rotation,
            atmosphere,
            render_mode,
            view_proj,
            prev_view_proj,
            jitter_ndc,
            prev_jitter_ndc,
            light_count,
            velocity_wired,
            ao_mask_wired,
            denoise_aux_ready,
            rt_enabled,
            temporal_upscale,
            width,
            native_width,
            ..
        } = pre;
        // SCENE_RENDER_MODE_DESIGN.md D4/INV-R4: the effective mode for this
        // frame's raster path. `rt_enabled` scenes ignore the wire entirely
        // (Rendered uniform set) — and because the substitution happens
        // here, the gi_materials table the RT pass builds from these draws
        // never sees a wireframe override either.
        let render_mode = effective_render_mode(render_mode, *rt_enabled);
        let instance_size = std::mem::size_of::<InstanceTransform>() as u64;

        let mut draws: Vec<ObjectDraw<'ctx>> = Vec::with_capacity(*objects);
        let mut fields: Vec<FieldObject<'ctx>> = Vec::new();
        // The sole CPU gate for the
        // Pass A/B split below. Only `Blend` objects ever reach Pass B
        // (`is_glass` in `gltf_import.rs` always routes a nonzero
        // `transmission_factor` to `Blend` alpha mode, so this condition and
        // "blend_draw_calls is non-empty" are the same fact) — checking both
        // here keeps that invariant explicit rather than assumed.
        let mut has_transmission = false;

        for n in 0..*objects {
            // Every object's
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
            let Some(object) =
                single_object.or_else(|| object_slot_id.and_then(|s| ctx.inputs.object_slot(s)))
            else {
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
            if let Some(field) = object.field {
                match self.collect_field(ctx, pre, n, &object, field) {
                    Some(Some(f)) => fields.push(f),
                    Some(None) => self.report_incomplete_frame(ctx),
                    None => return None,
                }
                continue;
            }
            let mesh_slot = object.mesh;
            // Never draw or trace unlanded bytes: the object drops out.
            if mesh_slot.is_some_and(|s| !ctx.inputs.slot_content_ready(s)) {
                self.report_incomplete_frame(ctx);
                continue;
            }
            let Some(vertices) = mesh_slot.and_then(|s| ctx.inputs.array_slot(s)) else {
                ctx.gpu_encoder()
                    .merge_frame_status(FrameRenderStatus::Failed(
                        FrameRenderFailure::InvalidGeometry,
                    ));
                Self::fail_frame(
                    ctx,
                    Some(n as u32),
                    format!(
                        "object_{n}: missing required `vertices` input (its scene_object's `vertices` port is unwired); renderer fell back to magenta clear"
                    ),
                );
                return None;
            };
            // This object's
            // `mesh_n` write generation, feeds the shadow cache key below.
            let vertices_content = mesh_slot.and_then(|s| ctx.inputs.content_version_of(s));
            if object
                .indices
                .is_some_and(|slot| !ctx.inputs.slot_content_ready(slot))
            {
                self.report_incomplete_frame(ctx);
                continue;
            }
            let indices = object.indices.and_then(|slot| ctx.inputs.array_slot(slot));
            if object.indices.is_some() && indices.is_none() {
                ctx.gpu_encoder()
                    .merge_frame_status(FrameRenderStatus::Failed(
                        FrameRenderFailure::InvalidGeometry,
                    ));
                Self::fail_frame(
                    ctx,
                    Some(n as u32),
                    format!("object_{n}: wired index buffer is unavailable"),
                );
                return None;
            }
            let indices_content = object
                .indices
                .and_then(|slot| ctx.inputs.content_version_of(slot));
            let capacity = indices.map_or_else(
                || whole_triangle_vertices(vertices),
                |buffer| (buffer.size / 4 / 3 * 3).min(u64::from(u32::MAX)) as u32,
            );
            let live_extent = object
                .indices
                .or(mesh_slot)
                .and_then(|s| ctx.inputs.live_extent_slot(s));
            let (object_vertex_count, point_count) = match &live_extent {
                Some(extent) => {
                    let bound = extent.bound.min(capacity);
                    (bound, bound)
                }
                None => (
                    capacity,
                    indices.map_or(
                        (vertices.size / std::mem::size_of::<MeshVertex>() as u64) as u32,
                        |_| capacity,
                    ),
                ),
            };
            let weights_slot = object.weights;
            if weights_slot.is_some_and(|s| !ctx.inputs.slot_content_ready(s)) {
                self.report_incomplete_frame(ctx);
                continue;
            }
            let weights = weights_slot.and_then(|s| ctx.inputs.array_slot(s));
            if weights_slot.is_some() && weights.is_none() {
                ctx.gpu_encoder()
                    .merge_frame_status(FrameRenderStatus::Failed(
                        FrameRenderFailure::InvalidGeometry,
                    ));
                Self::fail_frame(
                    ctx,
                    Some(n as u32),
                    format!(
                        "object_{n}: wired appearance weights input is unavailable; renderer fell back to magenta clear"
                    ),
                );
                return None;
            }
            let vertex_count = (vertices.size / std::mem::size_of::<MeshVertex>() as u64) as u32;
            if let Some(weights) = weights
                && ((weights.size / std::mem::size_of::<f32>() as u64) as u32) < vertex_count
            {
                ctx.gpu_encoder()
                    .merge_frame_status(FrameRenderStatus::Failed(
                        FrameRenderFailure::InvalidGeometry,
                    ));
                Self::fail_frame(
                    ctx,
                    Some(n as u32),
                    format!(
                        "object_{n}: appearance weights buffer is shorter than the mesh ({}, need {vertex_count}); renderer fell back to magenta clear",
                        weights.size / std::mem::size_of::<f32>() as u64
                    ),
                );
                return None;
            }
            // Appearance weights and
            // gain are SUPPORTED under RT — the old rejection here is
            // removed now that `rt_dynamic_coverage_and_attributes` proves
            // the coverage/brightness behavior against the raster formula.
            let material = match Self::object_material(
                ctx,
                n,
                object.material,
                &render_mode,
                envmap_wired.is_some(),
                *envmap_pending,
            ) {
                ObjectMaterial::Ready(material) => material,
                ObjectMaterial::Late => {
                    self.report_incomplete_frame(ctx);
                    continue;
                }
                ObjectMaterial::Failed => return None,
            };
            // `NodeInputs` is `Copy` (bindings.rs) — capture it by value so
            // these lookups don't hold `ctx` borrowed (`ctx.error`/
            // `ctx.outputs`/`ctx.gpu_encoder()` below still need mutably).
            let inputs = ctx.inputs;
            let base_color_map = object
                .base_color_map
                .and_then(|s| inputs.texture_2d_slot(s));
            // The four new optional
            // per-object texture ports.
            let normal_map = object.normal_map.and_then(|s| inputs.texture_2d_slot(s));
            let mr_map = object.mr_map.and_then(|s| inputs.texture_2d_slot(s));
            let legacy_roughness_map =
                single_object.and_then(|_| inputs.texture_2d("roughness_map"));
            let legacy_metallic_map = single_object.and_then(|_| inputs.texture_2d("metallic_map"));
            let occlusion_map = object.occlusion_map.and_then(|s| inputs.texture_2d_slot(s));
            let emissive_map = object.emissive_map.and_then(|s| inputs.texture_2d_slot(s));
            // Sheen, iridescence and anisotropy maps.
            let sheen_color_map = object
                .sheen_color_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let sheen_roughness_map = object
                .sheen_roughness_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let iridescence_map = object
                .iridescence_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let iridescence_thickness_map = object
                .iridescence_thickness_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let anisotropy_map = object
                .anisotropy_map
                .and_then(|s| inputs.texture_2d_slot(s));
            // Texture completion sweep.
            let clearcoat_map = object.clearcoat_map.and_then(|s| inputs.texture_2d_slot(s));
            let clearcoat_roughness_map = object
                .clearcoat_roughness_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let clearcoat_normal_map = object
                .clearcoat_normal_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let specular_map = object.specular_map.and_then(|s| inputs.texture_2d_slot(s));
            let specular_color_map = object
                .specular_color_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let transmission_map = object
                .transmission_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let volume_thickness_map = object
                .volume_thickness_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let diffuse_transmission_map = object
                .diffuse_transmission_map
                .and_then(|s| inputs.texture_2d_slot(s));
            let diffuse_transmission_color_map = object
                .diffuse_transmission_color_map
                .and_then(|s| inputs.texture_2d_slot(s));

            let (model, instance_inner) = Self::object_model(&object, cam.pos);
            let [prev_model_n, prev_inner_n] = self.advance_prev_model(n, model, instance_inner);
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
                    | if legacy_metallic_map.is_some() { 4 } else { 0 })
                    as f32;
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
            if *temporal_upscale {
                uniforms.render_mode[1] = (*width as f32 / (*native_width).max(1) as f32).log2();
            }
            if base_color_map.is_some() {
                uniforms.texture_flags[2] = 1.0; // z = base_color_map present (matches resolve_albedo's texture_flags.z gate)
            }
            // Presence flags.
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
            // Five new presence flags:
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
            // Texture completion sweep: every reserved single-flag `w` slot is now spent (see the five ifs above), so
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

            // Read while `material` is
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
            let is_transmissive =
                material.kind == MaterialKind::Pbr && material.transmission_factor > 0.0;

            // D11: wired instances_n draws the optional live count, clamped
            // to buffer capacity (0 → that object's draw becomes a legal
            // instance-count-0 no-op); unwired draws once via the identity
            // stub (bound at Pass 2 — this object carries no self-owned
            // buffer reference).
            let instances_slot = object.instances;
            let instances = instances_slot.and_then(|s| inputs.array_slot(s));
            let instances_content = instances_slot.and_then(|s| inputs.content_version_of(s));
            let instance_capacity =
                instances.map(|buf| (buf.size / instance_size).min(u64::from(u32::MAX)) as u32);
            let instance_count = effective_instance_count(instance_capacity, object.instance_count);
            // Transmission turns on the snapshot passes and the opaque-depth
            // redraw; a glass object that draws no instances needs neither.
            if is_transmissive && instance_count > 0 {
                has_transmission = true;
            }

            let points = color_pass_points(&render_mode);
            let fill_mode = color_pass_fill_mode(&render_mode);
            let opaque_fast = !is_blend
                && !is_transmissive
                && !points
                && fill_mode == manifold_gpu::GpuTriangleFillMode::Fill
                && solid_surface(&uniforms);
            let parallax = parallax_surface(material.kind, &uniforms);
            let pipeline = {
                let gpu = ctx.gpu_encoder();
                self.pipeline_for(
                    gpu.device,
                    material.kind,
                    *velocity_wired,
                    *ao_mask_wired,
                    *denoise_aux_ready,
                    is_blend || is_transmissive,
                    points,
                    opaque_fast,
                    parallax,
                )
                .clone()
            };

            let geometry_content_known = [
                object.mesh,
                object.indices,
                object.instances,
                object.topology,
            ]
            .into_iter()
            .flatten()
            .all(|slot| inputs.content_version_of(slot).is_some());
            // The producer's box stands for the vertices while the mesh is
            // known content and filled triangles (a line or a point reaches
            // past its vertex). The per-copy camera cull places it with each
            // copy's own transform; a live extent only drops vertices inside
            // it.
            let copy_bounds = (geometry_content_known
                && fill_mode == manifold_gpu::GpuTriangleFillMode::Fill
                && !points)
                .then(|| mesh_slot.and_then(|s| inputs.cpu_value_slot::<Option<MeshBounds>>(s)))
                .flatten()
                .flatten();
            // The whole-object test needs nothing but the model matrix to
            // place the vertices: no instances, no live extent. It is tested
            // against the same jittered `view_proj` the draw uniforms carry,
            // after the previous-model write above, which a skipped draw
            // still needs for its velocity on re-entry.
            let bounds = (object.instances.is_none() && live_extent.is_none())
                .then_some(copy_bounds)
                .flatten();
            let in_view = !bounds.is_some_and(|b| box_outside_clip(view_proj, &model, &b));

            // View-space depth of this
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
                prev_positions: None,
                provided_prev: object
                    .prev_positions
                    .filter(|&slot| ctx.inputs.slot_content_ready(slot))
                    .and_then(|slot| ctx.inputs.array_slot(slot)),
                prev_instances: None,
                topology_hint: object
                    .indices
                    .or(object.topology)
                    .and_then(|slot| inputs.content_version_of(slot)),
                geometry_content_known,
                appearance_content_known: [
                    object.base_color_map,
                    object.normal_map,
                    object.mr_map,
                    object.emissive_map,
                    object.anisotropy_map,
                    object.specular_map,
                    object.specular_color_map,
                    object.clearcoat_map,
                    object.clearcoat_roughness_map,
                    object.sheen_color_map,
                    object.sheen_roughness_map,
                    object.transmission_map,
                    object.volume_thickness_map,
                    object.clearcoat_normal_map,
                    object.iridescence_map,
                    object.iridescence_thickness_map,
                    object.diffuse_transmission_map,
                    object.diffuse_transmission_color_map,
                    object.occlusion_map,
                    object.weights,
                ]
                .into_iter()
                .flatten()
                .all(|slot| inputs.content_version_of(slot).is_some()),
                rt_texture_content: [
                    object.base_color_map,
                    object.normal_map,
                    object.mr_map,
                    object.emissive_map,
                    object.anisotropy_map,
                    object.specular_map,
                    object.specular_color_map,
                    object.clearcoat_map,
                    object.clearcoat_roughness_map,
                    object.sheen_color_map,
                    object.sheen_roughness_map,
                    object.transmission_map,
                    object.volume_thickness_map,
                    object.clearcoat_normal_map,
                    object.iridescence_map,
                    object.iridescence_thickness_map,
                    object.diffuse_transmission_map,
                    object.diffuse_transmission_color_map,
                    object.occlusion_map,
                ]
                .map(|slot| slot.and_then(|slot| ctx.inputs.content_version_of(slot))),
                instances_content,
                weights_content: weights_slot.and_then(|s| inputs.content_version_of(s)),
                gain: object.gain,
                alpha_mode,
                sort_depth,
                is_transmissive,
                cast_shadows: object.cast_shadows,
                closed: object.closed,
                kind: material.kind,
                fill_mode,
                points,
                bounds,
                copy_bounds,
                camera_cull: None,
                in_view,
            });
        }

        let live = draws
            .iter()
            .filter(|d| d.live_extent.is_some() || d.trims_instances())
            .count();
        let live_meshes = fields
            .iter()
            .filter(|f| f.mesh.as_ref().is_some_and(|m| m.live_extent.is_some()))
            .count();
        if live + live_meshes > 0 {
            let culled = draws.iter().filter(|d| d.culls_copies()).count();
            let gpu = ctx.gpu_encoder();
            let args = self
                .live_draw_args
                .prepare(gpu.device, live + culled + live_meshes);
            for (slot, draw) in draws
                .iter_mut()
                .filter(|d| d.live_extent.is_some() || d.trims_instances())
                .enumerate()
            {
                let instances = if draw.trims_instances() {
                    0
                } else {
                    draw.instance_count
                };
                match &draw.live_extent {
                    Some(extent) => {
                        let capacity = draw.indices.map_or_else(
                            || whole_triangle_vertices(draw.vertices),
                            |indices| (indices.size / 4 / 3 * 3).min(u64::from(u32::MAX)) as u32,
                        );
                        self.live_draw_args.write(
                            gpu.native_enc,
                            slot,
                            extent,
                            capacity,
                            instances,
                        );
                    }
                    None => {
                        self.live_draw_args
                            .write_fixed(gpu.native_enc, slot, draw.vertex_count)
                    }
                }
                draw.live_args = Some((args.clone(), slot as u64 * live_draw_args::ARGS_BYTES));
            }
            if draws.iter().any(|d| d.trims_instances()) {
                gpu.native_enc.compute_memory_barrier_buffers();
                for (slot, draw) in draws
                    .iter()
                    .filter(|d| d.live_extent.is_some() || d.trims_instances())
                    .enumerate()
                {
                    if let (true, Some(instances)) = (draw.trims_instances(), draw.instances) {
                        self.live_draw_args.trim(
                            gpu.native_enc,
                            slot,
                            instances,
                            draw.instance_count,
                        );
                    }
                }
            }
            // Each culled draw's view block follows the live blocks; its
            // survivors land in a per-slot buffer sized for every copy.
            let mut view_block = live as u64;
            for draw in draws.iter_mut().filter(|d| d.culls_copies()) {
                let Some(buffer) = self.culled_instances.get_mut(draw.slot as usize) else {
                    continue;
                };
                let bytes = u64::from(draw.instance_count) * INSTANCE_RECORD_BYTES;
                draw.camera_cull = Some(CameraCull {
                    offset: view_block * live_draw_args::ARGS_BYTES,
                    instances: grown_buffer(buffer, gpu.device, bytes),
                    prev: None,
                });
                view_block += 1;
            }
            // The water meshes' blocks follow the view blocks.
            let mut block = (live + culled) as u64;
            for mesh in fields.iter_mut().filter_map(|f| f.mesh.as_mut()) {
                let Some(extent) = &mesh.live_extent else {
                    continue;
                };
                self.live_draw_args.write(
                    gpu.native_enc,
                    block as usize,
                    extent,
                    whole_triangle_vertices(mesh.vertices),
                    1,
                );
                mesh.live_args = Some((args.clone(), block * live_draw_args::ARGS_BYTES));
                block += 1;
            }
        }
        // A field draws in Pass B, so it needs every resource the glass gate
        // ensures: the opaque depth snapshot, the scene-colour snapshot and
        // the Pass B depth states.
        has_transmission |= !fields.is_empty();
        Some((draws, fields, has_transmission))
    }

    /// Cull each camera-culled draw's copies against the camera's clip volume
    /// into its survivor buffers and view block. Runs after the trim and the
    /// previous-record plan, which it reads, and before any pass draws.
    pub(super) fn encode_copy_cull(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        draws: &mut [ObjectDraw<'_>],
    ) {
        if draws.iter().all(|d| d.camera_cull.is_none()) {
            return;
        }
        let gpu = ctx.gpu_encoder();
        // The trim wrote the instance words the view blocks copy.
        gpu.native_enc.compute_memory_barrier_buffers();
        for draw in draws.iter_mut() {
            let (Some(cull), Some(bounds), Some(source), Some((_, live_offset))) = (
                draw.camera_cull.as_mut(),
                draw.copy_bounds.as_ref(),
                draw.instances,
                draw.live_args.as_ref(),
            ) else {
                continue;
            };
            let prev_source = draw
                .prev_instances
                .as_ref()
                .and_then(prev_positions::PrevDraw::bound);
            cull.prev = None;
            if prev_source.is_some()
                && let Some(buffer) = self.culled_prev.get_mut(draw.slot as usize)
            {
                let bytes = u64::from(draw.instance_count) * INSTANCE_RECORD_BYTES;
                cull.prev = Some(grown_buffer(buffer, gpu.device, bytes));
            }
            self.live_draw_args.cull(
                gpu.native_enc,
                &live_draw_args::CullDraw {
                    clip: &draw.uniforms.view_proj,
                    model: &draw.uniforms.model,
                    inner: &draw.uniforms.instance_inner,
                    bounds,
                    count: draw.instance_count,
                    live_block: (live_offset / live_draw_args::ARGS_BYTES) as u32,
                    view_block: (cull.offset / live_draw_args::ARGS_BYTES) as u32,
                    instances: source,
                    out_instances: &cull.instances,
                    prev: prev_source.zip(cull.prev.as_ref()),
                },
            );
        }
        gpu.native_enc.compute_memory_barrier_buffers();
    }

    /// Abort the frame: report why, naming the render object at fault when
    /// there is one, and clear `color` to magenta. The color target is a
    /// recycled pool texture, so without the clear it would show whatever
    /// another frame left in it.
    pub(super) fn fail_frame(
        ctx: &mut EffectNodeContext<'_, '_>,
        object: Option<u32>,
        message: impl Into<String>,
    ) {
        match object {
            Some(object) => ctx.error_on_object(object, message),
            None => ctx.error(message),
        }
        if let Some(target) = ctx.outputs.texture_2d("color") {
            ctx.gpu_encoder()
                .native_enc
                .clear_texture(target, 1.0, 0.0, 1.0, 1.0);
        }
    }

    /// A frame with no visible scene objects still owns every graph output.
    /// Clear those outputs so a previous frame cannot remain on screen, and
    /// discard temporal history so the first visible frame starts cold.
    pub(super) fn clear_empty_frame<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
    ) {
        self.invalidate_temporal_history();
        self.water_march.reset();

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
        self.selection.reset();
        self.rt_history_ping = 0;
        self.rt_irr_needs_reset = true;
        self.rt_moments_valid = false;
        self.rt_prev_accumulating = false;
        self.prev_temporal_upscale = false;
        self.prev_model.fill(None);
        self.forget_prev_positions();
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

        // Built ONCE per
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
            use manifold_node_engine::scene::render_mode::{
                RENDER_MODE_POINTS, RENDER_MODE_RENDERED, RenderMode,
            };
            let mode = match ctx.params.get("render_mode") {
                // Physics View draws the scene as Rendered; the water switches itself.
                Some(ParamValue::Enum(mode)) if *mode <= RENDER_MODE_POINTS => *mode,
                _ => RENDER_MODE_RENDERED,
            };
            RenderMode {
                mode,
                ..RenderMode::default()
            }
        });

        // One light interpretation for surfaces and shafts: position/direction
        // plus mode, premultiplied colour plus caster slot, and range. Every
        // light with `cast_shadows` gets the next caster slot.
        let mut light_data: Vec<[f32; 4]> = Vec::with_capacity(lights_n * LIGHT_VEC4_STRIDE);
        let mut light_count: u32 = 0;
        let mut casters: Vec<manifold_node_engine::scene::light::Light> = Vec::new();
        // Every wired light (Sun AND
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
                let slot: f32 = if l.cast_shadows {
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

        // The caster table both renderers read, one entry per caster slot.
        let shadow_casters: Vec<manifold_gpu::ShadowCaster> =
            casters.iter().map(shadow_caster_entry).collect();

        let dims_tex = ctx.outputs.texture_2d("color")?;
        let width = dims_tex.width;
        let height = dims_tex.height;
        if width == 0 || height == 0 {
            return None;
        }
        let aspect = width as f32 / height as f32;
        // RAYTRACING_DESIGN.md section 8.2 D22 point 1/3 (T2-B): the per-scene
        // MetalFX Temporal toggle. When on, the WHOLE render (Pass A/B,
        // shafts, the RT half-res ray pass) draws at RENDER res — the
        // `upscale_quality` fraction of native — into
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
        let temporal_upscale_param = matches!(
            ctx.params.get("temporal_upscale"),
            Some(ParamValue::Bool(true))
        );
        // A live change to `temporal_upscale` or `upscale_quality` cannot
        // resize the compiled depth/velocity attachments (see
        // PresetRuntime::refresh_plan_if_forced_outputs_changed). Their
        // dimensions, rather than the pending params, set the render size:
        // the frame keeps drawing at the committed resolution until the host
        // rebuilds, whichever way the change goes. `output_canvas_scale` sized
        // them, so they agree with each other and sit below the canvas exactly
        // when the compiled plan renders reduced.
        let attachment_dims =
            |port: &str| ctx.outputs.texture_2d(port).map(|t| (t.width, t.height));
        let (render_width, render_height) =
            match (attachment_dims("depth"), attachment_dims("velocity")) {
                (Some(depth), Some(velocity)) if depth == velocity => depth,
                _ => (width, height),
            };
        let temporal_upscale = (render_width, render_height) != (width, height);
        if temporal_upscale_param != temporal_upscale && !self.rt_temporal_unavailable_logged {
            self.rt_temporal_unavailable_logged = true;
            log::warn!(
                "node.render_scene: temporal_upscale changed but the compiled depth/velocity size has not; retaining the compiled render resolution until the scene runtime rebuilds"
            );
        }
        let native_width = width;
        let native_height = height;
        let width = render_width;
        let height = render_height;
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
        // Since `view_proj = P * V`, `clip.w` has contributions from all
        // four columns of `view_proj`, so `jitter_ndc * col[3]` is added
        // into `col[0]` and `col[1]` across all 4 columns.
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
            let jx_ndc = jx_px * 2.0 / width as f32;
            let jy_ndc = jy_px * 2.0 / height as f32;
            for col in &mut view_proj {
                let w = col[3];
                col[0] += jx_ndc * w;
                col[1] += jy_ndc * w;
            }
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
            && matches!(
                ctx.params.get("rt_reflections"),
                Some(ParamValue::Bool(true))
            );
        // RT term toggles — per-term card params. Each defaults ON = today's
        // behavior. Inert when rt_enabled is false. Live-flip detection routes
        // through rt_irr_needs_reset to avoid the old term smearing.
        let rt_shadows_enabled =
            rt_enabled && matches!(ctx.params.get("rt_shadows"), Some(ParamValue::Bool(true)));
        let rt_ao_enabled =
            rt_enabled && matches!(ctx.params.get("rt_ao"), Some(ParamValue::Bool(true)));
        let rt_gi_enabled =
            rt_enabled && matches!(ctx.params.get("rt_gi"), Some(ParamValue::Bool(true)));
        // RT-Stage-3 P1 (BUG-mkgh): pre-blur firefly clamp toggle. Default
        // ON. Read once here (folded with rt_enabled like the other terms)
        // so the tail can gate the clamp dispatch without a second param
        // lookup after the `ctx` mutable borrows below.
        let rt_firefly_clamp_enabled = rt_enabled
            && matches!(
                ctx.params.get("rt_firefly_clamp"),
                Some(ParamValue::Bool(true))
            );
        // The active quality column,
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
        let rt_trace_w =
            ((width as u64 * rtq.ray_res_num as u64 / rtq.ray_res_den as u64) as u32).max(1);
        let rt_trace_h =
            ((height as u64 * rtq.ray_res_num as u64 / rtq.ray_res_den as u64) as u32).max(1);
        // Detect toggle flips: any term that was on last frame and is now off
        // (or vice versa) needs history reset so the old signal doesn't
        // trail. Routed through rt_irr_needs_reset — the ONE existing path
        // (the negative-rg gate). First frame (None = no prior state) only
        // fires the TemporalResetDetector; toggles only matter on flip.
        let toggle_flipped = rt_enabled
            && (self
                .rt_prev_toggle_shadows
                .is_some_and(|p| p != rt_shadows_enabled)
                || self.rt_prev_toggle_ao.is_some_and(|p| p != rt_ao_enabled)
                || self.rt_prev_toggle_gi.is_some_and(|p| p != rt_gi_enabled)
                || self
                    .rt_prev_toggle_refl
                    .is_some_and(|p| p != rt_reflections));
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
                port_index
                    .get(port.as_ref())
                    .copied()
                    .filter(|&slot| ctx.inputs.slot_content_ready(slot))
                    .and_then(|slot| ctx.inputs.object_slot(slot))
                    .map(|object| {
                        (
                            object.mesh,
                            object
                                .mesh
                                .and_then(|slot| ctx.inputs.mesh_revision_of(slot))
                                .map(|r| r.topology),
                            object
                                .topology
                                .and_then(|slot| ctx.inputs.content_version_of(slot)),
                        )
                    })
            }),
        );
        // A camera cut (a switch picking another camera) is a new shot: its
        // first frame has no history from the camera before it.
        let camera_cut = self
            .prev_cam_state
            .is_some_and(|previous| previous.shot != cam.shot);
        let reset_decision = self
            .rt_reset_detector
            .detect_reset(ctx.owner_key, &ctx.time)
            || topology_changed
            || camera_cut;
        // A cut/seek means this node's velocity history is stale too: clear
        // it so the first post-cut frame takes the "no history yet"
        // zero-velocity seeding instead of ndc deltas measured across two
        // different scenes.
        if reset_decision {
            self.prev_model.iter_mut().for_each(|m| *m = None);
            self.forget_prev_positions();
            self.prev_view_proj = None;
            self.prev_jitter_ndc = None;
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
        // Lazy — `velocity` costs nothing unless a
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
        let denoise_feed = matches!(
            ctx.params.get("rt_denoise_feed"),
            Some(ParamValue::Bool(true))
        );
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
        // The sole CPU gate for the
        // whole light-shaft feature, checked once per frame like
        // `velocity_wired`. `depth_wired` decides whether the march reads
        // the graph-allocated resolve target or the internal one (D3:
        // "shafts-on forces the internal Sample0 depth resolve even when
        // `depth` is unwired").
        let wants_shafts_now = wants_shafts(&atmosphere);
        let depth_wired = ctx.outputs.texture_2d("depth").is_some();
        // `None` (no history yet) seeds to
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
            if world_offset == [0.0; 3] {
                previous
            } else {
                mat4_mul(previous, translation)
            }
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
        let prev_jitter_ndc = self
            .prev_jitter_ndc
            .replace(jitter_ndc)
            .unwrap_or(jitter_ndc);

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
                shadow_casters,
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

/// One light's entry in the shared caster table: its shadow-map projection
/// and filter for the raster, its ray source and colour for the ray tracer.
fn shadow_caster_entry(
    l: &manifold_node_engine::scene::light::Light,
) -> manifold_gpu::ShadowCaster {
    use manifold_node_engine::scene::light::{LightMode, ShadowSoftness};
    let light_size = match l.shadow_softness {
        ShadowSoftness::Contact { light_size } => light_size,
        _ => 0.0,
    };
    // D12: `Contact` has no fixed kernel width — `kernel_half_width()`
    // returns the sentinel `-1`, which `shadow_factor` reads as "run the
    // PCSS blocker search using `light_size`".
    let khw = l.shadow_softness.kernel_half_width() as f32;
    let texel = 1.0 / shadow_map_resolution(l.shadow_resolution) as f32;
    // A dark light keeps its slot, but its traced fields stay neutral so its
    // movement cannot disturb RT history; the tracer skips its rays.
    let (source, kind) = if l.color[..3].iter().all(|&channel| channel == 0.0) {
        ([0.0, 1.0, 0.0, 0.0], 0.0)
    } else {
        match l.mode {
            LightMode::Sun => (
                [
                    -l.dir[0],
                    -l.dir[1],
                    -l.dir[2],
                    sun_cone_half_angle(l.shadow_softness),
                ],
                0.0,
            ),
            LightMode::Point | LightMode::Spot => ([l.pos[0], l.pos[1], l.pos[2], light_size], 1.0),
        }
    };
    manifold_gpu::ShadowCaster {
        view_proj: l.shadow_view_proj(),
        shadow: [l.shadow_bias, khw, texel, light_size],
        source,
        color: [l.color[0], l.color[1], l.color[2], kind],
    }
}
