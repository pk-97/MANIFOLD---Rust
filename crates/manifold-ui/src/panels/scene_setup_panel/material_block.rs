//! The Material block of an item's card (SCENE_DOCUMENT_DESIGN.md section
//! 7.6): the Material an Object or Group draws with, inline — its "Used by
//! N" line and Make Unique, its Surface, Opacity, feature and Advanced
//! sections, and "+ Add feature". What features a Material has is a field of
//! the document, so adding one is one command and one undo.

use super::*;
use crate::panels::dropdown::DropdownItem;
use crate::param_surface::{
    MATERIAL_FEATURES, MaterialFeature, MaterialGroup, MaterialLook, MaterialParamRole,
};

/// The block's buttons built this frame, cleared on every rebuild.
#[derive(Default)]
pub(super) struct MaterialButtons {
    /// A feature section's "×": `(button, material, feature)`.
    remove: Vec<(NodeId, ItemId, MaterialFeature)>,
    /// "+ Add feature": `(button, material)`.
    add: Option<(NodeId, ItemId)>,
    /// Make Unique, or New Material on an item with none: `(button, the
    /// item that draws with the material)`.
    unique: Option<(NodeId, ItemId)>,
    /// The Preset menu: `(button, material, the look it matches)`.
    look: Option<(NodeId, ItemId, Option<MaterialLook>)>,
}

impl MaterialButtons {
    pub(super) fn clear(&mut self) {
        self.remove.clear();
        self.add = None;
        self.unique = None;
        self.look = None;
    }
}

pub(super) fn feature_label(feature: MaterialFeature) -> &'static str {
    match feature {
        MaterialFeature::Coat => "Coat",
        MaterialFeature::Iridescence => "Iridescence",
        MaterialFeature::Emission => "Emission",
        MaterialFeature::Glass => "Glass",
        MaterialFeature::Sheen => "Sheen",
        MaterialFeature::Anisotropy => "Anisotropy",
        MaterialFeature::Translucency => "Translucency",
        MaterialFeature::Subsurface => "Subsurface",
    }
}

/// The feature a Material row belongs to, if it sits in a feature section.
pub(super) fn row_feature(row: &ParamRow) -> Option<MaterialFeature> {
    match row.spec.material_role {
        Some(MaterialParamRole::FeatureMode(feature))
        | Some(MaterialParamRole::Scalar(MaterialGroup::Feature(feature)))
        | Some(MaterialParamRole::Colour(MaterialGroup::Feature(feature), ..)) => Some(feature),
        _ => None,
    }
}

/// Where a Material row sits in the block: Surface, Opacity, the features in
/// the Material's order, then Advanced and the dormant texture rows.
pub(super) fn material_bucket(row: &ParamRow, features: &[MaterialFeature]) -> usize {
    if let Some(feature) = row_feature(row) {
        return 2 + features
            .iter()
            .position(|candidate| *candidate == feature)
            .unwrap_or(features.len() + feature as usize);
    }
    match row.spec.material_role {
        Some(MaterialParamRole::Scalar(MaterialGroup::Surface))
        | Some(MaterialParamRole::Colour(MaterialGroup::Surface, ..)) => 0,
        Some(MaterialParamRole::Scalar(MaterialGroup::Opacity))
        | Some(MaterialParamRole::Colour(MaterialGroup::Opacity, ..)) => 1,
        Some(MaterialParamRole::Placement(family, ..))
        | Some(MaterialParamRole::Sampler(family, ..)) => 30 + family as usize,
        _ => 20,
    }
}

impl ScenePanel {
    /// The title of the section a row sits under. A Material row is titled by
    /// its inspector group; every other row by its control group.
    pub(super) fn material_section_name(&self, row: &ParamRow) -> Option<String> {
        let Some(role) = row.spec.material_role else {
            return Some(
                self.lighting_section_title(row)
                    .unwrap_or(self.property_group(row).title())
                    .to_string(),
            );
        };
        let name = match role {
            MaterialParamRole::Placement(..) | MaterialParamRole::Sampler(..) => {
                "Advanced \u{b7} Textures".to_string()
            }
            MaterialParamRole::FeatureMode(feature) => feature_label(feature).to_string(),
            MaterialParamRole::Scalar(group) | MaterialParamRole::Colour(group, ..) => {
                match group {
                    MaterialGroup::Surface => "Surface".to_string(),
                    MaterialGroup::Opacity => "Opacity & Cutout".to_string(),
                    MaterialGroup::Feature(feature) => feature_label(feature).to_string(),
                    MaterialGroup::Advanced => "Advanced".to_string(),
                }
            }
        };
        Some(name)
    }

    pub(super) fn material_section_folded(&self, name: &str) -> bool {
        self.section_folded
            .get(name)
            .copied()
            .unwrap_or_else(|| name == "Advanced" || name.starts_with("Advanced \u{b7} "))
    }

    /// "Used by N" and, for an item that draws with a shared Material, Make
    /// Unique; then the Preset menu. An Object or Group that draws with no
    /// Material gets New Material instead, so its colour is always one click
    /// away. Built under the Material section's fields.
    pub(super) fn build_material_summary(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        owner: &SceneItemVm,
        block: Option<&MaterialBlockVm>,
        (label_width, slider_w): (f32, f32),
    ) -> f32 {
        let (text, button) = match block {
            Some(block) if owner.id == block.item => (format!("Used by {}", block.used_by), None),
            Some(block) if block.used_by > 1 => (
                format!("{} \u{b7} used by {}", block.name, block.used_by),
                Some("Make Unique"),
            ),
            // A Material only this item uses is part of its card: no name row.
            Some(block) => {
                return self.build_material_look(tree, inner_x, cy, block, (label_width, slider_w));
            }
            None => ("Default".to_owned(), Some("New Material")),
        };
        let button_w = button.map_or(0.0, |label| {
            tree.text_width(label, color::FONT_LABEL, crate::node::FontWeight::Regular) + 16.0
        });
        let label_w = if button.is_some() {
            inner_w - button_w - GAP
        } else {
            inner_w
        };
        let label = tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            label_w.max(0.0),
            ROW_H,
            &text,
            label_style(),
        );
        tree.set_name(label, "scene_setup.material.used_by");
        if let Some(button) = button {
            let id = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + inner_w - button_w,
                cy,
                button_w,
                ROW_H,
                btn_style(),
                button,
                crate::param_surface::stable_key(&format!("scene.material.unique.{}", owner.id)),
            );
            tree.set_name(
                id,
                if block.is_some() {
                    "scene_setup.material.make_unique"
                } else {
                    "scene_setup.material.new"
                },
            );
            self.material_block.unique = Some((id, owner.id.clone()));
        }
        cy += ROW_H + ROW_GAP;
        match block {
            Some(block) => {
                self.build_material_look(tree, inner_x, cy, block, (label_width, slider_w))
            }
            None => cy,
        }
    }

    /// The Preset row: the look the Material matches, or Custom; a click
    /// lists every look. Only a PBR Material has looks.
    fn build_material_look(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        cy: f32,
        block: &MaterialBlockVm,
        (label_width, slider_w): (f32, f32),
    ) -> f32 {
        use crate::panels::param_slider_shared::TOGGLE_BTN_H;
        if !block.pbr {
            return cy;
        }
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            label_width,
            ROW_H,
            "Preset",
            label_style(),
        );
        let start = label_width.min(slider_w);
        let current = block.look.map_or("Custom", MaterialLook::label);
        let id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + start,
            cy + (ROW_H - TOGGLE_BTN_H) * 0.5,
            (slider_w - start).max(0.0),
            TOGGLE_BTN_H,
            crate::chrome::components::dropdown_trigger_style(btn_style().font_size),
            current,
            crate::param_surface::stable_key(&format!("scene.material.look.{}", block.item)),
        );
        tree.set_name(id, "scene_setup.material.preset");
        self.material_block.look = Some((id, block.item.clone(), block.look));
        cy + ROW_H + ROW_GAP
    }

    /// A feature section's "×", at the right end of its header row.
    pub(super) fn build_feature_remove(
        &mut self,
        tree: &mut UITree,
        x: f32,
        cy: f32,
        material: &ItemId,
        feature: MaterialFeature,
    ) {
        let remove = tree.add_button_keyed(
            Some(self.content_parent),
            x,
            cy,
            ROW_H,
            ROW_H,
            btn_style(),
            "\u{d7}",
            crate::param_surface::stable_key(&format!(
                "scene.material.remove.{material}.{feature:?}"
            )),
        );
        tree.set_name(
            remove,
            format!("material.feature.remove.{}", feature_label(feature)),
        );
        self.material_block
            .remove
            .push((remove, material.clone(), feature));
    }

    /// "+ Add feature": one button; its menu lists the features the Material
    /// lacks.
    pub(super) fn build_add_feature(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        block: &MaterialBlockVm,
    ) -> f32 {
        if !block.pbr
            || MATERIAL_FEATURES
                .iter()
                .all(|feature| block.features.contains(feature))
        {
            return cy;
        }
        let id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(),
            "+ Add feature",
            crate::param_surface::stable_key(&format!("scene.material.add.{}", block.item)),
        );
        tree.set_name(id, "scene_setup.material.add_feature");
        self.material_block.add = Some((id, block.item.clone()));
        cy + ROW_H + ROW_GAP
    }

    /// A click on one of the block's buttons.
    pub(super) fn material_click(&self, node_id: NodeId, vm: &SceneSetupVm) -> Option<PanelAction> {
        let layer = || vm.layer_id.clone();
        if let Some((_, material, feature)) = self
            .material_block
            .remove
            .iter()
            .find(|(button, _, _)| *button == node_id)
        {
            return Some(PanelAction::Project(ProjectAction::SceneMaterialFeature {
                layer_id: layer(),
                material: material.clone(),
                feature: *feature,
                add: false,
            }));
        }
        if let Some((_, item)) = self
            .material_block
            .unique
            .as_ref()
            .filter(|(button, _)| *button == node_id)
        {
            return Some(PanelAction::Project(
                ProjectAction::SceneMakeMaterialUnique(layer(), item.clone()),
            ));
        }
        if let Some((_, material, current)) = self
            .material_block
            .look
            .as_ref()
            .filter(|(button, ..)| *button == node_id)
        {
            let items = MaterialLook::ALL
                .into_iter()
                .map(|look| {
                    DropdownItem::new(look.label())
                        .with_check(Some(look) == *current)
                        .with_action(PanelAction::Project(ProjectAction::SceneMaterialLook {
                            layer_id: layer(),
                            material: material.clone(),
                            look,
                        }))
                })
                .collect();
            return Some(PanelAction::Root(RootAction::SceneSetupActionMenuClicked {
                button_node_id: node_id,
                items,
            }));
        }
        let (_, material) = self
            .material_block
            .add
            .as_ref()
            .filter(|(button, _)| *button == node_id)?;
        let present: &[MaterialFeature] = vm
            .items
            .iter()
            .find_map(|item| {
                item.material
                    .as_ref()
                    .filter(|block| &block.item == material)
            })
            .map_or(&[], |block| &block.features);
        let items = MATERIAL_FEATURES
            .iter()
            .filter(|feature| !present.contains(feature))
            .map(|feature| {
                let mut item = DropdownItem::new(feature_label(*feature));
                item.action = Some(PanelAction::Project(ProjectAction::SceneMaterialFeature {
                    layer_id: layer(),
                    material: material.clone(),
                    feature: *feature,
                    add: true,
                }));
                item
            })
            .collect();
        Some(PanelAction::Root(RootAction::SceneSetupActionMenuClicked {
            button_node_id: node_id,
            items,
        }))
    }

    fn material_rgb_members_for_param(
        &self,
        param_id: &manifold_foundation::ParamId,
    ) -> Option<[manifold_foundation::ParamId; 3]> {
        self.properties_card.rows.iter().find_map(|row| {
            row.rgb_members
                .as_ref()
                .filter(|members| members.editable)
                .map(|members| &members.ids)
                .filter(|members| members.contains(param_id))
                .cloned()
        })
    }

    fn material_rgb_values(
        &self,
        members: &[manifold_foundation::ParamId; 3],
        changed_id: &manifold_foundation::ParamId,
        changed_value: f32,
    ) -> Option<[f32; 3]> {
        let mut values = [0.0; 3];
        for (index, id) in members.iter().enumerate() {
            let row = self
                .properties_card
                .row_id_index
                .get(id.as_ref())
                .copied()?;
            values[index] = self.properties_card.current_values.get(row).copied()?;
        }
        let channel = members.iter().position(|id| id == changed_id)?;
        values[channel] = changed_value;
        Some(values)
    }

    /// A colour channel's scrub, rewritten as the colour's one three-channel
    /// gesture, so a drag on one channel of a grouped colour is one undo step.
    pub(super) fn rewrite_material_rgb_actions(
        &self,
        actions: Vec<PanelAction>,
    ) -> Vec<PanelAction> {
        actions
            .into_iter()
            .map(|action| {
                let PanelAction::Scrub(crate::ValueRef::Param(target, param_id), phase) = action
                else {
                    return action;
                };
                let Some(members) = self.material_rgb_members_for_param(&param_id) else {
                    return PanelAction::Scrub(crate::ValueRef::Param(target, param_id), phase);
                };
                match phase {
                    crate::ScrubPhase::Begin => PanelAction::Scrub(
                        crate::ValueRef::ParamRgb(target, members),
                        crate::ScrubPhase::Begin,
                    ),
                    crate::ScrubPhase::Move(crate::ScrubValue::Scalar(value)) => {
                        let Some(values) = self.material_rgb_values(&members, &param_id, value)
                        else {
                            return PanelAction::Scrub(
                                crate::ValueRef::Param(target, param_id),
                                crate::ScrubPhase::Move(crate::ScrubValue::Scalar(value)),
                            );
                        };
                        PanelAction::Scrub(
                            crate::ValueRef::ParamRgb(target, members),
                            crate::ScrubPhase::Move(crate::ScrubValue::Rgb(values)),
                        )
                    }
                    crate::ScrubPhase::Commit => PanelAction::Scrub(
                        crate::ValueRef::ParamRgb(target, members),
                        crate::ScrubPhase::Commit,
                    ),
                    other => PanelAction::Scrub(crate::ValueRef::Param(target, param_id), other),
                }
            })
            .collect()
    }

    pub(super) fn material_full_value(&self, id: &manifold_foundation::ParamId) -> f32 {
        self.properties_card
            .row_id_index
            .get(id.as_ref())
            .and_then(|&slot| self.properties_card.current_values.get(slot).copied())
            .or_else(|| {
                self.full_params
                    .as_ref()
                    .and_then(|surface| {
                        surface
                            .rows
                            .get(*self.full_param_id_index.get(id.as_ref())?)
                    })
                    .map(|row| row.value.base)
            })
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }
}
