//! Water fields in the scene: collecting the objects that carry a level set,
//! and the material, model and previous-model steps they share with mesh
//! objects.

use super::*;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::scene::scene_object::FieldSurface;
use manifold_node_engine::scene::world_tiles::{pool_len, table_words};

/// One water field to march and shade this frame. Every array is a pooled
/// field of `pool_len(pool_tiles)` elements under the world-tile table in
/// `tiles`; the field has no CPU box, its extent lives on the GPU.
pub(super) struct FieldObject<'ctx> {
    /// The object index, which also names the field in the G-buffer.
    pub slot: u32,
    pub levelset: &'ctx manifold_gpu::GpuBuffer,
    pub velocity: Option<&'ctx manifold_gpu::GpuBuffer>,
    /// Foam coverage per node, 0 to 1.
    pub foam: Option<&'ctx manifold_gpu::GpuBuffer>,
    /// Mist extinction per node, per metre.
    pub mist: Option<&'ctx manifold_gpu::GpuBuffer>,
    /// Wetness per node, 0 to 1.
    pub wetness: Option<&'ctx manifold_gpu::GpuBuffer>,
    /// The world-tile table (`world_tiles` layout) the arrays are read through.
    pub tiles: &'ctx manifold_gpu::GpuBuffer,
    /// Eight floats: the held tiles' node bounds in object space.
    pub extent: &'ctx manifold_gpu::GpuBuffer,
    pub pool_tiles: u32,
    /// The display lattice's cell size in metres.
    pub cell_size: f32,
    pub model: [[f32; 4]; 4],
    pub prev_model: [[f32; 4]; 4],
    /// Ripple detail strength over the automatic one (`FieldSurface::ripple_detail`).
    pub ripple_detail: f32,
    pub uniforms: RenderSceneUniforms,
}

/// The material an object draws with this frame.
#[allow(
    clippy::large_enum_variant,
    reason = "returned once per object per frame and matched at once; boxing would allocate every frame"
)]
pub(super) enum ObjectMaterial {
    Ready(Material),
    /// An input is still streaming in; the object drops out of this frame.
    Late,
    /// The error is reported and the frame is cleared to magenta.
    Failed,
}

impl RenderScene {
    /// The object's material after the render-mode substitution. Wireframe
    /// and points get the unlit line surface, solid the clay material,
    /// rendered passes the object's own through untouched.
    pub(super) fn object_material(
        ctx: &mut EffectNodeContext<'_, '_>,
        n: usize,
        material: Option<Material>,
        render_mode: &manifold_node_engine::scene::render_mode::RenderMode,
        envmap_wired: bool,
        envmap_pending: bool,
    ) -> ObjectMaterial {
        let Some(material) = material else {
            ctx.gpu_encoder()
                .merge_frame_status(FrameRenderStatus::Failed(
                    FrameRenderFailure::InvalidGeometry,
                ));
            Self::fail_frame(
                ctx,
                Some(n as u32),
                format!(
                    "object_{n}: missing required `material` input (its scene_object's `material` port is unwired); renderer fell back to magenta clear"
                ),
            );
            return ObjectMaterial::Failed;
        };
        // SCENE_RENDER_MODE_DESIGN.md D6/D7/D8: ONE match on the mode
        // substitutes the object's material at the gather site, upstream of
        // `pipeline_for`, so the ordinary per-kind pipeline cache picks the
        // substitute's shader.
        let material = match render_mode.mode {
            manifold_node_engine::scene::render_mode::RENDER_MODE_WIREFRAME => {
                wireframe_material(render_mode)
            }
            manifold_node_engine::scene::render_mode::RENDER_MODE_SOLID
                if envmap_wired || envmap_pending =>
            {
                clay_material(render_mode)
            }
            manifold_node_engine::scene::render_mode::RENDER_MODE_SOLID => {
                clay_material_without_envmap(render_mode)
            }
            manifold_node_engine::scene::render_mode::RENDER_MODE_POINTS => {
                wireframe_material(render_mode)
            }
            _ => material,
        };
        if material.requires_envmap() && envmap_pending {
            return ObjectMaterial::Late;
        }
        if material.requires_envmap() && !envmap_wired {
            ctx.gpu_encoder()
                .merge_frame_status(FrameRenderStatus::Failed(
                    FrameRenderFailure::InvalidGeometry,
                ));
            Self::fail_frame(
                ctx,
                Some(n as u32),
                format!(
                    "{:?} material on `object_{n}` requires `envmap` input but it is unwired; renderer fell back to magenta",
                    material.kind
                ),
            );
            return ObjectMaterial::Failed;
        }
        ObjectMaterial::Ready(material)
    }

    /// The object's model matrix and, for instanced objects, the inner
    /// matrix the instances compose with.
    pub(super) fn object_model(
        object: &SceneObject,
        cam_pos: [f32; 3],
    ) -> ([[f32; 4]; 4], [[f32; 4]; 4]) {
        // `object.transform` already defaults to `Transform::default()`
        // (identity) when scene_object's own `transform` input is
        // unwired (D2) — matches the old scattered params' defaults
        // exactly (pos 0, rot 0, scale 1).
        let t = object.transform;
        let rot_euler = if t.billboard {
            t.billboard_rot_euler(cam_pos)
        } else {
            t.rot_euler
        };
        // In Group space the part's place inside one copy is its whole
        // chain: the groups between it and the copied group.
        let in_copy = matches!(
            object.instances_space,
            manifold_node_engine::scene::scene_object::InstanceSpace::Group
                | manifold_node_engine::scene::scene_object::InstanceSpace::World
        ) && object.instances.is_some();
        let local_model = if in_copy {
            t.world_with_rotation(rot_euler)
        } else {
            model_matrix(t.pos, rot_euler, t.scale)
        };
        let parent_model = object.parent_transform.map(|parent| {
            let parent_rot = if parent.billboard {
                parent.billboard_rot_euler(cam_pos)
            } else {
                parent.rot_euler
            };
            parent.world_with_rotation(parent_rot)
        });
        instance_space_pair(
            parent_model,
            local_model,
            object.instances_space,
            object.instances.is_some(),
        )
    }

    /// Last frame's `[model, instance_inner]` for object `n`, then store this
    /// frame's. `None` at this slot (no history yet — a brand-new node, or
    /// the slot right after a rebuild) seeds prev = current, giving THIS
    /// object exactly-zero first-frame velocity. Stored immediately after
    /// reading (not gated on `velocity_wired`) — see the `prev_view_proj`
    /// comment for why continuous tracking is the correct default.
    pub(super) fn advance_prev_model(
        &mut self,
        n: usize,
        model: [[f32; 4]; 4],
        instance_inner: [[f32; 4]; 4],
    ) -> [[[f32; 4]; 4]; 2] {
        let prev = self
            .prev_model
            .get(n)
            .copied()
            .flatten()
            .unwrap_or([model, instance_inner]);
        if let Some(slot) = self.prev_model.get_mut(n) {
            *slot = Some([model, instance_inner]);
        }
        prev
    }

    /// Report an invalid field and abort the frame.
    fn fail_field<T>(ctx: &mut EffectNodeContext<'_, '_>, n: usize, message: String) -> Option<T> {
        ctx.gpu_encoder()
            .merge_frame_status(FrameRenderStatus::Failed(
                FrameRenderFailure::InvalidGeometry,
            ));
        Self::fail_frame(ctx, Some(n as u32), message);
        None
    }

    /// The buffer behind a field port, holding at least `needed` 4-byte
    /// values. `Err` after the frame failure is reported.
    fn field_array<'ctx>(
        ctx: &mut EffectNodeContext<'ctx, '_>,
        n: usize,
        slot: Slot,
        port: &str,
        needed: u64,
    ) -> Result<&'ctx manifold_gpu::GpuBuffer, ()> {
        let Some(buffer) = ctx.inputs.array_slot(slot) else {
            Self::fail_field::<()>(
                ctx,
                n,
                format!("object_{n}: water field `{port}` input is unavailable"),
            );
            return Err(());
        };
        let have = buffer.size / 4;
        if have < needed {
            Self::fail_field::<()>(
                ctx,
                n,
                format!(
                    "object_{n}: water field {} holds {have} values, its pool needs {needed}",
                    port.trim_start_matches("field_"),
                ),
            );
            return Err(());
        }
        Ok(buffer)
    }

    /// [`Self::field_array`] for a port that may be unwired.
    fn optional_field_array<'ctx>(
        ctx: &mut EffectNodeContext<'ctx, '_>,
        n: usize,
        slot: Option<Slot>,
        port: &str,
        needed: u64,
    ) -> Result<Option<&'ctx manifold_gpu::GpuBuffer>, ()> {
        slot.map(|slot| Self::field_array(ctx, n, slot, port, needed))
            .transpose()
    }

    /// Validate one water object and compose its model and uniforms.
    /// `Some(None)` = an input is still streaming in (the object drops out
    /// of this frame); `None` = abort the frame.
    pub(super) fn collect_field<'ctx>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, '_>,
        pre: &FramePrelude<'ctx>,
        n: usize,
        object: &SceneObject,
        field: FieldSurface,
    ) -> Option<Option<FieldObject<'ctx>>> {
        let FramePrelude {
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
            rt_enabled,
            temporal_upscale,
            width,
            native_width,
            ..
        } = pre;
        let render_mode = effective_render_mode(render_mode, *rt_enabled);
        let inputs = ctx.inputs;
        if [field.levelset, field.tiles, field.extent]
            .into_iter()
            .chain(
                [field.velocity, field.foam, field.mist, field.wetness]
                    .into_iter()
                    .flatten(),
            )
            .any(|slot| !inputs.slot_content_ready(slot))
        {
            return Some(None);
        }
        let pool_tiles = field.pool_tiles;
        let elements = pool_len(pool_tiles);
        let levelset = Self::field_array(ctx, n, field.levelset, "field", elements).ok()?;
        let velocity =
            Self::optional_field_array(ctx, n, field.velocity, "field_velocity", 3 * elements)
                .ok()?;
        let foam = Self::optional_field_array(ctx, n, field.foam, "field_foam", elements).ok()?;
        let mist = Self::optional_field_array(ctx, n, field.mist, "field_mist", elements).ok()?;
        let wetness =
            Self::optional_field_array(ctx, n, field.wetness, "field_wetness", elements).ok()?;
        let tiles =
            Self::field_array(ctx, n, field.tiles, "field_tiles", table_words(pool_tiles)).ok()?;
        let extent = Self::field_array(ctx, n, field.extent, "field_extent", 8).ok()?;
        let material = match Self::object_material(
            ctx,
            n,
            object.material,
            &render_mode,
            envmap_wired.is_some(),
            *envmap_pending,
        ) {
            ObjectMaterial::Ready(material) => material,
            ObjectMaterial::Late => return Some(None),
            ObjectMaterial::Failed => return None,
        };
        let (model, instance_inner) = Self::object_model(object, cam.pos);
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
            false,
            render_mode.point_size,
        );
        uniforms.instance_inner = instance_inner;
        uniforms.appearance[3] = *env_rotation;
        uniforms.prev_instance_inner = prev_inner_n;
        uniforms.velocity_jitter = [
            jitter_ndc.0,
            jitter_ndc.1,
            prev_jitter_ndc.0,
            prev_jitter_ndc.1,
        ];
        if *temporal_upscale {
            uniforms.render_mode[1] = (*width as f32 / (*native_width).max(1) as f32).log2();
        }
        Some(Some(FieldObject {
            slot: n as u32,
            levelset,
            velocity,
            foam,
            mist,
            wetness,
            tiles,
            extent,
            pool_tiles,
            cell_size: field.cell_size,
            model,
            prev_model: prev_model_n,
            ripple_detail: field.ripple_detail,
            uniforms,
        }))
    }
}
