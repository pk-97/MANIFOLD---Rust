//! Ordered variadic inputs: a node with numbered inputs (`body_0`, `body_1`, ...) and a
//! count param that sizes them. A member's number is its order and sits on
//! exactly one wire. The count grows to cover every wired number; nothing
//! renumbers, and no count is capped.

use crate::effect_graph_def::{EffectGraphNode, SerializedParamValue};
use crate::scene_surface::{PHYSICS_WORLD_DEFAULT_BODIES, PHYSICS_WORLD_DEFAULT_COPY_GROUPS};

/// One ordered family on an owner node.
#[derive(Debug, Clone, Copy)]
pub struct Variadic {
    /// The owner param whose value is how many ports of the family exist.
    pub count_param: &'static str,
    /// The count when none is saved.
    pub default_count: u32,
}

/// A Physics World's body family.
pub const WORLD_BODIES: Variadic = Variadic {
    count_param: "bodies",
    default_count: PHYSICS_WORLD_DEFAULT_BODIES,
};

/// A Physics World's copy-group family.
pub const WORLD_COPY_GROUPS: Variadic = Variadic {
    count_param: "copy_groups",
    default_count: PHYSICS_WORLD_DEFAULT_COPY_GROUPS,
};

impl Variadic {
    /// The owner's saved count, or the family's default.
    pub fn count(&self, owner: &EffectGraphNode) -> u32 {
        match owner.params.get(self.count_param) {
            Some(SerializedParamValue::Float { value }) if value.is_finite() && *value >= 0.0 => value.round() as u32,
            Some(SerializedParamValue::Int { value }) => u32::try_from(*value).unwrap_or(self.default_count),
            _ => self.default_count,
        }
    }

    /// Grow the owner's count so number `k` has its port. A count already that
    /// big is left alone, so nothing shrinks and a count is written only when
    /// it must change.
    pub fn grow_to_cover(&self, owner: &mut EffectGraphNode, k: u32) {
        let needed = k + 1;
        if self.count(owner) < needed {
            owner.params.insert(self.count_param.to_string(), SerializedParamValue::Float { value: needed as f32 });
        }
    }
}
