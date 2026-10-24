//! Segment binding resolution: a fused segment builds only when every member
//! card keeps all of its params live; otherwise it is refused.

use super::*;

/// A fused segment that cannot be built with every member's params live. The
/// chain build refuses the segment by its cache key and rebuilds its cards one
/// by one.
pub(crate) struct SegmentRefusal {
    pub(crate) key: u64,
    effect_ids: Vec<EffectId>,
    reason: String,
}

impl SegmentRefusal {
    pub(crate) fn new(key: u64, members: &[&PresetInstance], reason: String) -> Self {
        Self {
            key,
            effect_ids: members.iter().map(|fx| fx.id.clone()).collect(),
            reason,
        }
    }
}

impl std::fmt::Display for SegmentRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&str> = self.effect_ids.iter().map(|id| id.as_str()).collect();
        write!(
            f,
            "segment of effects [{}] refused: {}",
            ids.join(", "),
            self.reason
        )
    }
}

/// Every member card's static and user bindings, resolved against the segment
/// splice (`graph` + `node_map`), in member order. `Err` names each binding
/// that did not resolve; the caller refuses the segment rather than build a
/// member with params dropped.
pub(crate) fn resolve_segment_bindings(
    view: &crate::freeze::install::SegmentView,
    members: &[&PresetInstance],
    graph: &Graph,
    node_map: &[(NodeId, NodeInstanceId)],
) -> Result<Vec<Vec<ResolvedBinding>>, String> {
    let mut resolved = Vec::with_capacity(members.len());
    let mut missing: Vec<String> = Vec::new();
    for (seg_idx, fx) in members.iter().enumerate() {
        let prefix = crate::freeze::segment::card_prefix(seg_idx);
        let Some(card_static) = view.card_bindings.get(seg_idx) else {
            missing.push(format!("{} has no bindings in the segment", fx.id.as_str()));
            continue;
        };
        let user_bindings = fx.user_param_bindings();
        let mut bindings = Vec::with_capacity(card_static.len() + user_bindings.len());
        for b in card_static {
            match ResolvedBinding::from_static(b, node_map) {
                Some(rb) => bindings.push(rb),
                None => missing.push(format!("{} param {}", fx.id.as_str(), b.id.as_ref())),
            }
        }
        for core in user_bindings.iter() {
            // Repoint into the segment namespace: a fused-away target goes
            // through the segment retarget map; a surviving target resolves
            // by prefixing its stable node id.
            let prefixed_key = (
                format!("{prefix}{}", core.node_id.as_str()),
                core.inner_param.clone(),
            );
            let mut c = core.clone();
            if let Some((fused_id, field)) = view.retarget.get(&prefixed_key) {
                c.node_id = fused_id.clone();
                c.inner_param = field.clone();
                c.convert = crate::freeze::install::convert_for_fused_field(c.convert);
            } else {
                c.node_id = NodeId::new(prefixed_key.0.clone());
            }
            match ResolvedBinding::from_user(&c, graph, node_map) {
                Some(rb) => bindings.push(rb),
                None => missing.push(format!("{} user param {}", fx.id.as_str(), core.id)),
            }
        }
        resolved.push(bindings);
    }
    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(format!("bindings did not resolve: {}", missing.join("; ")))
    }
}
