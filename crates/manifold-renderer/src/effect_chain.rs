use crate::effect::{EffectContext, find_chain_param};
use crate::effect_registry::EffectRegistry;
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use crate::wet_dry_lerp::WetDryLerpPipeline;
use manifold_core::EffectTypeId;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};

/// Dispatches a chain of effects through the registry, handling group wet/dry.
///
/// Owns its own ping-pong buffers (lazy) for processing. The first effect in
/// each chain invocation reads directly from the external input texture (no copy),
/// eliminating a full render pass per chain invocation (~629us at 4K).
pub struct EffectChain {
    ping: Option<RenderTarget>,
    pong: Option<RenderTarget>,
    /// Snapshot of dry state before entering a group with wet_dry < 1.0.
    dry_snapshot: Option<RenderTarget>,
    use_ping_as_source: bool,
}

impl Default for EffectChain {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectChain {
    pub fn new() -> Self {
        Self {
            ping: None,
            pong: None,
            dry_snapshot: None,
            use_ping_as_source: true,
        }
    }

    /// Ensure internal ping-pong buffers exist at the given dimensions.
    fn ensure_buffers(
        &mut self,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        width: u32,
        height: u32,
    ) {
        let format = GpuTextureFormat::Rgba16Float;
        if self.ping.is_none() {
            self.ping = Some(if let Some(p) = pool {
                RenderTarget::new_pooled(p, width, height, format, "EffectChain Ping")
            } else {
                RenderTarget::new(device, width, height, format, "EffectChain Ping")
            });
            self.pong = Some(if let Some(p) = pool {
                RenderTarget::new_pooled(p, width, height, format, "EffectChain Pong")
            } else {
                RenderTarget::new(device, width, height, format, "EffectChain Pong")
            });
        }
    }

    fn ensure_dry_snapshot(
        &mut self,
        device: &GpuDevice,
        pool: Option<&manifold_gpu::TexturePool>,
        width: u32,
        height: u32,
    ) {
        let format = GpuTextureFormat::Rgba16Float;
        if self.dry_snapshot.is_none() {
            self.dry_snapshot = Some(if let Some(p) = pool {
                RenderTarget::new_pooled(p, width, height, format, "EffectChain DrySnapshot")
            } else {
                RenderTarget::new(device, width, height, format, "EffectChain DrySnapshot")
            });
        }
    }

    fn source(&self) -> &RenderTarget {
        if self.use_ping_as_source {
            self.ping.as_ref().unwrap()
        } else {
            self.pong.as_ref().unwrap()
        }
    }

    fn target(&self) -> &RenderTarget {
        if self.use_ping_as_source {
            self.pong.as_ref().unwrap()
        } else {
            self.ping.as_ref().unwrap()
        }
    }

    fn source_texture(&self) -> &GpuTexture {
        &self.source().texture
    }

    /// The texture backing the current source buffer.
    /// Used by the compositor for copy_texture_to_texture after master effects.
    pub fn source_texture_pub(&self) -> &GpuTexture {
        &self.source().texture
    }

    fn target_texture(&self) -> &GpuTexture {
        &self.target().texture
    }

    fn swap(&mut self) {
        self.use_ping_as_source = !self.use_ping_as_source;
    }

    /// Apply a chain of effects. Returns the texture with the final result.
    ///
    /// If the chain is empty or has no enabled effects, returns `None` (caller
    /// should use the original input).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_chain<'a>(
        &'a mut self,
        gpu: &mut GpuEncoder,
        registry: &mut EffectRegistry,
        input_texture: &'a GpuTexture,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
        wet_dry_lerp: Option<&WetDryLerpPipeline>,
    ) -> Option<&'a GpuTexture> {
        // Quick scan: any enabled effects with registered processors?
        let has_enabled = effects
            .iter()
            .any(|fx| fx.enabled && registry.get_mut(fx.effect_type()).is_some());
        if !has_enabled {
            return None;
        }

        self.ensure_buffers(gpu.device, gpu.pool, ctx.width, ctx.height);
        self.use_ping_as_source = true;

        // Precompute cross-chain params for effects that need them.
        // Unity ref: EffectContext.FindChainParam() — VoronoiPrism reads EdgeStretch width.
        let chain_ctx = EffectContext {
            edge_stretch_width: find_chain_param(effects, &EffectTypeId::EDGE_STRETCH, 1, 0.5625),
            ..*ctx
        };

        // Performance: skip the internal blit that copies input -> ping buffer.
        // The first effect reads directly from input_texture, writing to the
        // chain's target buffer. Subsequent effects use normal ping-pong.
        // Saves ~629us per chain invocation at 4K (one fewer render pass).
        let mut first_effect_pending = true;

        let mut current_group_id: Option<&str> = None;

        for fx in effects {
            // Skip disabled effects and effects without registered processors
            // (replaces the pre-collected `enabled` Vec).
            if !fx.enabled || registry.get_mut(fx.effect_type()).is_none() {
                continue;
            }

            // Track group transitions for wet/dry
            let fx_group_id = fx.group_id.as_deref();
            if fx_group_id != current_group_id {
                // Leaving a group — apply wet/dry lerp if needed
                if let Some(prev_gid) = current_group_id
                    && let Some(group) = groups.iter().find(|g| g.id == prev_gid)
                {
                    self.apply_wet_dry_lerp(gpu, group.wet_dry, wet_dry_lerp);
                }

                // Entering a new group — snapshot dry state if wet_dry < 1.0
                if let Some(gid) = fx_group_id
                    && let Some(group) = groups.iter().find(|g| g.id == gid)
                {
                    if !group.enabled {
                        current_group_id = Some(gid);
                        continue;
                    }
                    if group.wet_dry < 1.0 {
                        // Only snapshot if at least one effect in this group
                        // will actually execute — avoids 2 GPU texture copies
                        // when all group effects are skipped (amount == 0).
                        let group_has_work = effects.iter().any(|e| {
                            e.enabled
                                && e.group_id.as_deref() == Some(gid)
                                && registry
                                    .get_mut(e.effect_type())
                                    .is_some_and(|p| !p.should_skip(e))
                        });
                        if group_has_work {
                            // If no effect has run yet, copy input -> source via
                            // GPU memcpy so the dry snapshot captures the input.
                            if first_effect_pending {
                                gpu.copy_texture_to_texture(
                                    input_texture,
                                    self.source_texture(),
                                    ctx.width,
                                    ctx.height,
                                );
                                first_effect_pending = false;
                            }
                            self.ensure_dry_snapshot(
                                gpu.device, gpu.pool, ctx.width, ctx.height,
                            );
                            // GPU copy source -> dry_snapshot
                            gpu.copy_texture_to_texture(
                                self.source_texture(),
                                &self.dry_snapshot.as_ref().unwrap().texture,
                                ctx.width,
                                ctx.height,
                            );
                        }
                    }
                }

                current_group_id = fx_group_id;
            }

            // Check if group is disabled — skip effect
            if let Some(gid) = fx_group_id
                && let Some(group) = groups.iter().find(|g| g.id == gid)
                && !group.enabled
            {
                continue;
            }

            // Apply the effect (skip if ShouldSkip — no GPU work, no swap)
            // Unity ref: CompositorStack checks ShouldSkip before Apply + buffer swap.
            if let Some(processor) = registry.get_mut(fx.effect_type())
                && !processor.should_skip(fx)
            {
                // First effect reads directly from input_texture (no copy).
                let source = if first_effect_pending {
                    input_texture
                } else {
                    self.source_texture()
                };
                processor.apply(gpu, source, self.target_texture(), fx, &chain_ctx);
                self.swap();
                first_effect_pending = false;
            }
        }

        // Final group exit — apply wet/dry if needed
        if let Some(prev_gid) = current_group_id
            && let Some(group) = groups.iter().find(|g| g.id == prev_gid)
        {
            self.apply_wet_dry_lerp(gpu, group.wet_dry, wet_dry_lerp);
        }

        // If no effect actually ran (all were ShouldSkip), return None so the
        // caller uses the original input — no blit was needed at all.
        if first_effect_pending {
            return None;
        }

        Some(self.source_texture())
    }

    /// Apply wet/dry lerp if wet_dry < 1.0 and dry snapshot exists.
    fn apply_wet_dry_lerp(
        &mut self,
        gpu: &mut GpuEncoder,
        wet_dry: f32,
        lerp_pipeline: Option<&WetDryLerpPipeline>,
    ) {
        if wet_dry >= 1.0 {
            return;
        }
        let dry_snap = match &self.dry_snapshot {
            Some(snap) => snap,
            None => return,
        };
        let lerp = match lerp_pipeline {
            Some(l) => l,
            None => return,
        };

        // Lerp: dry_snapshot (dry) + source (wet) -> target
        let target = self.target();
        lerp.apply(
            gpu,
            &dry_snap.texture,
            self.source_texture(),
            &target.texture,
            wet_dry,
            target.width,
            target.height,
        );
        self.swap();
    }

    /// Release all owned textures back to the pool. Resets to empty state
    /// (textures will be lazily recreated on next apply_chain).
    pub fn release_to_pool(&mut self, pool: &manifold_gpu::TexturePool) {
        if let Some(ping) = self.ping.take() {
            ping.release_to_pool(pool);
        }
        if let Some(pong) = self.pong.take() {
            pong.release_to_pool(pool);
        }
        if let Some(snap) = self.dry_snapshot.take() {
            snap.release_to_pool(pool);
        }
    }

    pub fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        if let Some(ping) = &mut self.ping {
            ping.resize(device, width, height);
        }
        if let Some(pong) = &mut self.pong {
            pong.resize(device, width, height);
        }
        if let Some(snap) = &mut self.dry_snapshot {
            snap.resize(device, width, height);
        }
    }
}
