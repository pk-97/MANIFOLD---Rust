//! Which outputs a [`PresetRuntime`] holds for dumps: every output, the
//! thumbnail atlas's texture set, or a node-scoped array set.

use super::*;

impl PresetRuntime {
    /// Enable one-shot "dump every output" mode iff this chain holds
    /// `dump_effect`; otherwise disable it. Call each frame with the requested
    /// effect (or `None`) so only the watched effect's chain pays the cost.
    /// This is the Cmd+D disk dump (whole graph); the editor thumbnail atlas
    /// uses [`Self::set_dump_visible`] instead (only the visible nodes).
    pub fn set_dump(&mut self, dump_effect: Option<&EffectId>) {
        let on =
            dump_effect.is_some_and(|eid| self.effect_nodes.iter().any(|s| &s.effect_id == eid));
        self.executor.set_dump_all(on);
    }

    /// Set (or clear) the continuous thumbnail-atlas dump for this chain —
    /// record only the nodes the editor canvas can currently show, resolved
    /// from their stable [`NodeId`]s to runtime instances via the owning slot's
    /// `node_map`. A `visible` id that names a selected group resolves to its
    /// primary-output producer via `group_preview_map`, mirroring
    /// [`Self::set_preview_target`]. `effect_id` selects the owning effect slot;
    /// pass `None` for a generator runtime (one graph, every slot eligible). A
    /// chain that doesn't hold the requested effect clears its set, so only the
    /// watched chain pays. Hidden / off-scope nodes are simply absent from the
    /// set, so they keep memoization and their textures recycle (sub-changes
    /// A + B). Textures only: arrays are held through [`Self::set_dump_arrays`].
    pub fn set_dump_visible(&mut self, effect_id: Option<&EffectId>, visible: &[NodeId]) {
        self.set_dump_visible_with_context(effect_id, visible, None);
    }

    pub fn set_dump_visible_with_context(
        &mut self,
        effect_id: Option<&EffectId>,
        visible: &[NodeId],
        context: Option<&super::ModifierPreviewContext>,
    ) {
        let set = self.resolve_nodes(effect_id, visible, context);
        self.executor.set_dump_set(set);
    }

    /// Hold the `Array` outputs of `nodes` after every frame, for
    /// [`Self::dump_arrays`] and [`Self::dump_arrays_all`], resolved as
    /// [`Self::set_dump_visible`] resolves; an empty list stops. The texture
    /// atlas set never holds arrays, so a node-scoped array read comes here.
    pub fn set_dump_arrays(&mut self, effect_id: Option<&EffectId>, nodes: &[NodeId]) {
        let set = if nodes.is_empty() { None } else { self.resolve_nodes(effect_id, nodes, None) };
        self.executor.set_dump_array_set(set);
    }

    /// `nodes` as runtime instances in the slot `effect_id` names (every slot
    /// for `None`); `None` when this chain doesn't hold that effect.
    fn resolve_nodes(
        &self,
        effect_id: Option<&EffectId>,
        visible: &[NodeId],
        context: Option<&super::ModifierPreviewContext>,
    ) -> Option<ahash::AHashSet<NodeInstanceId>> {
        let mut set: ahash::AHashSet<NodeInstanceId> = ahash::AHashSet::new();
        let mut matched = effect_id.is_none();
        for slot in &self.effect_nodes {
            if let Some(eid) = effect_id {
                if &slot.effect_id != eid {
                    continue;
                }
                matched = true;
            }
            for nid in visible {
                let nid = if let Some(context) = context {
                    let Ok(resolved) = super::modifier_preview::resolve(&self.modifier_preview_routes, context, nid) else { continue; };
                    resolved
                } else { nid };
                if let Some((_, instance)) =
                    slot.node_map.iter().find(|(mapped, _)| mapped == nid)
                {
                    set.insert(*instance);
                } else if let Some((_, producer, _)) =
                    slot.group_preview_map.iter().find(|(group, _, _)| group == nid)
                    && let Some((_, instance)) =
                        slot.node_map.iter().find(|(mapped, _)| mapped == producer)
                {
                    set.insert(*instance);
                }
            }
        }
        matched.then_some(set)
    }

    /// Clear any thumbnail-atlas dump set on this chain (atlas off, or this
    /// chain isn't the watched one).
    pub fn clear_dump_set(&mut self) {
        self.executor.set_dump_set(None);
    }

    /// Enable/disable one-shot "dump every output" mode on the executor
    /// (preserve every Texture2D output for one frame). Generator path; the
    /// effect chain uses [`Self::set_dump`] (gated by effect id).
    pub fn set_dump_all(&mut self, on: bool) {
        self.executor.set_dump_all(on);
    }
}
