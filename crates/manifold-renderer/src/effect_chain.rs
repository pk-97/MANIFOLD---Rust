use crate::effect::{EffectContext, find_chain_param};
use crate::effect_chain_graph::{ChainGraph, GraphEffectCache};
use crate::effect_registry::EffectRegistry;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::{primitive_id_for_effect, PrimitiveRegistry};
use crate::render_target::RenderTarget;
use crate::wet_dry_lerp::WetDryLerpPipeline;
use manifold_core::EffectTypeId;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
use std::sync::OnceLock;

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
    /// One cached `Graph` for the whole chain — used as the fast
    /// path when every effect has a registered factory and no
    /// group has wet_dry < 1.0. See `ChainGraph` docstring for the
    /// full precondition list.
    chain_graph: Option<ChainGraph>,
    /// Per-effect cached graph runtime executors (the §6.6 #3-#4
    /// approach). Used when `chain_graph` declines (groups with
    /// partial wet/dry, future cases). One day, when `chain_graph`
    /// also handles groups, this whole cache can be deleted along
    /// with the legacy ping/pong/dry_snapshot plumbing.
    graph_cache: GraphEffectCache,
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
            chain_graph: None,
            graph_cache: GraphEffectCache::new(),
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

    fn swap(&mut self) {
        self.use_ping_as_source = !self.use_ping_as_source;
    }

    /// Try the chain-as-one-graph dispatch. Returns `true` if the
    /// chain ran successfully through `self.chain_graph`; `false`
    /// to signal "fall back to per-effect dispatch". The chain
    /// output is then accessible via [`Self::chain_graph_output`]
    /// — split into two calls because returning a borrowed
    /// reference from a `&mut self` method here would extend the
    /// mutable borrow through the rest of `apply_chain`.
    ///
    /// Topology changes (effect added/removed/reordered, group
    /// wet-dry crossing 1.0, render-resolution change) trigger a
    /// rebuild via `ChainGraph::try_build`. Per-frame param changes
    /// reuse the cached graph (no rebuild).
    #[allow(clippy::too_many_arguments)]
    fn try_run_chain_graph(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        input_texture: &GpuTexture,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
    ) -> bool {
        let needs_rebuild = match &self.chain_graph {
            None => true,
            Some(cg) => !cg.is_compatible(effects, groups, ctx.width, ctx.height),
        };
        if needs_rebuild {
            self.chain_graph = ChainGraph::try_build(
                effects,
                groups,
                primitive_registry(),
                gpu.device,
                ctx.width,
                ctx.height,
            );
        }
        let Some(cg) = self.chain_graph.as_mut() else {
            return false;
        };
        cg.run(gpu, input_texture, effects, groups, ctx).is_some()
    }

    /// Read the chain output texture after a successful
    /// [`Self::try_run_chain_graph`]. Returns `None` if the chain
    /// graph isn't cached (preceding `try_run_chain_graph` either
    /// wasn't called or returned `false`).
    fn chain_graph_output(&self) -> Option<&GpuTexture> {
        self.chain_graph.as_ref()?.output_texture()
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
        // Quick scan: any enabled effects? (Skip registry lookup — the main loop
        // handles unregistered effects. This just avoids buffer/context setup.)
        if !effects.iter().any(|fx| fx.enabled) {
            return None;
        }

        // Precompute cross-chain params for effects that need them.
        // Unity ref: EffectContext.FindChainParam() — VoronoiPrism reads EdgeStretch width.
        let chain_ctx = EffectContext {
            edge_stretch_width: find_chain_param(effects, &EffectTypeId::EDGE_STRETCH, 1, 0.5625),
            ..*ctx
        };

        // Fast path: try to render the whole chain through one
        // cached `Graph`. Bails (returns `false`) for chains with
        // partial-wet-dry groups, unmapped effects, etc. — those
        // fall through to the per-effect dispatch below. (The
        // per-effect cache stays alive on fallback paths so its
        // runners survive across mode transitions.)
        if self.try_run_chain_graph(gpu, input_texture, effects, groups, &chain_ctx) {
            return self.chain_graph_output();
        }

        // Performance: skip the internal blit that copies input -> ping buffer.
        // The first effect reads directly from input_texture, writing to the
        // chain's target buffer. Subsequent effects use normal ping-pong.
        // Saves ~629us per chain invocation at 4K (one fewer render pass).
        // Buffers are lazily created on the first effect that actually runs.
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
                                self.ensure_buffers(
                                    gpu.device, gpu.pool, ctx.width, ctx.height,
                                );
                                self.use_ping_as_source = true;
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
            //
            // Dispatch order:
            //   1. If the effect has a primitive mapping AND should
            //      not be skipped, route through the graph cache —
            //      `processor.should_skip` is queried from the legacy
            //      side because per-effect skip predicates (Mirror's
            //      amount=0, etc.) are still authoritative until the
            //      graph runtime grows its own bypass instruction.
            //   2. Otherwise fall back to the legacy `processor.apply`.
            // The graph cache returns `false` if the effect has no
            // primitive mapping yet (Mirror, SoftFocusGraph,
            // StylizedFeedback, QuadMirror, NodeGraphTest), letting
            // those keep their existing dispatch.
            let processor_opt = registry.get_mut(fx.effect_type());
            let should_skip = processor_opt
                .as_ref()
                .map(|p| p.should_skip(fx))
                .unwrap_or(true);
            if !should_skip {
                let has_primitive_mapping = primitive_id_for_effect(fx.effect_type()).is_some();

                // Lazily create ping/pong buffers on first real effect.
                if first_effect_pending {
                    self.ensure_buffers(gpu.device, gpu.pool, ctx.width, ctx.height);
                    self.use_ping_as_source = true;
                }

                // Graph-runtime dispatch (swap-based, no per-effect
                // copies) for primitive-mapped effects.
                //
                // The graph runner needs owned `RenderTarget`s it can
                // install into its backend slots, while the legacy
                // `PostProcessEffect::apply` takes borrowed
                // `&GpuTexture`. To get owned RTs for the graph path
                // we `Option::take` them out of chain.ping/pong,
                // hand them to the runner, then put them back in
                // their original ping/pong slots after the runner
                // returns ownership.
                //
                // On the very first effect, chain's `ping` holds
                // uninitialized garbage from `ensure_buffers` — the
                // graph runner would sample that instead of the
                // upstream input. Materialise input → ping with a
                // single GPU copy first. Pays the cost once per
                // chain invocation, not once per effect.
                let mut dispatched_via_graph = false;
                if has_primitive_mapping {
                    if first_effect_pending {
                        let ping_rt = self.ping.as_ref().unwrap();
                        gpu.copy_texture_to_texture(
                            input_texture,
                            &ping_rt.texture,
                            ctx.width,
                            ctx.height,
                        );
                        first_effect_pending = false;
                    }
                    let (source_rt, target_rt) = if self.use_ping_as_source {
                        (self.ping.take().unwrap(), self.pong.take().unwrap())
                    } else {
                        (self.pong.take().unwrap(), self.ping.take().unwrap())
                    };
                    let (source_back, target_back, did_dispatch) = self.graph_cache.apply(
                        primitive_registry(),
                        gpu,
                        source_rt,
                        target_rt,
                        fx,
                        &chain_ctx,
                    );
                    dispatched_via_graph = did_dispatch;
                    if self.use_ping_as_source {
                        self.ping = Some(source_back);
                        self.pong = Some(target_back);
                    } else {
                        self.pong = Some(source_back);
                        self.ping = Some(target_back);
                    }
                }

                if !dispatched_via_graph {
                    // Legacy dispatch — borrow source/target textures
                    // from the chain. Reading through `self.ping`/
                    // `self.pong`/`self.use_ping_as_source` directly
                    // (rather than `self.target()` / etc.) gives the
                    // borrow checker per-field visibility.
                    let (source_tex, target_tex): (&GpuTexture, &GpuTexture) = if first_effect_pending {
                        let tgt = if self.use_ping_as_source {
                            &self.pong.as_ref().unwrap().texture
                        } else {
                            &self.ping.as_ref().unwrap().texture
                        };
                        (input_texture, tgt)
                    } else if self.use_ping_as_source {
                        (
                            &self.ping.as_ref().unwrap().texture,
                            &self.pong.as_ref().unwrap().texture,
                        )
                    } else {
                        (
                            &self.pong.as_ref().unwrap().texture,
                            &self.ping.as_ref().unwrap().texture,
                        )
                    };
                    let processor = processor_opt.expect(
                        "should_skip was queried — processor must be present",
                    );
                    processor.apply(gpu, source_tex, target_tex, fx, &chain_ctx);
                }
                self.swap();
                first_effect_pending = false;
            }
        }

        // Drop runners whose effect ids are no longer in the chain.
        // Cheap when nothing has changed (set comparison only).
        self.graph_cache.prune(effects);

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
        // Drop cached graph-runtime executors so their pre-bound
        // RenderTargets return to the pool too. Both the unified
        // chain graph and the per-effect runner cache get cleared.
        self.chain_graph = None;
        self.graph_cache.drop_all();
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
        // Cached graph runners (both the unified chain graph and
        // the per-effect cache) hold width/height-sized
        // RenderTargets; force a rebuild rather than per-target
        // resize so we don't have to plumb resize through the
        // executor stack.
        self.chain_graph = None;
        self.graph_cache.drop_all();
    }

    /// Reset per-effect transient state across the cached graph
    /// runners (mip pyramids, feedback buffers, etc.). Mirrors the
    /// existing `EffectRegistry::clear_all_state` call sites — both
    /// paths fire on seek so the chain stays in sync.
    pub fn clear_graph_runner_state(&mut self) {
        if let Some(cg) = self.chain_graph.as_mut() {
            cg.clear_state();
        }
        self.graph_cache.clear_state();
    }
}

/// Process-wide [`PrimitiveRegistry`] used by every `EffectChain`'s
/// graph-runtime dispatch path. Built lazily on first call so the
/// renderer's effect-chain code doesn't have to thread a registry
/// reference through `apply_chain`'s already-wide signature.
fn primitive_registry() -> &'static PrimitiveRegistry {
    static CELL: OnceLock<PrimitiveRegistry> = OnceLock::new();
    CELL.get_or_init(PrimitiveRegistry::with_builtin)
}
