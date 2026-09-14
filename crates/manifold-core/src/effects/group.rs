//! Effect rack groups (`EffectGroup`). Extracted from effects.rs (P2-E, D4).

use super::{default_one, default_true};
use crate::id::{EffectGroupId, EffectId};
use serde::{Deserialize, Serialize};

// ─── Effect Group ───

/// A rack group containing multiple effects with shared bypass and wet/dry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGroup {
    pub id: EffectGroupId,
    #[serde(default = "default_group_name")]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default = "default_one")]
    pub wet_dry: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_group_id: Option<EffectGroupId>,
    /// The effect instance that supplies coverage for this group's mask.
    /// The referenced effect remains an ordinary member of the group's effect
    /// list and is serialized by its stable `EffectId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_effect_id: Option<EffectId>,
}

impl EffectGroup {
    pub fn new(name: String) -> Self {
        Self {
            id: EffectGroupId::new(crate::short_id()),
            name,
            enabled: true,
            collapsed: false,
            wet_dry: 1.0,
            parent_group_id: None,
            mask_effect_id: None,
        }
    }

    pub fn clone_with_new_id(&self) -> Self {
        let mut cloned = self.clone();
        cloned.id = EffectGroupId::new(crate::short_id());
        cloned
    }
}

fn default_group_name() -> String {
    "Group".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_mask_old_project_defaults_to_none() {
        let group: EffectGroup = serde_json::from_str(
            r#"{"id":"group-1","name":"Group","enabled":true,"collapsed":false,"wetDry":1.0}"#,
        )
        .unwrap();

        assert_eq!(group.mask_effect_id, None);
        let json = serde_json::to_value(&group).unwrap();
        assert!(json.get("maskEffectId").is_none());
    }

    #[test]
    fn group_mask_roundtrips_identity() {
        let mut group = EffectGroup::new("Masked".to_string());
        group.parent_group_id = Some(EffectGroupId::new("parent"));
        group.mask_effect_id = Some(EffectId::new("mask"));

        let reloaded: EffectGroup =
            serde_json::from_value(serde_json::to_value(&group).unwrap()).unwrap();

        assert_eq!(reloaded.id, group.id);
        assert_eq!(reloaded.parent_group_id, group.parent_group_id);
        assert_eq!(reloaded.mask_effect_id, group.mask_effect_id);
    }
}
