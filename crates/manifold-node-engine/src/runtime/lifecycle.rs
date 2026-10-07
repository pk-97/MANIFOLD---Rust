//! Runtime state clearing, including sparse Math View evaluations.

use super::*;

impl PresetRuntime {
    /// Forwarded `clear_state` for each effect node — called on seek
    /// / project load so trails, feedback, and mip pyramids don't
    /// carry stale content across playback discontinuities. Also
    /// wipes the chain's `StateStore` so primitives that key state
    /// there (e.g. `temporal::Feedback`'s prev-frame buffer) reset
    /// alongside instance-local state.
    pub fn clear_state(&mut self) {
        self.executor.reset_scene_viewport_state();
        for view in &mut self.math_views {
            view.events.clear();
            for variant in &mut view.variants {
                variant.clear_state();
            }
        }
        self.pending_trigger_baseline = None;
        if let Some(events) = &mut self.modifier_events {
            events.clear();
        }
        // Collect node ids first so we can release the &self borrow
        // before calling get_node_mut on each.
        let mut nodes_to_clear: Vec<NodeInstanceId> = Vec::new();
        for slot in &self.effect_nodes {
            for (_, id) in &slot.handles {
                nodes_to_clear.push(*id);
            }
        }
        for node_id in nodes_to_clear {
            if let Some(inst) = self.graph.get_node_mut(node_id) {
                inst.node.clear_state();
            }
        }
        self.state_store.cleanup_all();
    }

    /// State harvest across a rebuild (docs/CHAIN_FUSION_DESIGN.md section 5): for
    /// every card whose `(effect_id, def_content_key)` matches a card in the
    /// prior runtime, move the prior node *impls* (the `Box<dyn EffectNode>`
    /// holding sim buffers, trail textures, DNN workers) and their StateStore
    /// buckets into this runtime, matched per node by stable `NodeId` + type.
    ///
    /// Safe because a matching content key means the prior impl's baked
    /// configuration (ports, WGSL source, pipelines — all derived from the
    /// def) is identical to what this build just constructed; params and
    /// bindings live on `NodeInstance` / the binding apply path and are this
    /// build's own. Skipped when dimensions changed — resolution-dependent
    /// state must rebuild, exactly as today. Cards that were edited (key
    /// mismatch) or removed keep fresh instances; intentional resets (seek,
    /// project load, idle clear, card deletion) run through `clear_state` /
    /// pool eviction, untouched by this path.
    pub(super) fn harvest_state_from(&mut self, prior: &mut Self) {
        if prior.width != self.width || prior.height != self.height {
            return;
        }
        // Harvest only when the chain is the SAME SET of active cards —
        // reorders, value edits, editor open/close, fused-segment swap-ins.
        // A membership change (card added / removed / enabled / disabled /
        // skip-flipped) resets everything, exactly as before the harvest
        // existed: a feedback trail accumulated through a card that was just
        // toggled off holds that card's look — and latching blends (Screen /
        // Additive at full amount) would hold it FOREVER, leaving stale
        // blown-out frames rotating in the loop with no escape. Toggling is
        // an intentional look change; the reset is the escape hatch.
        let same_card_set = self.effect_nodes.len() == prior.effect_nodes.len()
            && self.effect_nodes.iter().all(|s| {
                prior
                    .effect_nodes
                    .iter()
                    .any(|p| p.effect_id == s.effect_id)
            });
        if !same_card_set {
            return;
        }
        // new instance → old instance, for every node whose impl was carried
        // over. Drives the persistent-texture pass below.
        let mut harvested: ahash::AHashMap<NodeInstanceId, NodeInstanceId> =
            ahash::AHashMap::default();
        for (idx, slot) in self.effect_nodes.iter().enumerate() {
            if slot.def_content_key == 0 {
                continue;
            }
            let Some((old_idx, old_slot)) = prior.effect_nodes.iter().enumerate().find(|(_, s)| {
                s.effect_id == slot.effect_id
                    && s.effect_type == slot.effect_type
                    && s.def_content_key == slot.def_content_key
            }) else {
                continue;
            };
            // A stateful card's state is a function of what FEEDS it — a
            // feedback trail is a picture of the upstream chain. Carry it
            // only when the ordered sequence of cards before this one is
            // unchanged; an upstream reorder resets exactly this card (the
            // trail's content no longer corresponds to anything the chain
            // produces, and latching blends would hold the stale look
            // forever). Downstream reorders carry. Identity is by EffectId,
            // not content key, so upstream VALUE edits still carry — the
            // trail just evolves with the new look.
            let prefix_unchanged = idx == old_idx
                && self.effect_nodes[..idx]
                    .iter()
                    .zip(&prior.effect_nodes[..old_idx])
                    .all(|(a, b)| a.effect_id == b.effect_id);
            if !prefix_unchanged {
                continue;
            }
            for (node_id, new_inst) in &slot.node_map {
                let Some((_, old_inst)) = old_slot.node_map.iter().find(|(nid, _)| nid == node_id)
                else {
                    continue;
                };
                let Some(old_node) = prior.graph.get_node_mut(*old_inst) else {
                    continue;
                };
                let Some(new_node) = self.graph.get_node_mut(*new_inst) else {
                    continue;
                };
                if old_node.node.type_id() != new_node.node.type_id() {
                    continue;
                }
                std::mem::swap(&mut old_node.node, &mut new_node.node);
                prior
                    .state_store
                    .migrate_node(*old_inst, *new_inst, &mut self.state_store);
                harvested.insert(*new_inst, *old_inst);
            }
        }
        if std::env::var("MANIFOLD_LOG_HARVEST").is_ok() {
            eprintln!(
                "[harvest] slots new={} prior={} nodes_carried={} persistent_new={}",
                self.effect_nodes.len(),
                prior.effect_nodes.len(),
                harvested.len(),
                self.plan.persistent_resources().len(),
            );
        }
        if harvested.is_empty() {
            return;
        }

        // Cross-frame PIXELS live in backend persistent slots, not in the
        // impls or the StateStore — feedback's trail is its persistent `out`
        // texture, and the back-edge producer's slot is the other half of
        // the zero-copy ping-pong (`FeedbackState` tracks only dims + mode).
        // Install each harvested node's persistent textures into the new
        // backend's slots: one atomic retain per texture, no GPU copy. The
        // migrated `FeedbackState` then correctly skips its first-frame
        // re-seed, reading the carried trail. (Array-buffer state —
        // `aliased_array_io` — is not migrated; no chain effect uses it.)
        let producer_of = |plan: &ExecutionPlan, node: NodeInstanceId, port: &str| {
            plan.steps().iter().find(|s| s.node == node).and_then(|s| {
                s.outputs
                    .iter()
                    .find(|(name, _)| *name == port)
                    .map(|(_, id)| *id)
            })
        };
        // (new persistent resource, old persistent resource) pairs.
        let mut moves: Vec<(ResourceId, ResourceId)> = Vec::new();
        for &res in self.plan.persistent_resources() {
            // Producing (node, port) of this persistent resource in the new plan.
            let Some((n_inst, port)) = self.plan.steps().iter().find_map(|s| {
                s.outputs
                    .iter()
                    .find(|(_, id)| *id == res)
                    .map(|(name, _)| (s.node, *name))
            }) else {
                continue;
            };
            let Some(&o_inst) = harvested.get(&n_inst) else {
                continue;
            };
            let Some(o_res) = producer_of(&prior.plan, o_inst, port) else {
                continue;
            };
            moves.push((res, o_res));
        }
        if moves.is_empty() {
            return;
        }
        // MOVE the owned RenderTargets across backends. Ownership (and pool
        // bookkeeping) transfers with the target; the prior runtime is being
        // dropped, so its emptied slots never render again. NEVER install via
        // `replace_texture_2d` here — that records a borrowed SHADOW over the
        // slot, and the feedback ping-pong's `swap_texture_2d` refuses
        // shadowed slots, freezing the trail with per-frame swap errors.
        let Some(old_metal) = prior
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
        else {
            return; // mock backend — nothing to move
        };
        let Some(new_metal) = self
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
        else {
            return;
        };
        let mut installed = 0usize;
        let total = moves.len();
        let mut installed_res: Vec<ResourceId> = Vec::with_capacity(total);
        for (res, o_res) in moves {
            let Some(old_slot) = crate::exec::backend::Backend::slot_for(old_metal, o_res) else {
                continue;
            };
            let Some(new_slot) = crate::exec::backend::Backend::slot_for(new_metal, res) else {
                continue;
            };
            let Some(rt) = old_metal.take_render_target(old_slot) else {
                continue;
            };
            // The displaced fresh target drops here (pooled → returns to pool).
            let _fresh = new_metal.swap_texture_2d(new_slot, rt);
            installed += 1;
            installed_res.push(res);
        }
        // The fresh executor would clear-to-black each persistent slot on
        // first acquisition — mark the carried ones initialized instead.
        for res in installed_res {
            self.executor.mark_persistent_initialized(res);
        }
        if std::env::var("MANIFOLD_LOG_HARVEST").is_ok() {
            eprintln!("[harvest] persistent targets moved {installed}/{total}");
        }
    }
}
