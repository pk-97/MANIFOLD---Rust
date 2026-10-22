//! Per-frame RT geometry: the acceleration-structure object list, accel build or refit, and the per-draw RT consumer flags authored after topology resolves.

use super::*;

impl RenderScene {
    /// Update acceleration structures from current mesh revisions on this encoder.
    /// Returned history invalidation is shared by every temporal consumer.
    pub(super) fn rt_accel_maintenance<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        rt_ready: &mut bool,
        objects: &[manifold_gpu::raytrace::RtObjectGeometry<'ctx>],
        draws: &[ObjectDraw<'ctx>],
        gi_materials_data: &mut Vec<manifold_gpu::raytrace::GiMaterial>,
    ) -> Result<RtFrameTables<'ctx>, FrameRenderFailure> {
            // Reject an unrepresentable material set before publishing tables.
            // Missing texture bindings must never turn a cutout solid or lose a lobe.
            let mut texture_keys: arrayvec::ArrayVec<usize, { manifold_gpu::raytrace::MAX_RT_MATERIAL_TEXTURES }> = arrayvec::ArrayVec::new();
            for object in objects {
                for texture in [object.base_color_texture, object.normal_texture, object.mr_texture, object.emissive_texture]
                    .into_iter().chain(object.extra_material_textures).flatten()
                {
                    let key = texture.identity_key();
                    if !texture_keys.contains(&key) && texture_keys.try_push(key).is_err() {
                        ctx.error(format!("Ray-traced materials exceed the {} unique-texture capacity.", manifold_gpu::raytrace::MAX_RT_MATERIAL_TEXTURES));
                        return Err(FrameRenderFailure::RtAllocation);
                    }
                }
            }
            let opaque_draws = draws
                .iter()
                .filter(|d| !d.routes_to_transparent() && d.instance_count > 0);
            // RS-B: build gi_materials alongside objects (SAME order) for
            // the emissive light table at accel-build time. Reused below
            // for the GPU upload at dispatch time.
            gi_materials_data.clear();
            gi_materials_data.extend(opaque_draws.clone()
                .map(|d| {
                    manifold_gpu::raytrace::GiMaterial::new(
                        [
                            d.uniforms.base_color[0],
                            d.uniforms.base_color[1],
                            d.uniforms.base_color[2],
                        ],
                        [
                            d.uniforms.emission[0] * d.uniforms.emission[3],
                            d.uniforms.emission[1] * d.uniforms.emission[3],
                            d.uniforms.emission[2] * d.uniforms.emission[3],
                        ],
                        [
                            d.uniforms.pbr_metallic_roughness[0],
                            d.uniforms.pbr_metallic_roughness[1],
                            d.uniforms.anisotropy_dispersion_params[0],
                            d.uniforms.anisotropy_dispersion_params[1],
                        ],
                        // RT-TL-B (section 16 TL4): the walk's attenuation
                        // factor — the same `diffuse_transmission_params.x`
                        // the raster forward term reads.
                        d.uniforms.diffuse_transmission_params,
                    ).with_surface(
                        match d.kind {
                            MaterialKind::Unlit => 0.0,
                            MaterialKind::Pbr => 2.0,
                            MaterialKind::Cel => 3.0,
                        },
                        {
                            let mr = d.uniforms.pbr_metallic_roughness;
                            let dielectric = ((mr[2] - 1.0) / (mr[2] + 1.0)).powi(2);
                            let weight = mr[3].clamp(0.0, 1.0);
                            let tint = d.uniforms.pbr_specular_tint;
                            [(dielectric*tint[0]).min(1.0)*weight,
                             (dielectric*tint[1]).min(1.0)*weight,
                             (dielectric*tint[2]).min(1.0)*weight, weight]
                        },
                    ).with_extensions(
                        [d.uniforms.alpha_params[2], d.uniforms.alpha_params[3], d.uniforms.normal_uv_t[3], 0.0],
                        d.uniforms.sheen_params,
                        d.uniforms.iridescence_params,
                        [d.uniforms.transmission_volume_params[0], d.uniforms.pbr_metallic_roughness[2],
                            d.uniforms.transmission_volume_params[1], d.uniforms.anisotropy_dispersion_params[2]],
                        [d.uniforms.volume_attenuation_color[0], d.uniforms.volume_attenuation_color[1],
                            d.uniforms.volume_attenuation_color[2], d.uniforms.transmission_volume_params[2]],
                        [d.uniforms.occlusion_uv_t[2], 0.0, 0.0, 0.0],
                    )
                })
                );
            // RT-TL-B cost recovery (RAYTRACING_DESIGN.md section 16.4):
            // scene-level flag — true if any object's diffuse_transmission_factor > 0.
            self.rt_has_translucency = gi_materials_data.iter().any(|m| m.translucency[0] > 0.0);

            // Structural/instance keys supplement exact per-object mesh revisions.
            use std::hash::{Hash, Hasher};
            let mut hasher = ahash::AHasher::default();
            hasher.write_usize(objects.len());
            for o in objects {
                o.vertex_buffer.identity_key().hash(&mut hasher);
                o.index_buffer.map(manifold_gpu::GpuBuffer::identity_key).hash(&mut hasher);
                hasher.write_u32(o.triangle_count);
                // RT_INSTANCING_DESIGN.md D2/D9/INV-RTI5 + P1.5: instance-slot
                // CAPACITY is topology — the TLAS slot count is baked at
                // build time, so a capacity change prepares a replacement
                // before this frame's update. Same rule as manifold-gpu's
                // `effective_instance_slots` (P1.5: any WIRED object takes
                // the GPU path, 1-capacity included — its TRS is GPU-side).
                // The wired buffer's IDENTITY deliberately does NOT ride
                // here (D9: the descriptor kernel reads whatever buffer is
                // wired each refit); content changes ride the accel key
                // below.
                hasher.write_u32(if o.instances_addr != 0 {
                    o.instance_slots.max(1)
                } else {
                    1
                });
            }
            hasher.write_u64(ctx.rebuild_epoch);
            let topo_key = hasher.finish();
            for (o, d) in objects.iter().zip(opaque_draws.clone()) {
                hasher.write(bytemuck::bytes_of(&o.transform));
                hasher.write(bytemuck::bytes_of(&o.instance_inner));
                // Per-object cast_shadows toggle rewrites only the instance
                // mask (see `refit_accel`), same cheap path as a transform
                // change — folded into the SAME key so a toggle with no
                // transform change still triggers a refit.
                hasher.write_u8(o.cast_shadows as u8);
                // RT_INSTANCING_DESIGN.md D9/INV-RTI4: a wired instances
                // buffer's CONTENT changes refit (descriptor-build
                // re-dispatch + TLAS refit in one command buffer). Static
                // loop/mirror buffers never bump the generation, so the
                // common case is zero per-frame cost. Kept separate from
                // mesh revisions: re-scattering updates instance descriptors
                // without requiring a BLAS rebuild. `None` (unwired) hashes as a distinct state,
                // mirroring the vertices_content Option discipline.
                d.instances_content.hash(&mut hasher);
            }
            let accel_key = hasher.finish();
            // Content key is diagnostic only; revisions drive geometry updates.
            let mut content_hasher = ahash::AHasher::default();
            topo_key.hash(&mut content_hasher);
            for d in opaque_draws.clone() {
                d.vertices_content.hash(&mut content_hasher);
                d.indices_content.hash(&mut content_hasher);
            }
            let content_key = content_hasher.finish();
            // SCENE_MODIFIER_RT_DESIGN.md §5.2 (P4b): appearance state —
            // weights, gain, material values and logical texture content.
            // Hit shading
            // reads these LIVE through the normal-source table (rebuilt
            // every RT-ready frame below). This key refreshes the baked
            // emissive table and invalidates the shared temporal histories
            // (design dirty rule 4 — refresh tables, no BLAS work).
            let mut appearance_hasher = rt_changes::AppearanceKeyBuilder::default();
            for d in opaque_draws.clone() {
                appearance_hasher.content(d.weights_content);
                appearance_hasher.parameter_bytes(&d.gain.to_bits().to_ne_bytes());
            }
            for material in gi_materials_data.iter() {
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.albedo));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.emissive));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.metallic_roughness));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.translucency));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.specular));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.kind));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.clearcoat));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.sheen));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.iridescence));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.transmission));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.attenuation));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&material.surface_misc));
            }
            for draw in opaque_draws.clone() {
                for content in draw.rt_texture_content { appearance_hasher.content(content); }
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&draw.uniforms.alpha_params));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&draw.uniforms.emissive_uv_m));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&draw.uniforms.emissive_uv_t));
                appearance_hasher.parameter_bytes(bytemuck::cast_slice(&draw.map_uniforms));
                appearance_hasher.parameter_bytes(bytemuck::bytes_of(&draw.uniforms.normal_uv_t));
            }
            let appearance_key = appearance_hasher.finish();
            let appearance_changed = !opaque_draws.clone().all(|draw| draw.appearance_content_known)
                || self.rt_appearance_key != Some(appearance_key);

            let structural_changed = self.rt_accel_topo_key != Some(topo_key)
                || self.rt_accel.as_ref().is_none_or(|a| a.check_topology(objects).is_err());
            self.rt_changes.clear();
            for (index, draw) in opaque_draws.clone().enumerate() {
                self.rt_changes.push(rt_changes::classify_mesh_change(
                    self.rt_mesh_revisions[index], draw.mesh_revision,
                    draw.topology_hint, structural_changed || !draw.geometry_content_known,
                ));
            }
            #[cfg(feature = "gpu-proofs")]
            if ctx.gpu.as_ref().is_some_and(|gpu| gpu.force_rt_rebuild) {
                self.rt_changes.fill(RtGeometryChange::Rebuild);
            }
            let geometry_changed = self.rt_changes.iter().any(|c| *c != RtGeometryChange::Reuse);
            let changes = rt_changes::SceneChanges {
                geometry: geometry_changed,
                appearance: appearance_changed,
                instances: structural_changed || !opaque_draws.clone().all(|draw| draw.geometry_content_known)
                    || self.rt_accel_key != Some(accel_key),
            };
            let history_changed = changes.reset_history();
            let instance_changed = changes.update_instances();
            if rt_source_trace_enabled() {
                log::info!("[RT-SOURCE] changes geometry={} appearance={} instances={} reset={}",
                    changes.geometry, changes.appearance, changes.instances, history_changed);
            }
            let gpu = ctx.gpu_encoder();
            // RAYTRACING_DESIGN.md section 5.2 P3: sized to THIS frame's object
            // count, same NLL-borrow reason the tracer/masks/params
            // buffers above are ensured before `opaque_draws`'
            // long-lived immutable borrow starts.
            // RT_INSTANCING_DESIGN.md D11: the GPU table is canonical
            // per-object rows [0, N) + per-slot rows [N, N+Σ), matching
            // `ensure_normal_sources` — object-indexed readers (n4.w
            // roughness, RS-C sampler) use the canonical rows,
            // instance_id-indexed readers add N via params.slot_row_base.
            // A wired draw contributes its live `instance_count` slot rows;
            // unwired draws contribute their one identity slot.
            let rt_gi_slot_count: usize = objects
                .iter()
                .map(|o| if o.instances_addr != 0 { o.instance_slots.max(1) as usize } else { 1 })
                .sum::<usize>()
                + objects.len();
            let tracer = self.rt_tracer.as_ref().expect("ensured before maintenance");
            if structural_changed {
                let plan = tracer.plan_accel(gpu.device, self.rt_accel.as_ref(), objects)
                    .map_err(rt_frame_failure)?;
                let mut additional_bytes = plan.additional_peak_bytes();
                for (capacity, needed, stride) in [
                    (self.rt_gi_materials_capacity, rt_gi_slot_count, std::mem::size_of::<manifold_gpu::raytrace::GiMaterial>()),
                    (self.rt_obj_motion_capacity, objects.len(), std::mem::size_of::<[[f32; 4]; 4]>()),
                    (self.rt_normal_sources_capacity, rt_gi_slot_count, std::mem::size_of::<manifold_gpu::raytrace::RtNormalSource>()),
                    (self.rt_normal_sources_scratch_capacity, rt_gi_slot_count, std::mem::size_of::<manifold_gpu::raytrace::RtNormalSource>()),
                ] {
                    if capacity < needed {
                        additional_bytes = additional_bytes.checked_add((needed * stride) as u64)
                            .ok_or(FrameRenderFailure::RtAllocation)?;
                    }
                }
                manifold_node_engine::load::expand::admit_candidate_bytes(
                    gpu.device.modifier_memory_snapshot(), additional_bytes,
                ).map_err(|error| {
                    log::error!("node.render_scene: RT candidate admission failed: {error}");
                    FrameRenderFailure::RtAllocation
                })?;
                let materials = prepare_rt_table::<manifold_gpu::raytrace::GiMaterial>(
                    gpu.device, self.rt_gi_materials_capacity, rt_gi_slot_count, false)?;
                let motion = prepare_rt_table::<[[f32; 4]; 4]>(
                    gpu.device, self.rt_obj_motion_capacity, objects.len(), false)?;
                let normals = prepare_rt_table::<manifold_gpu::raytrace::RtNormalSource>(
                    gpu.device, self.rt_normal_sources_capacity, rt_gi_slot_count, false)?;
                let normal_scratch = prepare_rt_table::<manifold_gpu::raytrace::RtNormalSource>(
                    gpu.device, self.rt_normal_sources_scratch_capacity, rt_gi_slot_count, true)?;
                self.rt_gi_upload.try_reserve(rt_gi_slot_count.saturating_sub(self.rt_gi_upload.len()))
                    .map_err(|_| FrameRenderFailure::RtAllocation)?;
                self.rt_motion_upload.try_reserve(objects.len().saturating_sub(self.rt_motion_upload.len()))
                    .map_err(|_| FrameRenderFailure::RtAllocation)?;
                tracer.prepare_accel(gpu.device, &mut self.rt_accel, plan)
                    .map_err(rt_frame_failure)?;
                for (replacement, destination, capacity, needed) in [
                    (materials, &mut self.rt_gi_materials, &mut self.rt_gi_materials_capacity, rt_gi_slot_count),
                    (motion, &mut self.rt_obj_motion, &mut self.rt_obj_motion_capacity, objects.len()),
                    (normals, &mut self.rt_normal_sources, &mut self.rt_normal_sources_capacity, rt_gi_slot_count),
                    (normal_scratch, &mut self.rt_normal_sources_scratch, &mut self.rt_normal_sources_scratch_capacity, rt_gi_slot_count),
                ] {
                    if let Some(buffer) = replacement {
                        *destination = Some(buffer);
                        *capacity = needed;
                    }
                }
            }
            // RT-T1-B: same cadence as `gi_materials` above — rebuilt from
            // the SAME `objects` slice every RT-ready frame (below), not
            // gated on the accel-rebuild key (an object's `transform` alone
            // changing, e.g. animation, must refresh its normal_matrix too).
            // RT-T2-A: also returns the ordered alpha-masked-object base-
            // color-texture list, threaded straight to `dispatch_shadow_
            // rays` below (same per-frame cadence — this list can change
            // even when topology/transform don't, e.g. a material swap).
            // BUG-wytp: the same returned list now also carries normal-map
            // textures (and MR maps, since R3), indexed by
            // `RtNormalSource::normal_tex_index`/`mr_tex_index`.
            let alpha_textures = manifold_gpu::raytrace::ensure_normal_sources_snapshot(
                &mut self.rt_normal_sources_scratch,
                &mut self.rt_normal_sources_scratch_capacity,
                &mut self.rt_normal_sources,
                &mut self.rt_normal_sources_capacity,
                gpu.device,
                gpu.native_enc,
                objects,
            );
            let accel = self.rt_accel.as_mut().ok_or(FrameRenderFailure::RtNeedsPreparation)?;
            let update = tracer.encode_accel_update(
                gpu.device, gpu.native_enc, accel, objects, &self.rt_changes,
                gi_materials_data, instance_changed, changes.refresh_emissive(),
            ).map_err(rt_frame_failure)?;
            gpu.rt_updates.blas_builds += update.blas_builds;
            gpu.rt_updates.blas_refits += update.blas_refits;
            gpu.rt_updates.tlas_builds += update.tlas_builds;
            gpu.rt_updates.tlas_refits += update.tlas_refits;
            gpu.rt_updates.emissive_refreshes += update.emissive_refreshes;
            gpu.rt_history_resets += u32::from(history_changed);
            #[cfg(feature = "gpu-proofs")]
            if gpu.capture_rt_geometry {
                let mut history_textures = Vec::new();
                history_textures.extend(self.rt_irr_history.iter().flatten());
                history_textures.extend(self.rt_refl_history.iter().flatten());
                history_textures.extend(self.rt_sv_history.iter().flatten());
                history_textures.extend(self.rt_sv_m1_history.iter().flatten());
                history_textures.extend(self.rt_sv_m2_history.iter().flatten());
                history_textures.extend(self.rt_sv_hold_history.iter().flatten());
                history_textures.extend(self.rt_svt_history.iter().flatten());
                history_textures.extend(self.rt_depth_history.iter().flatten());
                history_textures.extend(self.rt_normal_history.iter().flatten());
                history_textures.extend(self.rt_moments_history.iter().flatten());
                self.rt_probe = Some(rt_proof::RtProbeScene::capture(
                    objects,
                    &alpha_textures,
                    &history_textures,
                ));
            }
            for (index, draw) in opaque_draws.clone().enumerate() {
                self.rt_mesh_revisions[index] = draw.mesh_revision.map(|r| (r, draw.topology_hint));
            }
            self.rt_accel_topo_key = Some(topo_key);
            self.rt_accel_content_key = Some(content_key);
            self.rt_accel_key = Some(accel_key);
            self.rt_appearance_key = Some(appearance_key);
            *rt_ready = true;
        Ok((alpha_textures, topo_key, content_key, history_changed))
    }

    /// BUG-trh7 stage 2, pass 2: resolve resident RT topology BEFORE
    /// authoring any consumer flag — build the AS object list, run
    /// `accel.check_topology` (reject path included), then write the
    /// per-draw RT consumer flags and rebuild Blend pipelines aux-free
    /// when glass routes the Blend group to Pass B. The ordering is the
    /// source-order test's guarantee: a late check would leave uploaded
    /// flags inconsistent with trace suppression. Returns the AS object
    /// list, topology_valid, and rt_just_resumed (both consumed by the
    /// RT block).
    pub(super) fn collect_rt_objects<'ctx>(
        draws: &[ObjectDraw<'ctx>], rt_enabled: bool,
        rt_objects: &mut Vec<manifold_gpu::raytrace::RtObjectGeometry<'ctx>>,
    ) {
        // Resolve resident topology before authoring RT consumer flags or
        // selecting raster fallback. Reuse this exact object list for AS work.
        rt_objects.clear();
        rt_objects.extend(draws.iter()
                .filter(|d| rt_enabled && !d.routes_to_transparent() && d.instance_count > 0)
                .map(|d| {
                    let rt_instances_wired = d.instances.is_some();
                    manifold_gpu::raytrace::RtObjectGeometry {
                    vertex_buffer: d.vertices,
                    vertex_stride: std::mem::size_of::<MeshVertex>() as u32,
                    vertex_offset: 0,
                    index_buffer: d.indices,
                    triangle_count: d.vertex_count / 3,
                    transform: d.uniforms.model,
                    instance_inner: d.uniforms.instance_inner,
                    // RT-T1-B: `MeshVertex`'s normal field offset (position
                    // 12 bytes incl. pad + this) — see `mesh_common.rs`'s
                    // `MeshVertex` layout.
                    normal_offset: 16,
                    // RT-T2-A (RAYTRACING_DESIGN.md section 8.2 Tier-2 item 4):
                    // `MeshVertex`'s UV field offset (position 16 + normal
                    // 16 = 32).
                    uv_offset: 32,
                    alpha_mask: d.alpha_mode == AlphaMode::Mask,
                    // Translucency is material state: visibility queries
                    // override opacity, while BLAS opacity follows alpha mask.
                    translucent: d.uniforms.diffuse_transmission_params[0] > 0.0,
                    alpha_cutoff: d.uniforms.alpha_params[1],
                    base_color_texture: d.base_color_map,
                    // Textured roughness (R3) (RAYTRACING_DESIGN.md section 9.6): same
                    // "None = unwired, flat factor fallback" shape as
                    // `base_color_texture` above.
                    mr_texture: d.mr_map,
                    // BUG-wytp (rt-reflections-are-normal-map-blind): the normal map
                    // reaches the RT kernel exactly the way the MR map does —
                    // "None = unwired, vertex normal stands" shape. The kernel
                    // samples it at the primary hit to perturb the reflection
                    // lobe's R and the AO/GI hemisphere normal.
                    normal_texture: d.normal_map,
                    // BUG-1gqt: the emissive map + its KHR_texture_transform
                    // fold reach the trace kernels (factor × sample at the
                    // hit), mirroring the raster's `resolve_emissive`.
                    emissive_texture: d.emissive_map,
                    emissive_uv_m: d.uniforms.emissive_uv_m,
                    emissive_uv_t: [d.uniforms.emissive_uv_t[0], d.uniforms.emissive_uv_t[1]],
                    cast_shadows: d.cast_shadows,
                    // RT_INSTANCING_DESIGN.md D1/D7 (P1): wire the
                    // instance binding from `d.instances` /
                    // `d.instance_count` — the buffer's GPU address plus the
                    // live slot count. The count is part of the RT topology
                    // key below, so changing it rebuilds the descriptor
                    // slots and keeps tracing consistent with raster/shadow
                    // draws; content changes ride the accel key via
                    // `instances_content` (D9).
                    // Zero live-count objects are filtered above, so the RT
                    // descriptor kernel never receives a zero-slot buffer.
                    instances_addr: if rt_instances_wired {
                        d.instances.map_or(0, |b| b.gpu_address())
                    } else {
                        0
                    },
                    instances_buffer: if rt_instances_wired { d.instances } else { None },
                    instance_slots: if rt_instances_wired { d.instance_count } else { 1 },
                    // SCENE_MODIFIER_RT_DESIGN.md §5.2 (P4b): appearance
                    // reaches RT — per-vertex weights (bindless, pinned in
                    // the accel's geometry set) + the per-object gain. A
                    // wired buffer's float count was checked against the
                    // mesh vertex count in Pass 1 (and again, structured,
                    // at plan/encode). The descriptor-nonopaque property
                    // (weights wired || gain != 1.0, via
                    // `blas_geometry_nonopaque`) rides the topology check,
                    // so a change in it rebuilds the affected BLAS.
                    appearance_weights: d.weights,
                    appearance_gain: d.gain,
                    base_color_uv_transform: [
                        d.uniforms.base_color_uv_m[0], d.uniforms.base_color_uv_m[1],
                        d.uniforms.base_color_uv_m[2], d.uniforms.base_color_uv_m[3],
                        d.uniforms.base_color_uv_t[0], d.uniforms.base_color_uv_t[1],
                    ],
                    mr_uv_transform: [
                        d.uniforms.mr_uv_m[0], d.uniforms.mr_uv_m[1],
                        d.uniforms.mr_uv_m[2], d.uniforms.mr_uv_m[3],
                        d.uniforms.mr_uv_t[0], d.uniforms.mr_uv_t[1],
                    ],
                    normal_uv_transform: [
                        d.uniforms.normal_uv_m[0], d.uniforms.normal_uv_m[1],
                        d.uniforms.normal_uv_m[2], d.uniforms.normal_uv_m[3],
                        d.uniforms.normal_uv_t[0], d.uniforms.normal_uv_t[1],
                    ],
                    normal_scale: d.uniforms.normal_uv_t[2],
                    base_color_alpha: d.uniforms.base_color[3],
                    tangent_offset: 48,
                    extra_material_textures: [d.anisotropy_map, d.specular_map, d.specular_color_map, d.clearcoat_map, d.clearcoat_roughness_map, d.sheen_color_map, d.sheen_roughness_map, d.transmission_map, d.volume_thickness_map, d.clearcoat_normal_map, d.iridescence_map, d.iridescence_thickness_map, d.diffuse_transmission_map, d.diffuse_transmission_color_map, d.occlusion_map],
                    material_attributes: manifold_gpu::raytrace::RtMaterialAttributes {
                        uv1_offset: std::mem::offset_of!(MeshVertex, _pad2) as u32,
                        color_offset: std::mem::offset_of!(MeshVertex, color) as u32,
                        sampling: std::array::from_fn(|i| {
                            let m = d.map_uniforms[[0, 1, 2, 3, 4, 9, 13, 14, 5, 6, 7, 8, 10, 11, 12, 15, 16, 17, 18, 3][i]];
                            [m.offset_set[2] as u32, m.sampling[0], m.sampling[1], m.sampling[2] & 1]
                        }),
                        extension_uv_transforms: [9, 13, 14, 5, 6, 7, 8, 10, 11, 12, 15, 16, 17, 18, 3].map(|i| {
                            let m = d.map_uniforms[i];
                            [m.matrix[0], m.matrix[1], m.matrix[2], m.matrix[3], m.offset_set[0], m.offset_set[1]]
                        }),
                    },
                }
                })
                );
    }

    pub(super) fn author_rt_flags<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        rt_ready: &bool,
        draws: &mut [ObjectDraw<'ctx>],
        has_transmission: bool,
    ) -> (bool, bool) {
        let FramePrelude {
            ref casters, rt_enabled, rt_reflections, rt_shadows_enabled,
            rt_ao_enabled, rt_gi_enabled, ..
        } = *pre;
        let will_rt_accumulate_this_frame = rt_enabled && *rt_ready;
        let rt_just_resumed = will_rt_accumulate_this_frame && !self.rt_prev_accumulating;
        self.rt_prev_accumulating = will_rt_accumulate_this_frame;
        for draw in draws.iter_mut() {
            // Screen-space RT results belong to the opaque depth surface.
            // A transparent fragment at the same pixel is a different surface:
            // reading that lighting projects background edges and denoising
            // tiles onto glass. It uses environment lighting and raster shadows.
            let surface_rt = rt_enabled && *rt_ready && !draw.routes_to_transparent();
            let uniforms = &mut draw.uniforms;
            // RAYTRACING_DESIGN.md RT-D3 (P1-part-2): `scene_params.w` was
            // a permanently-zero reserved slot (see the field's doc
            // comment) — repurposed as the RT-active flag `shadow_factor`,
            // `rt_or_flat_ambient` (GI/AO), and the reflection substitution
            // all branch on, same reuse doctrine as `alpha_params.zw`
            // (clearcoat) and `pbr_metallic_roughness.zw`
            // (ior/specular_factor). BUG-17r3: no longer gated on
            // `!casters.is_empty()` — a zero-light emissive-only scene
            // still needs RT GI/AO/reflections; `shadow_factor`'s own
            // caster-slot lookup already no-ops when the light loop has no
            // caster slot to hand it (`slot_f < 0.0` returns fully lit),
            // so this flag is safe to raise with zero casters too.
            uniforms.scene_params[3] = if surface_rt { 1.0 } else { 0.0 };
            // RAYTRACING_DESIGN.md section 9 RD9/RD1: the reflection-substitution
            // gate — stricter than scene_params.w: the raster may only
            // read `rt_reflection` (binding 43) when the trace dispatch
            // actually ran WITH refl_spp > 0 this frame, i.e. the
            // rt_reflections param is also on. Same per-object write
            // (scene-wide value, like scene_params.w). BUG-17r3: reflections
            // trace against the scene geometry, not toward a light — never
            // caster-gated.
            uniforms.rt_flags[0] = if rt_reflections && surface_rt { 1.0 } else { 0.0 };
            // RAYTRACING_DESIGN.md section 14 ED6: rt_flags.y = the traced-
            // diffuse substitution gate — the raster may only read the RT
            // irradiance texture's `.rgb` for `diffuse_ibl` when the GI
            // gather actually ran this frame. BUG-majv: that condition is
            // `gi_spp > 0` on the trace dispatch, which since the per-term
            // toggles means `rt_gi_enabled` too — this gate used to be
            // `will_rt_accumulate_this_frame` alone, so RT-on + GI-off read
            // an `.rgb` channel the kernel never wrote (stale or
            // reset-to-zero across an off->on cycle: black diffuse with
            // sparse residue). Mirrors the reflection fallback discipline
            // (`rt_refl.a < 0` keeping the raster prefiltered fetch). No new
            // scene param (MB4).
            uniforms.rt_flags[1] = if rt_gi_enabled && surface_rt { 1.0 } else { 0.0 };
            // RAYTRACING_DESIGN.md section 16 TL5: rt_flags.z = designated
            // sun caster slot + 1 (0 = none). fs_pbr reads this to know which
            // light substitutes rt_sun_tint for the luma vis channel.
            // BUG-majv: gate on rt_shadows_enabled, not bare rt_ready — the
            // svt texture is only written by the mask/lighting dispatches at
            // shadow_spp > 0. The previous completion-latched readiness
            // could read a stale rt_sun_tint with RT
            // off or with the shadow kernel disabled — a zeroed texture
            // zeroed the sun's entire direct contribution.
            uniforms.rt_flags[2] = if rt_shadows_enabled && surface_rt { rt_svt_slot(casters).map(|s| s as f32 + 1.0).unwrap_or(0.0) } else { 0.0 };
            // RT term toggles: rt_flags.w = RT shadow mask read gate. When
            // rt_shadows is off, shadow_factor falls through to raster shadow
            // maps. The kernel still dispatches (for AO/GI/refl), but the sv
            // textures are not written (shadow_spp=0 gated in-kernel) and the
            // WGSL never reads them (gated here).
            uniforms.rt_flags[3] = if rt_shadows_enabled && surface_rt { 1.0 } else { 0.0 };
            // RAYTRACING_DESIGN.md section 12 AM2/AM6: `fog_params.z` was a
            // permanently-zero reserved slot — repurposed as `ao_mask_owed`,
            // the value the EMIT_AO_MASK fragment variants write to the
            // ao_mask attachment. 0 for unlit-kind materials (baked_look)
            // and scene-wide whenever RT is providing AO (RT on + RT AO on);
            // 1 for every lit raster pixel and whenever RT AO is toggled off
            // (so the downstream GTAO masked_mix darkens with screen-space
            // occlusion instead of reading the unwritten RT AO channel).
            // Same reserved-slot reuse doctrine as `scene_params.w` above.
            // Written unconditionally — non-mask pipelines never read it.
            uniforms.fog_params[2] =
                if draw.kind == MaterialKind::Unlit || (surface_rt && rt_ao_enabled)
                {
                    0.0
                } else {
                    1.0
                };
            // BUG-majv: `fog_params.w` (was permanently-zero reserved) = the
            // RT AO read gate for `rt_or_flat_ambient`. The irradiance
            // texture's `.a` is only written when the trace dispatch ran
            // with ao_spp > 0 (or gi_spp > 0, which writes a neutral 1.0);
            // gating on scene_params.w alone read a stale/zeroed `.a` with
            // the AO kernel off, killing the ambient term after an
            // off->on cycle. AO off also restores the raster's
            // full-strength flat ambient (the 0.15 RT ceiling only applies
            // when RT AO is actually providing the occlusion term).
            uniforms.fog_params[3] = if rt_ao_enabled && surface_rt { 1.0 } else { 0.0 };
        }

        // RAYTRACING_DESIGN.md section 12 AM1: when `has_transmission`
        // routes the Blend group to Pass B (single color attachment, no
        // MSAA, no aux MRT — see the E2a seam below), those draws must carry
        // a pipeline matching that pass: one color attachment and 1x
        // samples. Metal requires the pipeline's attachment layout and
        // sample count to match the pass; a 4x pipeline there is undefined
        // without the debug layer (BUG-hkbdp.6.16, multibounce GPU hang
        // flake). Swap here — the first point `has_transmission` is fully
        // known.
        if has_transmission {
            let gpu = ctx.gpu_encoder();
            for draw in draws
                .iter_mut()
                .filter(|d| d.routes_to_transparent() && !d.points)
            {
                draw.pipeline = self.pass_b_pipeline_for(gpu.device, draw.kind).clone();
            }
        }

        (rt_just_resumed, will_rt_accumulate_this_frame)
    }
}
