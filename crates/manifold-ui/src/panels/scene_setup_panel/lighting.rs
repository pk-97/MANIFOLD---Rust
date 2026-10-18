//! The Scene tab's Lighting item: every lighting control in one place. The
//! environment's source comes first (its HDRI file, or the add-environment
//! row), then the environment and exposure rows, then one sub-section per
//! light headed by that light's own header (rename, delete). The rows are
//! ordinary properties rows; this module only gathers and orders them.

use super::*;

const KEY_HDRI_FILE: u64 = 80_014;

/// One light's sub-section in the current Lighting pass.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PropertyLight {
    pub(super) doc_id: u32,
    pub(super) index: usize,
    pub(super) name: String,
    ids: Vec<String>,
}

impl SceneSelection {
    /// What the tab shows for this selection: a light shows as Lighting.
    pub fn shown(self) -> Self {
        match self {
            SceneSelection::Light(_) => SceneSelection::Lighting,
            other => other,
        }
    }
}

impl ScenePanel {
    pub(super) fn build_lighting_properties(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        vm: &SceneSetupVm,
        owned: &[String],
    ) -> f32 {
        let mut cy = self.build_environment_source(tree, (inner_x, inner_w), cy, vm);
        self.property_lights = vm
            .items
            .iter()
            .filter_map(|item| {
                let SceneSelection::Light(doc_id) = item.selection else { return None };
                vm.lights.iter().find_map(|light| match light {
                    LightRowVm::Known(row) if row.node_doc_id == doc_id => Some(PropertyLight {
                        doc_id,
                        index: row.index,
                        name: row.name.clone(),
                        ids: item.parameter_ids.clone(),
                    }),
                    _ => None,
                })
            })
            .collect();
        self.property_lights_drawn = 0;
        let mut ids = owned.to_vec();
        ids.extend(self.property_lights.iter().flat_map(|light| light.ids.iter().cloned()));
        cy = self.build_item_properties(tree, inner_x, inner_w, cy, PropertyRows::new(&ids, ControlGroup::Environment));
        cy = self.build_light_headers_through(tree, (inner_x, inner_w), cy, usize::MAX);
        for light in &vm.lights {
            if let LightRowVm::Custom { index } = light {
                let label = format!("Light {index} — custom (edit in graph)");
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &label, label_style());
                cy += ROW_H;
            }
        }
        cy
    }

    /// Where the environment comes from: the HDRI file row (the generator's
    /// own string param edit, as on the layer card), a custom producer, or
    /// the add-environment row.
    fn build_environment_source(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        vm: &SceneSetupVm,
    ) -> f32 {
        match &vm.environment {
            EnvironmentRowVm::Importer { hdri_file, hdri_param: Some(index), .. } => {
                let file = self
                    .full_params
                    .as_ref()
                    .and_then(|surface| surface.string_params.get(*index))
                    .map(|sp| sp.display_value.clone().unwrap_or_else(|| sp.value.clone()))
                    .unwrap_or_else(|| hdri_file.clone());
                let name = std::path::Path::new(&file).file_name().map_or(file.clone(), |name| name.to_string_lossy().into_owned());
                let text = if name.is_empty() { "HDRI File: (empty)".to_string() } else { format!("HDRI File: {name}") };
                let id = tree.add_button_keyed(
                    Some(self.content_parent), inner_x, cy, inner_w, ROW_H, string_row_style(), &text, KEY_HDRI_FILE,
                );
                tree.set_name(id, "scene_setup.lighting.hdri_file");
                self.hdri_file_id = Some((id, *index));
                cy += ROW_H;
            }
            EnvironmentRowVm::Importer { hdri_file, .. } if !hdri_file.is_empty() => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &format!("HDRI: {hdri_file}"), label_style());
                cy += ROW_H;
            }
            EnvironmentRowVm::Custom => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Environment: Custom (edit in graph)", label_style());
                cy += ROW_H;
            }
            EnvironmentRowVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Environment: None", label_style());
                cy += ROW_H;
                self.add_environment_id = Some(tree.add_button_keyed(
                    Some(self.content_parent), inner_x, cy, inner_w, ROW_H, btn_style(), "+ Add Environment", KEY_ADD_ENVIRONMENT,
                ));
                cy += ROW_H;
            }
            EnvironmentRowVm::Importer { .. } | EnvironmentRowVm::Bare { .. } => {}
        }
        cy
    }

    /// The HDRI file row opens the same string param edit as the layer card.
    pub(super) fn hdri_file_click(&self, node_id: NodeId) -> Option<PanelAction> {
        let (_, index) = self.hdri_file_id.filter(|(id, _)| *id == node_id)?;
        Some(PanelAction::Params(ParamsAction::GenStringParamClicked(index)))
    }

    /// The HDRI file row's current text and screen rect, for the text entry
    /// its click opens; `None` unless the row for string param `index` is
    /// built.
    pub fn string_param_entry(&self, tree: &UITree, index: usize) -> Option<(String, Rect)> {
        let (id, _) = self.hdri_file_id.filter(|(_, built)| *built == index)?;
        let value = self.full_params.as_ref()?.string_params.get(index)?.value.clone();
        Some((value, tree.get_bounds(id)))
    }

    /// Which light's sub-section `row` sits in during a Lighting pass.
    pub(super) fn property_light_of(&self, row: &ParamRow) -> Option<usize> {
        self.property_lights.iter().position(|light| light.ids.iter().any(|id| id == row.id.as_ref()))
    }

    /// Draw every light header not drawn yet, up to and including light
    /// `last`, so a light with no rows to show still gets its header in
    /// light order.
    pub(super) fn build_light_headers_through(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        last: usize,
    ) -> f32 {
        while self.property_lights_drawn <= last && self.property_lights_drawn < self.property_lights.len() {
            let PropertyLight { doc_id, index, name, .. } = self.property_lights[self.property_lights_drawn].clone();
            cy = self.build_light_header(tree, (inner_x, inner_w), cy, (doc_id, index, &name));
            self.property_lights_drawn += 1;
        }
        cy
    }

    /// Lighting orders its rows environment, exposure, then each light in
    /// item order; every other pass ranks all rows equal.
    pub(super) fn property_section_rank(&self, row: &ParamRow) -> usize {
        if let Some(index) = self.property_light_of(row) {
            return index + 2;
        }
        usize::from(self.lighting_section_title(row).is_some())
    }

    /// Inside Lighting the camera's exposure is its own section.
    pub(super) fn lighting_section_title(&self, row: &ParamRow) -> Option<&'static str> {
        let lighting = self.state.as_live().is_some_and(|vm| self.selection.get(&vm.layer_id) == Some(&SceneSelection::Lighting));
        (lighting && row.spec.scene.is_some_and(|scene| scene.group == ControlGroup::Lens)).then_some("Exposure")
    }
}

fn string_row_style() -> UIStyle {
    UIStyle {
        bg_color: color::INSPECTOR_BG,
        text_color: color::TEXT_WHITE_C32,
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    }
}
