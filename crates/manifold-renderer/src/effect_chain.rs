use manifold_core::EffectTypeId;
use manifold_core::effects::{EffectGroup, EffectInstance};
use crate::effect::{EffectContext, find_chain_param};
use crate::effect_registry::EffectRegistry;
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use crate::wet_dry_lerp::WetDryLerpPipeline;

/// Dispatches a chain of effects through the registry, handling group wet/dry.
///
/// Owns its own ping-pong buffers (lazy) for processing. The first effect in
/// each chain invocation reads directly from the external input view (no copy),
/// eliminating a full render pass per chain invocation (~629μs at 4K).
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
    fn ensure_buffers(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let format = wgpu::TextureFormat::Rgba16Float;
        if self.ping.is_none() {
            self.ping = Some(RenderTarget::new(device, width, height, format, "EffectChain Ping"));
            self.pong = Some(RenderTarget::new(device, width, height, format, "EffectChain Pong"));
        }
    }

    fn ensure_dry_snapshot(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let format = wgpu::TextureFormat::Rgba16Float;
        if self.dry_snapshot.is_none() {
            self.dry_snapshot = Some(RenderTarget::new(device, width, height, format, "EffectChain DrySnapshot"));
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

    fn source_view(&self) -> &wgpu::TextureView {
        &self.source().view
    }

    /// The raw texture backing the current source buffer.
    /// Used by the compositor for copy_texture_to_texture after master effects.
    pub fn source_texture(&self) -> &wgpu::Texture {
        &self.source().texture
    }

    fn target_view(&self) -> &wgpu::TextureView {
        &self.target().view
    }

    fn swap(&mut self) {
        self.use_ping_as_source = !self.use_ping_as_source;
    }

    /// Apply a chain of effects. Returns the texture view with the final result.
    ///
    /// If the chain is empty or has no enabled effects, returns `None` (caller
    /// should use the original input).
    ///
    /// `mid_chain_tap`: Optional callback index for external output (LED walls).
    /// Unity ref: CompositorStack.cs lines 864-865, 918-920
    #[allow(clippy::too_many_arguments)]
    pub fn apply_chain(
        &mut self,
        gpu: &mut GpuEncoder,
        registry: &mut EffectRegistry,
        input_view: &wgpu::TextureView,
        input_texture: &wgpu::Texture,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
        wet_dry_lerp: Option<&WetDryLerpPipeline>,
        gpu_profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> Option<&wgpu::TextureView> {
        // Filter to enabled effects with registered processors
        let enabled: Vec<usize> = effects
            .iter()
            .enumerate()
            .filter(|(_, fx)| {
                if !fx.enabled { return false; }
                if registry.get_mut(fx.effect_type()).is_none() {
                    log::debug!("Effect {:?} has no GPU processor — skipped", fx.effect_type());
                    return false;
                }
                true
            })
            .map(|(i, _)| i)
            .collect();

        if enabled.is_empty() {
            return None;
        }

        self.ensure_buffers(gpu.device, ctx.width, ctx.height);
        self.use_ping_as_source = true;

        // Precompute cross-chain params for effects that need them.
        // Unity ref: EffectContext.FindChainParam() — VoronoiPrism reads EdgeStretch width.
        let chain_ctx = EffectContext {
            edge_stretch_width: find_chain_param(
                effects, &EffectTypeId::EDGE_STRETCH, 1, 0.5625,
            ),
            ..*ctx
        };

        // Set profiler scope based on owner context
        if let Some(profiler) = gpu_profiler {
            let scope = if chain_ctx.owner_key == 0 {
                "master:".to_string()
            } else if chain_ctx.is_clip_level {
                format!("clip:{}:", chain_ctx.owner_key)
            } else {
                format!("layer:{}:", chain_ctx.owner_key - 1)
            };
            profiler.set_scope(&scope);
        }

        // Performance: skip the internal blit that copies input → ping buffer.
        // The first effect reads directly from input_view, writing to the
        // chain's target buffer. Subsequent effects use normal ping-pong.
        // Saves ~629μs per chain invocation at 4K (one fewer render pass).
        let mut first_effect_pending = true;

        let mut current_group_id: Option<&str> = None;

        for &idx in &enabled {
            let fx = &effects[idx];

            // Track group transitions for wet/dry
            let fx_group_id = fx.group_id.as_deref();
            if fx_group_id != current_group_id {
                // Leaving a group — apply wet/dry lerp if needed
                if let Some(prev_gid) = current_group_id
                    && let Some(group) = groups.iter().find(|g| g.id == prev_gid) {
                        self.apply_wet_dry_lerp(
                            gpu, group.wet_dry, wet_dry_lerp, gpu_profiler,
                        );
                    }

                // Entering a new group — snapshot dry state if wet_dry < 1.0
                if let Some(gid) = fx_group_id
                    && let Some(group) = groups.iter().find(|g| g.id == gid) {
                        if !group.enabled {
                            current_group_id = Some(gid);
                            continue;
                        }
                        if group.wet_dry < 1.0 {
                            // If no effect has run yet, copy input → source via
                            // GPU memcpy so the dry snapshot captures the input.
                            if first_effect_pending {
                                copy_tex_to_rt(
                                    gpu.encoder, input_texture, self.source(),
                                );
                                first_effect_pending = false;
                            }
                            self.ensure_dry_snapshot(gpu.device, ctx.width, ctx.height);
                            // GPU copy source → dry_snapshot
                            copy_rt_to_rt(
                                gpu.encoder,
                                self.source(),
                                self.dry_snapshot.as_ref().unwrap(),
                            );
                        }
                    }

                current_group_id = fx_group_id;
            }

            // Check if group is disabled — skip effect
            if let Some(gid) = fx_group_id
                && let Some(group) = groups.iter().find(|g| g.id == gid)
                    && !group.enabled {
                        continue;
                    }

            // Apply the effect (skip if ShouldSkip — no GPU work, no swap)
            // Unity ref: CompositorStack checks ShouldSkip before Apply + buffer swap.
            //
            // When hal encoding is active, skip effects that don't support hal —
            // their render passes would go to the dummy wgpu encoder (never submitted),
            // corrupting the effect chain output.
            if let Some(processor) = registry.get_mut(fx.effect_type())
                && !processor.should_skip(fx)
                && {
                    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
                    { !gpu.has_hal_encoder() || processor.supports_hal() }
                    #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
                    { true }
                } {
                    // First effect reads directly from input_view (no copy).
                    let source_v = if first_effect_pending {
                        input_view
                    } else {
                        self.source_view()
                    };
                    processor.apply(
                        gpu,
                        source_v,
                        self.target_view(),
                        &self.target().texture,
                        fx, &chain_ctx,
                        gpu_profiler,
                    );
                    self.swap();
                    first_effect_pending = false;
                }
        }

        // Final group exit — apply wet/dry if needed
        if let Some(prev_gid) = current_group_id
            && let Some(group) = groups.iter().find(|g| g.id == prev_gid) {
                self.apply_wet_dry_lerp(
                    gpu, group.wet_dry, wet_dry_lerp, gpu_profiler,
                );
            }

        // Clear profiler scope
        if let Some(profiler) = gpu_profiler {
            profiler.clear_scope();
        }

        // If no effect actually ran (all were ShouldSkip), return None so the
        // caller uses the original input view — no blit was needed at all.
        if first_effect_pending {
            return None;
        }

        Some(self.source_view())
    }

    /// Apply wet/dry lerp if wet_dry < 1.0 and dry snapshot exists.
    fn apply_wet_dry_lerp(
        &mut self,
        gpu: &mut GpuEncoder,
        wet_dry: f32,
        lerp_pipeline: Option<&WetDryLerpPipeline>,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
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

        // Lerp: dry_snapshot (dry) + source (wet) → target
        let target = self.target();
        lerp.apply(
            gpu,
            &dry_snap.view,
            self.source_view(),
            &target.view,
            target.width,
            target.height,
            wet_dry,
            profiler,
        );
        self.swap();
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
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

/// GPU-side texture copy from a raw texture into a RenderTarget.
fn copy_tex_to_rt(
    encoder: &mut wgpu::CommandEncoder,
    source: &wgpu::Texture,
    target: &RenderTarget,
) {
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &target.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: target.width,
            height: target.height,
            depth_or_array_layers: 1,
        },
    );
}

/// GPU-side texture copy between two RenderTargets using wgpu's copy command.
fn copy_rt_to_rt(
    encoder: &mut wgpu::CommandEncoder,
    source: &RenderTarget,
    target: &RenderTarget,
) {
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &source.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &target.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: source.width.min(target.width),
            height: source.height.min(target.height),
            depth_or_array_layers: 1,
        },
    );
}
