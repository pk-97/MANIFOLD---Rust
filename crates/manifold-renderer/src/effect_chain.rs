use manifold_core::effects::{EffectGroup, EffectInstance};
use crate::effect::EffectContext;
use crate::effect_registry::EffectRegistry;
use crate::render_target::RenderTarget;

/// Dispatches a chain of effects through the registry, handling group wet/dry.
///
/// Owns its own ping-pong buffers (lazy) for processing. Takes source/target
/// explicitly — no mutable self-referencing.
pub struct EffectChain {
    ping: Option<RenderTarget>,
    pong: Option<RenderTarget>,
    /// Snapshot of dry state before entering a group with wet_dry < 1.0.
    dry_snapshot: Option<RenderTarget>,
    use_ping_as_source: bool,
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

    fn source_view(&self) -> &wgpu::TextureView {
        if self.use_ping_as_source {
            &self.ping.as_ref().unwrap().view
        } else {
            &self.pong.as_ref().unwrap().view
        }
    }

    fn target_view(&self) -> &wgpu::TextureView {
        if self.use_ping_as_source {
            &self.pong.as_ref().unwrap().view
        } else {
            &self.ping.as_ref().unwrap().view
        }
    }

    fn swap(&mut self) {
        self.use_ping_as_source = !self.use_ping_as_source;
    }

    /// Apply a chain of effects. Returns the texture view with the final result.
    ///
    /// The caller must copy/blit `input_view` into our internal source before
    /// calling this, or pass `input_view` as the first source. The chain
    /// processes effects in order, swapping ping/pong after each.
    ///
    /// If the chain is empty or has no enabled effects, returns `None` (caller
    /// should use the original input).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_chain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        registry: &mut EffectRegistry,
        input_view: &wgpu::TextureView,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
    ) -> Option<&wgpu::TextureView> {
        // Filter to enabled effects with registered processors
        let enabled: Vec<usize> = effects
            .iter()
            .enumerate()
            .filter(|(_, fx)| fx.enabled && registry.get_mut(fx.effect_type).is_some())
            .map(|(i, _)| i)
            .collect();

        if enabled.is_empty() {
            return None;
        }

        self.ensure_buffers(device, ctx.width, ctx.height);
        self.use_ping_as_source = true;

        // Copy input into our source buffer
        copy_texture_to_texture(encoder, input_view, self.source_view(), ctx.width, ctx.height);

        let mut current_group_id: Option<&str> = None;

        for &idx in &enabled {
            let fx = &effects[idx];

            // Track group transitions for wet/dry
            let fx_group_id = fx.group_id.as_deref();
            if fx_group_id != current_group_id {
                // Leaving a group — apply wet/dry lerp if needed
                if let Some(prev_gid) = current_group_id {
                    if let Some(group) = groups.iter().find(|g| g.id == prev_gid) {
                        if group.wet_dry < 1.0 && self.dry_snapshot.is_some() {
                            // TODO: wet/dry lerp pass (Phase C — needs wet_dry_lerp.wgsl)
                            // For now, skip lerp — full wet
                        }
                    }
                }

                // Entering a new group — snapshot dry state if wet_dry < 1.0
                if let Some(gid) = fx_group_id {
                    if let Some(group) = groups.iter().find(|g| g.id == gid) {
                        if !group.enabled {
                            // Skip all effects in this disabled group
                            current_group_id = Some(gid);
                            continue;
                        }
                        if group.wet_dry < 1.0 {
                            self.ensure_dry_snapshot(device, ctx.width, ctx.height);
                            copy_texture_to_texture(
                                encoder,
                                self.source_view(),
                                &self.dry_snapshot.as_ref().unwrap().view,
                                ctx.width, ctx.height,
                            );
                        }
                    }
                }

                current_group_id = fx_group_id;
            }

            // Check if group is disabled — skip effect
            if let Some(gid) = fx_group_id {
                if let Some(group) = groups.iter().find(|g| g.id == gid) {
                    if !group.enabled {
                        continue;
                    }
                }
            }

            // Apply the effect
            if let Some(processor) = registry.get_mut(fx.effect_type) {
                processor.apply(
                    device, queue, encoder,
                    self.source_view(),
                    self.target_view(),
                    fx, ctx,
                );
                self.swap();
            }
        }

        // Final group exit — apply wet/dry if needed
        if let Some(prev_gid) = current_group_id {
            if let Some(group) = groups.iter().find(|g| g.id == prev_gid) {
                if group.wet_dry < 1.0 && self.dry_snapshot.is_some() {
                    // TODO: wet/dry lerp pass
                }
            }
        }

        Some(self.source_view())
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

/// Copy one texture view to another via a blit render pass.
fn copy_texture_to_texture(
    encoder: &mut wgpu::CommandEncoder,
    _source: &wgpu::TextureView,
    target: &wgpu::TextureView,
    _width: u32,
    _height: u32,
) {
    // For now, clear the target. Full texture copy requires a blit pipeline
    // which will be added when we integrate the first concrete effect.
    // The effect chain's apply_chain copies input → source, so each effect
    // reads source and writes target. This clear prevents stale data.
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("EffectChain Copy"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}
