//! Material inspector grouping, controls, placement, and focused tests.

use super::*;

impl ScenePanel {
    pub(super) fn material_row_feature(&self, row: &ParamRow) -> Option<crate::param_surface::MaterialFeature> {
        Self::row_feature(row).or_else(|| self.material_object_gain(row)
            .then_some(crate::param_surface::MaterialFeature::Emission))
    }

    fn sync_material_feature_order(&mut self) {
        use crate::param_surface::MaterialFeature;
        let features = [MaterialFeature::Coat, MaterialFeature::Iridescence, MaterialFeature::Emission,
            MaterialFeature::Glass, MaterialFeature::Sheen, MaterialFeature::Anisotropy,
            MaterialFeature::Translucency];
        let visible = features.map(|feature| self.full_params.as_ref()
            .is_some_and(|surface| self.material_feature_visible(&surface.rows, feature)));
        for (feature, visible) in features.into_iter().zip(visible) {
            if visible {
                self.material_visible_features.insert(feature);
                if !self.material_feature_order.contains(&feature) {
                    self.material_feature_order.push(feature);
                }
            }
        }
    }

    pub(super) fn material_remove_action(
        &self,
        feature: crate::param_surface::MaterialFeature,
        mode_id: &manifold_foundation::ParamId,
        target: GraphParamTarget,
    ) -> Vec<PanelAction> {
        let Some(info) = self.active_material_info.as_ref() else { return Vec::new() };
        if !self.material_action_context(&info.object, &info.material, false) {
            return Vec::new();
        }
        vec![PanelAction::Project(ProjectAction::MaterialParamsSet {
            target,
            object: info.object.clone(),
            material: info.material.clone(),
            kind: MaterialEditKind::Feature,
            writes: vec![MaterialParamWrite { param_id: mode_id.clone(), value: 3.0 }],
            description: format!("Remove {} feature", Self::material_feature_label(feature)),
        })]
    }

    /// Material descriptors provide the user-facing grouping while ordinary
    /// scene rows continue to use their stamped manifest section verbatim.
    pub(super) fn material_section_name(&self, row: &ParamRow) -> Option<String> {
        if self.material_object_gain(row) {
            return Some("Emission".to_string());
        }
        let Some(role) = row.spec.material_role else {
            return match row.spec.section.as_deref() {
                Some("Material") => Some("Advanced".to_string()),
                _ => row.spec.section.clone(),
            };
        };
        let name = match role {
            MaterialParamRole::Placement(family, ..) | MaterialParamRole::Sampler(family, ..) => {
                if self.material_family_connected(family) {
                    format!("Textures · {}", Self::material_family_label(family))
                } else {
                    "Advanced · Dormant Textures".to_string()
                }
            }
            MaterialParamRole::FeatureMode(feature) => match feature {
                crate::param_surface::MaterialFeature::Coat => "Coat".to_string(),
                crate::param_surface::MaterialFeature::Iridescence => "Iridescence".to_string(),
                crate::param_surface::MaterialFeature::Emission => "Emission".to_string(),
                crate::param_surface::MaterialFeature::Glass => "Glass".to_string(),
                crate::param_surface::MaterialFeature::Sheen => "Sheen".to_string(),
                crate::param_surface::MaterialFeature::Anisotropy => "Anisotropy".to_string(),
                crate::param_surface::MaterialFeature::Translucency => "Translucency".to_string(),
            },
            MaterialParamRole::Scalar(group) | MaterialParamRole::Colour(group, ..) => {
                match group {
                    MaterialGroup::Surface => "Surface".to_string(),
                    MaterialGroup::Opacity => "Opacity & Cutout".to_string(),
                    MaterialGroup::Feature(feature) => match feature {
                        crate::param_surface::MaterialFeature::Coat => "Coat".to_string(),
                        crate::param_surface::MaterialFeature::Iridescence => {
                            "Iridescence".to_string()
                        }
                        crate::param_surface::MaterialFeature::Emission => "Emission".to_string(),
                        crate::param_surface::MaterialFeature::Glass => "Glass".to_string(),
                        crate::param_surface::MaterialFeature::Sheen => "Sheen".to_string(),
                        crate::param_surface::MaterialFeature::Anisotropy => {
                            "Anisotropy".to_string()
                        }
                        crate::param_surface::MaterialFeature::Translucency => {
                            "Translucency".to_string()
                        }
                    },
                    MaterialGroup::Advanced => "Advanced".to_string(),
                }
            }
        };
        Some(name)
    }

    /// Match the descriptor's exact inner name when the app supplied the
    /// selected-material dictionary. The display-name fallback keeps older
    /// snapshots readable without using an id suffix as a write target.
    fn material_param_named(&self, row: &ParamRow, name: &str) -> bool {
        if let Some(info) = &self.active_material_info {
            if info
                .params
                .iter()
                .any(|(inner, id)| id == &row.id && inner == name)
            {
                return true;
            }
            if info.params.iter().any(|(_, id)| id == &row.id) {
                return false;
            }
        }
        row.spec
            .name
            .chars()
            .filter(|character| !character.is_ascii_whitespace() && *character != '-')
            .flat_map(char::to_lowercase)
            .eq(name.chars().filter(|character| *character != '_'))
    }

    pub(super) fn material_section_folded(&self, name: &str) -> bool {
        self.section_folded.get(name).copied().unwrap_or_else(|| {
            name == "Advanced"
                || name == "Advanced · Opacity"
                || name == "Advanced · Dormant Textures"
                || name.starts_with("Textures · ")
        })
    }

    pub(super) fn build_material_section_sources(
        &self,
        tree: &mut UITree,
        x: f32,
        width: f32,
        mut cy: f32,
        name: &str,
    ) -> f32 {
        let Some(info) = &self.active_material_info else {
            return cy;
        };
        let features = [
            crate::param_surface::MaterialFeature::Coat,
            crate::param_surface::MaterialFeature::Iridescence,
            crate::param_surface::MaterialFeature::Emission,
            crate::param_surface::MaterialFeature::Glass,
            crate::param_surface::MaterialFeature::Sheen,
            crate::param_surface::MaterialFeature::Anisotropy,
            crate::param_surface::MaterialFeature::Translucency,
        ];
        for texture in &info.textures {
            if !texture.connected || Self::material_family_for_port(&texture.port).is_some() {
                continue;
            }
            let belongs = features.iter().any(|feature| {
                Self::material_feature_label(*feature) == name
                    && Self::material_feature_map_for_port(*feature, &texture.port)
            });
            let advanced = name == "Advanced"
                && !features
                    .iter()
                    .any(|feature| Self::material_feature_map_for_port(*feature, &texture.port));
            if belongs || advanced {
                tree.add_label(
                    Some(self.content_parent),
                    x,
                    cy,
                    width,
                    ROW_H,
                    &format!("{} · {}", texture.label, texture.source_label),
                    label_style(),
                );
                cy += ROW_H;
            }
        }
        cy
    }

    fn material_family_label(family: MaterialMapFamily) -> &'static str {
        match family {
            MaterialMapFamily::Base => "Base Color",
            MaterialMapFamily::Normal => "Normal",
            MaterialMapFamily::MetallicRoughness => "Metallic / Roughness",
            MaterialMapFamily::Occlusion => "Occlusion",
            MaterialMapFamily::Emission => "Emission",
        }
    }

    fn material_family_for_port(port: &str) -> Option<MaterialMapFamily> {
        match port {
            "base_color_map" => Some(MaterialMapFamily::Base),
            "normal_map" => Some(MaterialMapFamily::Normal),
            "mr_map" | "metallic_roughness_map" => Some(MaterialMapFamily::MetallicRoughness),
            "occlusion_map" => Some(MaterialMapFamily::Occlusion),
            "emissive_map" => Some(MaterialMapFamily::Emission),
            _ => None,
        }
    }

    fn material_family_connected(&self, family: MaterialMapFamily) -> bool {
        self.active_material_info.as_ref().is_some_and(|info| {
            info.textures.iter().any(|texture| {
                texture.connected && Self::material_family_for_port(&texture.port) == Some(family)
            })
        })
    }

    fn material_feature_map_for_port(
        feature: crate::param_surface::MaterialFeature,
        port: &str,
    ) -> bool {
        match feature {
            crate::param_surface::MaterialFeature::Coat => matches!(
                port,
                "clearcoat_map" | "clearcoat_roughness_map" | "clearcoat_normal_map"
            ),
            crate::param_surface::MaterialFeature::Iridescence => {
                matches!(port, "iridescence_map" | "iridescence_thickness_map")
            }
            crate::param_surface::MaterialFeature::Emission => port == "emissive_map",
            crate::param_surface::MaterialFeature::Glass => {
                matches!(port, "transmission_map" | "volume_thickness_map")
            }
            crate::param_surface::MaterialFeature::Sheen => {
                matches!(port, "sheen_color_map" | "sheen_roughness_map")
            }
            crate::param_surface::MaterialFeature::Anisotropy => port == "anisotropy_map",
            crate::param_surface::MaterialFeature::Translucency => port == "volume_thickness_map",
        }
    }

    pub(super) fn material_object_gain(&self, row: &ParamRow) -> bool {
        self.active_material_info
            .as_ref()
            .is_some_and(|info| info.object_gain.as_ref() == Some(&row.id))
    }

    pub(super) fn material_param_selected(&self, row: &ParamRow) -> bool {
        row.spec.material_role.is_none()
            || self
                .active_material_info
                .as_ref()
                .is_none_or(|info| info.params.iter().any(|(_, id)| id == &row.id))
    }

    fn selected_material_rows(&self) -> Vec<ParamRow> {
        self.full_params
            .as_ref()
            .map(|surface| {
                surface
                    .rows
                    .iter()
                    .filter(|row| self.material_param_selected(row))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn material_panel_row_visible(row: &ParamRow) -> bool {
        !matches!(
            row.spec.material_role,
            Some(MaterialParamRole::Placement(..) | MaterialParamRole::Sampler(..))
        )
    }

    fn material_rgb_colour(row: &ParamRow) -> Option<crate::param_surface::MaterialColour> {
        match row.spec.material_role {
            Some(MaterialParamRole::Colour(_, colour, RgbChannel::R)) => Some(colour),
            _ => None,
        }
    }

    fn material_rgb_members_for_param(
        &self,
        param_id: &manifold_foundation::ParamId,
    ) -> Option<[manifold_foundation::ParamId; 3]> {
        self.properties_card.rows.iter().find_map(|row| {
            row.rgb_members
                .as_ref()
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

    pub(super) fn row_feature(row: &ParamRow) -> Option<crate::param_surface::MaterialFeature> {
        match row.spec.material_role {
            Some(MaterialParamRole::FeatureMode(feature))
            | Some(MaterialParamRole::Scalar(MaterialGroup::Feature(feature)))
            | Some(MaterialParamRole::Colour(MaterialGroup::Feature(feature), ..)) => Some(feature),
            _ => None,
        }
    }

    pub(super) fn material_bucket(&self, row: &ParamRow) -> usize {
        if let Some(feature) = self.material_row_feature(row) {
            return 2 + self.material_feature_order.iter().position(|candidate| *candidate == feature)
                .unwrap_or(self.material_feature_order.len() + feature as usize);
        }
        match row.spec.material_role {
            Some(MaterialParamRole::Scalar(MaterialGroup::Surface))
            | Some(MaterialParamRole::Colour(MaterialGroup::Surface, ..)) => 0,
            Some(MaterialParamRole::Scalar(MaterialGroup::Opacity))
            | Some(MaterialParamRole::Colour(MaterialGroup::Opacity, ..)) => 1,
            Some(MaterialParamRole::FeatureMode(feature))
            | Some(MaterialParamRole::Scalar(MaterialGroup::Feature(feature)))
            | Some(MaterialParamRole::Colour(MaterialGroup::Feature(feature), ..)) => {
                2 + feature as usize
            }
            Some(MaterialParamRole::Placement(family, ..))
            | Some(MaterialParamRole::Sampler(family, ..)) => {
                if self.material_family_connected(family) {
                    10 + family as usize
                } else {
                    30
                }
            }
            Some(MaterialParamRole::Scalar(MaterialGroup::Advanced))
            | Some(MaterialParamRole::Colour(MaterialGroup::Advanced, ..)) => 20,
            None if row.spec.section.as_deref() == Some("Material") => 20,
            None => usize::MAX,
        }
    }

    pub(super) fn material_feature_visible(
        &self,
        rows: &[ParamRow],
        feature: crate::param_surface::MaterialFeature,
    ) -> bool {
        let mode = rows.iter().find(|row| self.material_param_selected(row)
            && row.spec.material_role == Some(MaterialParamRole::FeatureMode(feature)))
            .map(|row| row.value.base.round() as i32);
        if mode == Some(3) {
            return false;
        }
        if matches!(mode, Some(1 | 2)) {
            return true;
        }
        if self.material_feature_context.is_some() {
            return self.material_visible_features.contains(&feature);
        }
        let authored = rows
            .iter()
            .filter(|row| self.material_param_selected(row))
            .filter(|row| Self::row_feature(row) == Some(feature))
            .any(|row| {
                // A wire, automation lane, or host mapping is an authored
                // attachment even when the current scalar happens to equal its
                // neutral default. Keep the feature visible so the attachment
                // remains reachable from the inspector.
                let attached = row.value.driven
                    || row.material_attached
                    || row.modulation.driver_active
                    || row.modulation.envelope_active
                    || row.modulation.automation_active
                    || row.audio.active
                    || row.mapping.ableton_display.is_some()
                    || row.mapping.ableton_range.is_some();
                attached
                    || (self.material_feature_is_controlling(row, feature)
                        && (row.value.base - row.spec.default).abs() > f32::EPSILON)
            });
        if authored {
            return true;
        }
        self.active_material_info.as_ref().is_some_and(|info| {
            info.textures.iter().any(|texture| {
                texture.connected && Self::material_feature_map_for_port(feature, &texture.port)
            })
        })
    }

    /// Secondary feature values remain editable after the feature is added,
    /// but they do not themselves make a neutral feature appear in the main
    /// surface. The mode and the feature's primary control are the authored
    /// presence signals; attachments on any member still force presence.
    fn material_feature_is_controlling(
        &self,
        row: &ParamRow,
        feature: crate::param_surface::MaterialFeature,
    ) -> bool {
        if matches!(
            row.spec.material_role,
            Some(MaterialParamRole::FeatureMode(mode)) if mode == feature
        ) {
            return true;
        }
        let Some(inner_name) = self.active_material_info.as_ref().and_then(|info| {
            info.params
                .iter()
                .find(|(_, id)| id == &row.id)
                .map(|(name, _)| name.as_str())
        }) else {
            return false;
        };
        match feature {
            crate::param_surface::MaterialFeature::Coat => inner_name == "clearcoat",
            crate::param_surface::MaterialFeature::Iridescence => inner_name == "iridescence",
            crate::param_surface::MaterialFeature::Emission => {
                matches!(
                    inner_name,
                    "emission_r" | "emission_g" | "emission_b" | "emission_intensity"
                )
            }
            crate::param_surface::MaterialFeature::Glass => inner_name == "transmission",
            crate::param_surface::MaterialFeature::Sheen => {
                matches!(
                    inner_name,
                    "sheen_color_r" | "sheen_color_g" | "sheen_color_b"
                )
            }
            crate::param_surface::MaterialFeature::Anisotropy => {
                inner_name == "anisotropy_strength"
            }
            crate::param_surface::MaterialFeature::Translucency => inner_name == "translucency",
        }
    }

    pub(super) fn material_feature_label(
        feature: crate::param_surface::MaterialFeature,
    ) -> &'static str {
        match feature {
            crate::param_surface::MaterialFeature::Coat => "Coat",
            crate::param_surface::MaterialFeature::Iridescence => "Iridescence",
            crate::param_surface::MaterialFeature::Emission => "Emission",
            crate::param_surface::MaterialFeature::Glass => "Glass",
            crate::param_surface::MaterialFeature::Sheen => "Sheen",
            crate::param_surface::MaterialFeature::Anisotropy => "Anisotropy",
            crate::param_surface::MaterialFeature::Translucency => "Translucency",
        }
    }

    fn material_feature_writes(
        &self,
        rows: &[ParamRow],
        feature: crate::param_surface::MaterialFeature,
        mode_id: &manifold_foundation::ParamId,
    ) -> Vec<MaterialParamWrite> {
        let seed_names: &[(&str, f32)] = match feature {
            crate::param_surface::MaterialFeature::Coat => &[("clearcoat", 1.0)],
            crate::param_surface::MaterialFeature::Iridescence => &[("iridescence", 1.0)],
            crate::param_surface::MaterialFeature::Emission => &[
                ("emission_r", 1.0),
                ("emission_g", 1.0),
                ("emission_b", 1.0),
                ("emission_intensity", 1.0),
            ],
            crate::param_surface::MaterialFeature::Glass => &[("transmission", 1.0)],
            crate::param_surface::MaterialFeature::Sheen => &[
                ("sheen_color_r", 0.5),
                ("sheen_color_g", 0.5),
                ("sheen_color_b", 0.5),
            ],
            crate::param_surface::MaterialFeature::Anisotropy => &[("anisotropy_strength", 0.5)],
            crate::param_surface::MaterialFeature::Translucency => &[("translucency", 0.5)],
        };
        let mut writes = vec![MaterialParamWrite {
            param_id: mode_id.clone(),
            value: 2.0,
        }];
        if rows.iter().any(|row| &row.id == mode_id && row.value.base.round() == 3.0) {
            return writes;
        }
        for &(name, value) in seed_names {
            let candidates: Vec<&ParamRow> = rows
                .iter()
                .filter(|row| {
                    Self::row_feature(row) == Some(feature)
                        && self.material_feature_is_controlling(row, feature)
                        && self.active_material_info.as_ref().is_some_and(|info| {
                            info.params
                                .iter()
                                .any(|(inner_name, id)| inner_name == name && id == &row.id)
                        })
                })
                .collect();
            let Some(row) = candidates
                .first()
                .copied()
                .filter(|_| candidates.len() == 1)
            else {
                continue;
            };
            let attached = row.value.driven
                || row.material_attached
                || row.modulation.driver_active
                || row.modulation.envelope_active
                || row.modulation.automation_active
                || row.audio.active
                || row.mapping.ableton_display.is_some()
                || row.mapping.ableton_range.is_some();
            if !attached && (row.value.base - row.spec.default).abs() <= f32::EPSILON {
                writes.push(MaterialParamWrite {
                    param_id: row.id.clone(),
                    value,
                });
            }
        }
        writes
    }

    pub(super) fn material_action_context(
        &self,
        object: &ModifierObjectRef,
        material: &ModifierObjectRef,
        require_shared_scope: bool,
    ) -> bool {
        !object.node.is_empty()
            && !material.node.is_empty()
            && self.active_material_info.as_ref().is_some_and(|info| {
                info.object == *object
                    && info.material == *material
                    && (!require_shared_scope || info.shared_object_count.is_some())
            })
    }

    /// One properties-card row, built through the SAME shared core every
    /// effect/generator card row uses — no synthesis, no `RowAddr`: `slot`
    /// indexes `self.properties_card.rows` directly, whose
    /// `id` IS the real exposed param — the dispatch identity every
    /// downstream `PanelAction` carries unchanged.
    pub(super) fn build_properties_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        cy: f32,
        slot: usize,
        label_width: f32,
        slider_w: f32,
        target: GraphParamTarget,
    ) -> f32 {
        // Scene-relative range substitution for translate params (SCENE_PANEL_UX_DESIGN.md).
        // When bounds are available, substitute the derived range (center ± 2×extent per axis)
        // so both slider drag clamp and type-in clamp see the same widened range.
        if let Some((bounds_min, bounds_max)) = self.state.as_live().and_then(|vm| vm.scene_bounds)
        {
            // Extract param ID string from the ParamRow's id field (Cow<'static, str>)
            let param_id = self.properties_card.rows[slot].id.as_ref();

            // Match transform_3d position params: pos_x, pos_y, pos_z
            if let Some(axis) = param_id
                .strip_prefix("pos_")
                .and_then(|suffix| match suffix {
                    "x" => Some(0usize),
                    "y" => Some(1usize),
                    "z" => Some(2usize),
                    _ => None,
                })
            {
                // Compute center and extent for this axis
                let center = (bounds_min[axis] + bounds_max[axis]) * 0.5;
                let mut extent = bounds_max[axis] - bounds_min[axis];

                // Floor extent at 1.0 so tiny scenes keep a usable range
                extent = extent.max(1.0);

                // Range is center ± 2×extent (per brief decision)
                let range_min = center - 2.0 * extent;
                let range_max = center + 2.0 * extent;

                self.properties_card.rows[slot].spec.min = range_min;
                self.properties_card.rows[slot].spec.max = range_max;
            }
        }
        let mut info = self.properties_card.rows[slot].clone();
        if info.spec.material_role.is_some() {
            info.spec.inactive_reason = None;
        }
        if self.material_object_gain(&info) {
            info.spec.name = "Object gain".into();
        }
        if self.material_param_named(&info, "alpha_mode") {
            info.spec.value_labels = Some(vec!["Solid".into(), "Cutout".into(), "Fade".into()]);
        }
        if let Some(MaterialParamRole::FeatureMode(feature)) = info.spec.material_role {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                label_width,
                ROW_H,
                "Enabled",
                label_style(),
            );
            let label = match info.value.base.round() as i32 {
                1 => "Off",
                2 => "On",
                _ => "Auto",
            };
            let id = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + label_width,
                cy,
                (slider_w - label_width).max(0.0),
                ROW_H,
                btn_style(),
                label,
                param_row_key_base(info.id.as_ref()) | ROW_ROLE_TOGGLE,
            );
            tree.set_name(id, format!("param_row.{}.value", info.id));
            self.properties_card.row_host.row_index.insert(
                tree.widget_of(id),
                slot,
                RowRole::MaterialFeatureToggle(feature),
            );
            self.material_mode_ids.push((id, slot));
            return cy + ROW_H + ROW_GAP;
        }
        let mut row_cy = cy;
        if info.rgb_members.is_some() && Self::material_rgb_colour(&info).is_some() {
            row_cy = self.build_material_swatch_header(
                tree,
                inner_x,
                cy,
                slot,
                label_width,
                slider_w,
                &info,
            );
        }

        // Boolean and trigger parameters use the shared button/dispatch path.
        if info.spec.is_toggle || info.spec.is_trigger {
            info.spec.default = self.properties_card.last_pushed_values.get(slot)
                .copied().filter(|value| !value.is_nan())
                .unwrap_or(self.properties_card.current_values[slot]);
            let row = build_toggle_trigger_row(
                tree,
                Some(self.content_parent),
                inner_x,
                row_cy,
                slider_w,
                &info,
                &self.properties_card.mod_state,
                slot,
                target,
                color::FONT_LABEL,
                true,
                false,
                Some(param_row_key_base(info.id.as_ref())),
                None,
            );
            let host = &mut self.properties_card.row_host;
            host.toggle_ids[slot] = Some(ToggleParamIds {
                label_id: row.label_id,
                button_id: row.button_id,
            });
            host.audio_btn_ids[slot] = row.audio_btn;
            host.audio_configs[slot] = row.audio_config;
            host.audio_trigger_mode_badge_ids[slot] = row.mode_badge_id;
            host.reindex_row(tree, slot);
            return row.new_cy;
        }

        // The value this row must SHOW: the sync's last-pushed value (the tree
        // is minted fresh every frame — a row the dirty-check skipped must
        // redraw that value here or it snaps back to the default). Never-
        // pushed (NaN) rows fall back to the synced base, which is what the
        // sync will compare against and push this same frame.
        let display_value = match self.properties_card.last_pushed_values.get(slot) {
            Some(&v) if !v.is_nan() => v,
            _ => self
                .properties_card
                .current_values
                .get(slot)
                .copied()
                .unwrap_or(info.spec.default),
        };
        let display_value = self
            .properties_card
            .row_host
            .active_param_value(&target, &info.id)
            .unwrap_or(display_value);

        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            row_cy,
            slider_w,
            &info,
            &self.properties_card.mod_state,
            slot,
            target,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true,
            label_width,
            false,
            self.properties_card
                .mod_active_tab
                .get(slot)
                .copied()
                .unwrap_or(ModTab::Driver),
            true,
            Some(param_row_key_base(info.id.as_ref())),
            None,
            Some(display_value),
        );
        let new_cy = built.new_cy;
        self.properties_card.row_host.install_row(tree, slot, built);

        new_cy
    }

    pub(super) fn material_full_value(&self, id: &manifold_foundation::ParamId) -> f32 {
        self.properties_card
            .row_id_index
            .get(id.as_ref())
            .and_then(|&slot| self.properties_card.current_values.get(slot).copied())
            .or_else(|| {
                self.full_params.as_ref().and_then(|surface| {
                    surface
                        .rows
                        .iter()
                        .find(|row| row.id == *id)
                        .map(|row| row.value.base)
                })
            })
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }

    pub(super) fn sync_material_swatches(&self, tree: &mut UITree) {
        for (node, _, ids, _) in &self.material_swatch_ids {
            let rgb: [u8; 3] = std::array::from_fn(|index| {
                (self.material_full_value(&ids[index]) * 255.0).round() as u8
            });
            let colour = Color32::new(rgb[0], rgb[1], rgb[2], 255); // design-token-exempt: displays authored material RGB, not a UI theme colour
            let Some(current) = tree.get_node(*node) else {
                continue;
            };
            if current.style.bg_color == colour {
                continue;
            }
            let mut style = current.style;
            style.bg_color = colour;
            style.text_color = if rgb.iter().map(|v| *v as u32).sum::<u32>() > 421 {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            tree.set_style(*node, style);
            tree.set_text(
                *node,
                &format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]),
            );
        }
    }

    fn build_material_swatch_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        cy: f32,
        slot: usize,
        label_width: f32,
        slider_w: f32,
        info: &ParamRow,
    ) -> f32 {
        let Some(rgb_members) = info.rgb_members.clone() else {
            return cy;
        };
        let Some(colour) = Self::material_rgb_colour(info) else {
            return cy;
        };
        let rgb = rgb_members.clone().map(|id| self.material_full_value(&id));
        let to_byte = |value: f32| (value * 255.0).round() as u8;
        let label = format!(
            "#{:02X}{:02X}{:02X}",
            to_byte(rgb[0]),
            to_byte(rgb[1]),
            to_byte(rgb[2])
        );
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            label_width,
            ROW_H,
            match colour {
                crate::param_surface::MaterialColour::Base => "Base colour",
                crate::param_surface::MaterialColour::Specular => "Specular tint",
                crate::param_surface::MaterialColour::Emission => "Emission colour",
                crate::param_surface::MaterialColour::Sheen => "Sheen colour",
                crate::param_surface::MaterialColour::Attenuation => "Attenuation colour",
            },
            label_style(),
        );
        let swatch_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + label_width,
            cy,
            (slider_w - label_width).max(0.0),
            ROW_H,
            UIStyle {
                bg_color: Color32::new(to_byte(rgb[0]), to_byte(rgb[1]), to_byte(rgb[2]), 255), // design-token-exempt: displays authored material RGB, not a UI theme colour
                text_color: if rgb.iter().copied().sum::<f32>() > 1.65 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                },
                font_size: color::FONT_LABEL,
                corner_radius: color::SMALL_RADIUS,
                ..drag_value_style()
            },
            &label,
            param_row_key_base(info.id.as_ref()) | MATERIAL_SWATCH_KEY_BASE,
        );
        tree.set_name(swatch_id, format!("material.swatch.{}", info.id));
        self.material_swatch_ids
            .push((swatch_id, slot, rgb_members, colour));
        self.properties_card.row_host.row_index.insert(
            tree.widget_of(swatch_id),
            slot,
            RowRole::ColourSwatch(colour),
        );
        cy + ROW_H + ROW_GAP
    }

    pub(super) fn build_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        selected: SceneSelection,
    ) -> f32 {
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Properties",
            section_label_style(),
        );
        cy += ROW_H;
        match selected {
            SceneSelection::Object(id) => {
                let Some(row) = vm.objects.iter().find_map(|o| match o {
                    ObjectRowVm::Known(r) if r.object_node_id == id => Some(r.as_ref()),
                    _ => None,
                }) else {
                    return cy;
                };
                cy = self.build_object_properties_header(tree, inner_x, inner_w, cy, row);
                self.build_object_properties_body(tree, inner_x, inner_w, cy, row)
            }
            SceneSelection::Light(id) => {
                let Some(row) = vm.lights.iter().find_map(|l| match l {
                    LightRowVm::Known(r) if r.node_doc_id == id => Some(r.as_ref()),
                    _ => None,
                }) else {
                    return cy;
                };
                cy = self.build_light_properties_header(tree, inner_x, inner_w, cy, row);
                self.build_light_properties_body(tree, inner_x, inner_w, cy, row)
            }
            SceneSelection::Camera => self.build_camera_section(tree, inner_x, inner_w, cy, vm),
            SceneSelection::World => self.build_world_properties(tree, inner_x, inner_w, cy, vm),
            SceneSelection::OutlinerFold(_) => cy, // Fold headers don't have properties
        }
    }

    /// Object properties header: editable name (click to rename — same
    /// single-click-opens-text-input UX the outliner/graph rename affordance
    /// already uses) + Duplicate + Remove (D11).
    fn build_object_properties_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        let btn_w = STEP_W * 4.0; // Frame + Duplicate + Remove
        let name_w = inner_w - btn_w - 8.0;
        let name_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            name_w,
            ROW_H,
            drag_value_style(),
            &row.name,
            obj_key(row.index, OBJ_OFF_NAME),
        );
        // Stable automation name (UX-P1): `scripts/ui-flows/` selects the
        // Properties header's name text by NAME, not raw text, so a flow can
        // assert "the header text changed" without hard-coding which object
        // it changed to.
        tree.set_name(name_id, "scene_setup.properties.name_value");
        let identity_node_id = row.group_node_id.unwrap_or(row.object_node_id);
        self.object_name_ids
            .push((identity_node_id, name_id, row.name.clone()));

        // Frame button (scene-panel-ux lane)
        let frame_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "Frame",
            obj_key(row.index, OBJ_OFF_FRAME),
        );
        self.object_frame_ids.push((frame_id, row.index));

        let dup_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0 + STEP_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{29C9}",
            obj_key(row.index, OBJ_OFF_REMOVE) + 1,
        );
        self.object_duplicate_ids.push((dup_id, row.index));
        let remove_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0 + STEP_W * 2.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2715}",
            obj_key(row.index, OBJ_OFF_REMOVE),
        );
        self.object_remove_ids.push((remove_id, row.index));
        cy + ROW_H + ROW_GAP
    }

    /// Object properties body: transform triplets, material quick knobs,
    /// modifier stack — the body `build_object_row` used to render only when
    /// expanded; now always rendered (there is no fold state left — the
    /// outliner IS the fold).
    /// P2 slice 2a: replaced the transform-triplet/material/metallic/
    /// roughness row builders with one `build_filtered_properties` pass over
    /// `row.sections` (Transform + Material + the object's own section +
    /// every modifier's own section — see `ObjectKnownRow::sections`'s doc
    /// comment). The modifier STACK below stays a structural verb (add/
    /// remove/reorder, unchanged) — only its per-modifier PARAM rows moved
    /// into the unified pass above (each modifier's section is already part
    /// of `row.sections`, so its rows render there, grouped under its own
    /// section header).
    fn build_object_properties_body(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        self.active_material_info = row.material_inspector.clone();
        if let Some(material) = row.material_inspector.clone() {
            let context = self
                .live_layer_id()
                .cloned()
                .map(|layer| (layer, material.object.clone(), material.material.clone()));
            if self.material_feature_context != context {
                self.material_feature_context = None;
                self.material_feature_order.clear();
                let rows = self.selected_material_rows();
                self.material_visible_features = rows
                    .iter()
                    .filter_map(Self::row_feature)
                    .filter(|feature| self.material_feature_visible(&rows, *feature))
                    .collect();
                self.material_feature_context = context;
            }
            self.sync_material_feature_order();
            cy = self.build_material_header(tree, inner_x, inner_w, cy, &material);
            cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections);
            cy = self.build_material_feature_actions(tree, inner_x, inner_w, cy, &material);
            cy = self.build_material_inspector(tree, inner_x, inner_w, cy, row, row.skin.as_ref());
        } else if let Some(skin) = &row.skin {
            // Non-PBR materials still expose their layer-skin control, but
            // there is no material drawer to host it.
            cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections);
            cy = self.build_skin_row(tree, inner_x, inner_w, cy, row, skin);
        } else {
            cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections);
        }
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Modifiers",
            label_style(),
        );
        cy += ROW_H;
        if row.modifiers_addable {
            for m in &row.modifiers {
                cy = self.build_modifier_stack_row(
                    tree,
                    inner_x,
                    inner_w,
                    cy,
                    row.index,
                    row.group_node_id.unwrap_or(row.object_node_id),
                    m,
                    row.modifiers.len(),
                );
            }
            cy = self.build_add_modifier_button(
                tree,
                inner_x,
                inner_w,
                cy,
                row.index,
                row.group_node_id.unwrap_or(row.object_node_id),
            );
        } else {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Custom chain — edit in graph",
                label_style(),
            );
            cy += ROW_H;
        }
        cy + ROW_GAP
    }

    /// Material-specific structural affordances that sit beside the ordinary
    /// manifest rows: scope notice, starter looks, and texture ownership.
    /// Numeric factors and placement remain in `build_filtered_properties` so
    /// they retain the shared row host, stable ids, and modulation drawers.
    fn build_material_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        info: &MaterialInspectorInfo,
    ) -> f32 {
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Material",
            section_label_style(),
        );
        cy += ROW_H;
        if let Some(count) = info.shared_object_count.filter(|count| *count > 1) {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                &format!("Shared · {count} objects"),
                label_style(),
            );
            cy += ROW_H;
        }
        if info.shared_object_count.is_some() {
            let looks = [
                (MaterialLook::Default, "Default", 4_u64),
                (MaterialLook::Matte, "Matte", 0_u64),
                (MaterialLook::Coated, "Coated", 1_u64),
                (MaterialLook::BrushedMetal, "Brushed Metal", 2_u64),
                (MaterialLook::Glass, "Glass", 3_u64),
            ];
            let gap = ROW_GAP;
            let min_button_w = looks.iter().map(|(_, label, _)| {
                tree.text_width(label, btn_style().font_size, crate::node::FontWeight::Regular) + 2.0 * GAP
            }).fold(0.0_f32, f32::max);
            let columns = (((inner_w + gap) / (min_button_w + gap)).floor() as usize).clamp(1, looks.len());
            let button_w = ((inner_w - gap * (columns - 1) as f32) / columns as f32).max(0.0);
            for (position, (look, label, offset)) in looks.into_iter().enumerate() {
                let id = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x + (position % columns) as f32 * (button_w + gap),
                    cy + (position / columns) as f32 * (ROW_H + gap),
                    button_w,
                    ROW_H,
                    btn_style(),
                    label,
                    MATERIAL_LOOK_KEY_BASE + offset,
                );
                self.material_look_ids
                    .push((id, look, info.object.clone(), info.material.clone()));
            }
            cy += (looks.len().div_ceil(columns) - 1) as f32 * (ROW_H + gap);
        } else {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Shared scope unknown",
                label_style(),
            );
        }
        cy + ROW_H + ROW_GAP
    }

    fn build_material_feature_actions(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        info: &MaterialInspectorInfo,
    ) -> f32 {
        let feature_rows = self.selected_material_rows();
        const FEATURES: [crate::param_surface::MaterialFeature; 7] = [
            crate::param_surface::MaterialFeature::Coat,
            crate::param_surface::MaterialFeature::Iridescence,
            crate::param_surface::MaterialFeature::Emission,
            crate::param_surface::MaterialFeature::Glass,
            crate::param_surface::MaterialFeature::Sheen,
            crate::param_surface::MaterialFeature::Anisotropy,
            crate::param_surface::MaterialFeature::Translucency,
        ];
        let mut add_features = Vec::new();
        for feature in FEATURES {
            if self.material_feature_visible(&feature_rows, feature) {
                continue;
            }
            let Some(mode_row) = feature_rows.iter().find(|row| {
                row.spec.material_role == Some(MaterialParamRole::FeatureMode(feature))
            }) else {
                continue;
            };
            add_features.push((feature, mode_row.id.clone()));
        }
        if add_features.is_empty() {
            return cy;
        }
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Optional Features",
            section_label_style(),
        );
        cy += ROW_H;
        let button_w = ((inner_w - ROW_GAP * 2.0) / 3.0).max(0.0);
        let feature_count = add_features.len();
        for (index, (feature, mode_id)) in add_features.into_iter().enumerate() {
            let col = (index % 3) as f32;
            let row = (index / 3) as f32;
            let label = format!("+ Add {}", Self::material_feature_label(feature));
            let id = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + col * (button_w + ROW_GAP),
                cy + row * (ROW_H + ROW_GAP),
                button_w,
                ROW_H,
                btn_style(),
                &label,
                MATERIAL_LOOK_KEY_BASE + 32 + feature as u64,
            );
            let writes = self.material_feature_writes(&feature_rows, feature, &mode_id);
            self.material_feature_ids.push((
                id,
                feature,
                writes,
                info.object.clone(),
                info.material.clone(),
            ));
        }
        cy + feature_count.div_ceil(3) as f32 * (ROW_H + ROW_GAP)
    }

    fn build_material_inspector(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        object_row: &ObjectKnownRow,
        skin: Option<&SkinRowVm>,
    ) -> f32 {
        if let Some(skin) = skin {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Textures",
                section_label_style(),
            );
            cy += ROW_H;
            cy = self.build_skin_row(tree, inner_x, inner_w, cy, object_row, skin);
        }
        cy + ROW_GAP
    }

    /// P4b: one Skin row per Known object — source layer dropdown + target-map
    /// dropdown. Each half is a clickable button (not a bare label) so the
    /// affordance rule is met. A missing source layer shows a trailing chip.
    fn build_skin_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        row: &ObjectKnownRow,
        skin: &SkinRowVm,
    ) -> f32 {
        let label_w = crate::slider::label_width_for_row(inner_w);
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            label_w,
            ROW_H,
            "Skin",
            label_style(),
        );
        let btn_gap = 4.0f32;
        let remaining = (inner_w - label_w).max(0.0);
        let chip_w = if skin.source_missing { 80.0f32 } else { 0.0f32 };
        let chip_gap = if skin.source_missing { btn_gap } else { 0.0f32 };
        let btn_w = ((remaining - chip_w - chip_gap - btn_gap) / 2.0).max(0.0);
        let source_label = skin
            .source
            .as_ref()
            .and_then(|id| {
                skin.source_options
                    .iter()
                    .find(|(lid, _)| lid == id)
                    .map(|(_, name)| name.clone())
            })
            .unwrap_or_else(|| "None".to_string());
        let source_btn = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + label_w,
            cy,
            btn_w,
            ROW_H,
            btn_style(),
            &format!("Source: {source_label}"),
            obj_key(row.index, OBJ_OFF_SKIN_SOURCE),
        );
        tree.set_name(source_btn, "scene_setup.skin.source");
        self.skin_source_ids
            .push((source_btn, row.object_node_id, skin.clone()));
        let target_btn = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + label_w + btn_w + btn_gap,
            cy,
            btn_w,
            ROW_H,
            btn_style(),
            &format!("Map: {}", skin.target_map.label()),
            obj_key(row.index, OBJ_OFF_SKIN_TARGET),
        );
        tree.set_name(target_btn, "scene_setup.skin.target");
        self.skin_target_ids
            .push((target_btn, row.object_node_id, skin.clone()));
        if skin.source_missing {
            tree.add_label(
                Some(self.content_parent),
                inner_x + label_w + btn_w * 2.0 + btn_gap * 2.0,
                cy,
                chip_w,
                ROW_H,
                "missing layer",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
        }
        cy + ROW_H + ROW_GAP
    }

    pub(super) fn properties_row_action(
        &mut self,
        row: usize,
        role: RowRole,
        node: NodeId,
        target: GraphParamTarget,
    ) -> Vec<PanelAction> {
        if let RowRole::MaterialFeatureToggle(feature) = role {
            let Some(info) = self.active_material_info.as_ref() else {
                return Vec::new();
            };
            if !self.material_action_context(&info.object, &info.material, false) {
                return Vec::new();
            }
            let Some(param) = self.properties_card.rows.get(row) else {
                return Vec::new();
            };
            let current = self
                .properties_card
                .current_values
                .get(row)
                .copied()
                .unwrap_or(param.value.base)
                .round() as i32;
            let next = match current {
                0 => 1, // FollowValues → explicit Off
                1 => 2, // Off → explicit On
                _ => 1, // On → explicit Off; authored factors remain intact
            } as f32;
            return vec![PanelAction::Project(ProjectAction::MaterialParamsSet {
                target,
                object: info.object.clone(),
                material: info.material.clone(),
                kind: MaterialEditKind::Feature,
                writes: vec![MaterialParamWrite {
                    param_id: param.id.clone(),
                    value: next,
                }],
                description: format!("Set {} mode", Self::material_feature_label(feature)),
            })];
        }
        let card = &mut self.properties_card;
        let mut copied_flash = CopyToClipboardLabelState::default();
        card.row_host.row_action(
            target,
            row,
            role,
            node,
            &card.rows,
            &card.current_values,
            &card.osc_addresses,
            &mut card.mod_state,
            &mut card.mod_active_tab,
            &mut copied_flash,
            &mut self.section_folded,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material_test_row(id: &str, role: MaterialParamRole, base: f32, default: f32) -> ParamRow {
        let mut row = placeholder_param_info();
        row.id = manifold_foundation::ParamId::from(id.to_string());
        row.spec.name = id.to_string();
        row.spec.section = Some("Material".to_string());
        row.spec.material_role = Some(role);
        row.spec.default = default;
        row.value.base = base;
        row.value.effective = base;
        row
    }

    #[test]
    fn material_inspector_buckets_are_unique_and_ordered() {
        let rows = [
            material_test_row(
                "advanced",
                MaterialParamRole::Scalar(MaterialGroup::Advanced),
                0.0,
                0.0,
            ),
            material_test_row(
                "coat",
                MaterialParamRole::Scalar(MaterialGroup::Feature(
                    crate::param_surface::MaterialFeature::Coat,
                )),
                0.0,
                0.0,
            ),
            material_test_row(
                "opacity",
                MaterialParamRole::Scalar(MaterialGroup::Opacity),
                1.0,
                1.0,
            ),
            material_test_row(
                "surface",
                MaterialParamRole::Scalar(MaterialGroup::Surface),
                0.5,
                0.5,
            ),
        ];
        let panel = ScenePanel::new();
        let mut indices = [0usize, 1, 2, 3];
        indices.sort_by_key(|&index| panel.material_bucket(&rows[index]));
        assert_eq!(
            indices,
            [3, 2, 1, 0],
            "surface, opacity, feature, advanced order"
        );
        let names: Vec<String> = indices
            .iter()
            .map(|&index| panel.material_section_name(&rows[index]).unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["Surface", "Opacity & Cutout", "Coat", "Advanced"]
        );
        assert!(names.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn material_inspector_feature_mode_emits_scoped_enum_batch() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mode = material_test_row(
            "51_coat_mode",
            MaterialParamRole::FeatureMode(feature),
            0.0,
            0.0,
        );
        let mut panel = ScenePanel::new();
        panel.properties_card.rows = vec![mode];
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("object"),
            },
            material: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("material"),
            },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![(
                "coat_mode".into(),
                manifold_foundation::ParamId::from("51_coat_mode".to_string()),
            )],
        });
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        let actions = panel.properties_row_action(
            0,
            RowRole::MaterialFeatureToggle(feature),
            NodeId::PLACEHOLDER,
            target,
        );
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::MaterialParamsSet { kind: MaterialEditKind::Feature, writes, .. })]
                if writes.len() == 1 && writes[0].param_id.as_ref() == "51_coat_mode" && writes[0].value == 1.0
        ));
        let actions = panel.material_remove_action(feature, &"51_coat_mode".into(),
            GraphParamTarget::GeneratorOf(LayerId::new("layer")));
        assert!(matches!(actions.as_slice(),
            [PanelAction::Project(ProjectAction::MaterialParamsSet { kind: MaterialEditKind::Feature, object, material, writes, .. })]
                if object.node.as_str() == "object" && material.node.as_str() == "material"
                    && writes.len() == 1 && writes[0].param_id.as_ref() == "51_coat_mode" && writes[0].value == 3.0
        ));
    }

    #[test]
    fn material_inspector_secondary_values_stay_under_add_feature() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mode = material_test_row(
            "51_coat_mode",
            MaterialParamRole::FeatureMode(feature),
            0.0,
            0.0,
        );
        let secondary = material_test_row(
            "51_clearcoat_roughness",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.9,
            0.0,
        );
        let panel = ScenePanel::new();
        assert!(
            !panel.material_feature_visible(&[mode, secondary], feature),
            "secondary authored values must remain reachable through Add Feature"
        );
    }

    #[test]
    fn material_inspector_factor_drag_cannot_change_feature_presence() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut panel = ScenePanel::new();
        panel.material_feature_context = Some((
            LayerId::new("layer"),
            ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("object"),
            },
            ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("material"),
            },
        ));
        let mut row = material_test_row(
            "51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        assert!(!panel.material_feature_visible(&[row.clone()], feature));
        row.value.base = 1.0;
        row.value.effective = 1.0;
        assert!(!panel.material_feature_visible(&[row.clone()], feature));
        panel.material_visible_features.insert(feature);
        row.value.base = 0.0;
        row.value.effective = 0.0;
        assert!(panel.material_feature_visible(&[row], feature));
    }

    #[test]
    fn material_inspector_removed_feature_stays_hidden_and_readds_without_reseeding() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mode = material_test_row("51_coat_mode", MaterialParamRole::FeatureMode(feature), 3.0, 0.0);
        let factor = material_test_row("51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)), 0.0, 0.0);
        let mut panel = ScenePanel::new();
        panel.material_visible_features.insert(feature);
        let rows = vec![mode.clone(), factor];
        assert!(!panel.material_feature_visible(&rows, feature), "Removed overrides retained section presence");
        let writes = panel.material_feature_writes(&rows, feature, &mode.id);
        assert_eq!(writes.len(), 1, "re-adding must preserve even a zero-valued factor");
        assert_eq!(writes[0].param_id, mode.id);
        assert_eq!(writes[0].value, 2.0);
        let mut restored = rows;
        restored[0].value.base = 2.0;
        assert!(panel.material_feature_visible(&restored, feature), "Undo removal restores the section");
    }

    #[test]
    fn material_inspector_new_features_follow_existing_sections() {
        use crate::param_surface::MaterialFeature;
        let mut panel = ScenePanel::new();
        panel.material_feature_order = vec![MaterialFeature::Glass, MaterialFeature::Coat];
        let glass = material_test_row("51_glass_mode", MaterialParamRole::FeatureMode(MaterialFeature::Glass), 2.0, 0.0);
        let coat = material_test_row("51_coat_mode", MaterialParamRole::FeatureMode(MaterialFeature::Coat), 2.0, 0.0);
        assert!(panel.material_bucket(&glass) < panel.material_bucket(&coat));
    }

    #[test]
    fn material_inspector_rgb_sliders_preview_before_release_and_sync_hex() {
        let mut panel = ScenePanel::new();
        panel.properties_card.resize(3);
        let ids =
            ["51_color_r", "51_color_g", "51_color_b"].map(manifold_foundation::ParamId::from);
        for (index, channel) in [RgbChannel::R, RgbChannel::G, RgbChannel::B]
            .into_iter()
            .enumerate()
        {
            let mut row = material_test_row(
                ids[index].as_ref(),
                MaterialParamRole::Colour(
                    MaterialGroup::Surface,
                    crate::param_surface::MaterialColour::Base,
                    channel,
                ),
                1.0,
                1.0,
            );
            row.spec.min = 0.0;
            row.spec.max = 1.0;
            if index == 0 {
                row.rgb_members = Some(ids.clone());
            }
            panel
                .properties_card
                .row_id_index
                .insert(row.id.to_string(), index);
            panel.properties_card.rows[index] = row;
            panel.properties_card.current_values[index] = 1.0;
        }
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        let mut tree = UITree::new();
        panel.content_parent = tree.add_panel(None, 0.0, 0.0, 400.0, 800.0, UIStyle::default());
        let mut y = 0.0;
        for slot in 0..3 {
            y = panel.build_properties_row(&mut tree, 0.0, y, slot, 100.0, 380.0, target.clone());
        }
        assert!(
            panel
                .properties_card
                .row_host
                .slider_ids
                .iter()
                .all(Option::is_some)
        );
        assert_eq!(panel.material_swatch_ids.len(), 1);
        let track = panel.properties_card.row_host.slider_ids[0]
            .as_ref()
            .unwrap()
            .track;
        let rect = tree.get_bounds(track);
        let down = panel.properties_card.handle_pointer_down(
            track,
            Vec2::new(rect.x + rect.width, rect.y),
            &mut tree,
            &target,
        );
        let down = panel.rewrite_material_rgb_actions(down);
        assert!(matches!(
            down.first(),
            Some(PanelAction::Scrub(
                ValueRef::ParamRgb(..),
                ScrubPhase::Begin
            ))
        ));
        let moved =
            panel
                .properties_card
                .handle_drag(Vec2::new(rect.x, rect.y), &mut tree, false, &target);
        let moved = panel.rewrite_material_rgb_actions(moved);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::ParamRgb(_, members), ScrubPhase::Move(ScrubValue::Rgb(rgb)))] if members == &ids && rgb[0] < 1.0 && rgb[1..] == [1.0, 1.0])
        );
        panel.sync_material_swatches(&mut tree);
        let hex = tree
            .get_node(panel.material_swatch_ids[0].0)
            .unwrap()
            .text
            .as_deref()
            .unwrap();
        assert_ne!(hex, "#FFFFFF");
        assert!(hex.ends_with("FFFF"));
        let release = panel.properties_card.row_host.handle_drag_end();
        assert!(matches!(
            panel.rewrite_material_rgb_actions(release).as_slice(),
            [PanelAction::Scrub(
                ValueRef::ParamRgb(..),
                ScrubPhase::Commit
            )]
        ));
    }

    #[test]
    fn material_inspector_mode_is_an_explicit_control_without_a_slider() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut panel = ScenePanel::new();
        panel.properties_card.resize(1);
        panel.properties_card.rows[0] = material_test_row(
            "51_coat_mode",
            MaterialParamRole::FeatureMode(feature),
            0.0,
            0.0,
        );
        let mut tree = UITree::new();
        panel.content_parent = tree.add_panel(None, 0.0, 0.0, 400.0, 800.0, UIStyle::default());
        panel.build_properties_row(
            &mut tree,
            0.0,
            0.0,
            0,
            100.0,
            380.0,
            GraphParamTarget::GeneratorOf(LayerId::new("layer")),
        );
        assert!(panel.properties_card.row_host.slider_ids[0].is_none());
        assert_eq!(panel.material_mode_ids.len(), 1);
        assert_eq!(
            tree.get_node(panel.material_mode_ids[0].0)
                .unwrap()
                .text
                .as_deref(),
            Some("Auto")
        );
    }

    #[test]
    fn material_inspector_mapping_keeps_neutral_feature_visible() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut row = material_test_row(
            "51_clearcoat_roughness",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        row.mapping.ableton_range = Some((0.0, 1.0));
        let panel = ScenePanel::new();
        assert!(panel.material_feature_visible(&[row], feature));
    }

    #[test]
    fn material_inspector_authored_feature_survives_effective_zero() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut row = material_test_row(
            "51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.75,
            0.0,
        );
        row.value.effective = 0.0;
        let mut panel = ScenePanel::new();
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("object"),
            },
            material: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("material"),
            },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![("clearcoat".into(), row.id.clone())],
        });
        assert!(
            panel.material_feature_visible(&[row], feature),
            "authored feature membership must use base, not the modulated frame value"
        );
    }

    #[test]
    fn material_inspector_external_attachment_survives_dormant_flags() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut row = material_test_row(
            "51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        row.material_attached = true;
        let panel = ScenePanel::new();
        assert!(panel.material_feature_visible(&[row], feature));
    }

    #[test]
    fn material_inspector_controlling_factor_uses_exact_inner_binding() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let custom_id = manifold_foundation::ParamId::from("outer_custom_factor".to_string());
        let mut row = material_test_row(
            custom_id.as_ref(),
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.6,
            0.0,
        );
        row.id = custom_id.clone();
        let mut panel = ScenePanel::new();
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("object"),
            },
            material: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("material"),
            },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![("clearcoat".into(), custom_id)],
        });
        assert!(panel.material_feature_visible(&[row], feature));
    }

    #[test]
    fn material_inspector_rgb_lifecycle_preserves_other_channels() {
        let r = manifold_foundation::ParamId::from("51_base_color_r".to_string());
        let g = manifold_foundation::ParamId::from("51_base_color_g".to_string());
        let b = manifold_foundation::ParamId::from("51_base_color_b".to_string());
        let mut panel = ScenePanel::new();
        let mut red = material_test_row(
            "51_base_color_r",
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Base,
                RgbChannel::R,
            ),
            0.2,
            0.0,
        );
        red.rgb_members = Some([r.clone(), g.clone(), b.clone()]);
        let green = material_test_row(
            "51_base_color_g",
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Base,
                RgbChannel::G,
            ),
            0.4,
            0.0,
        );
        let blue = material_test_row(
            "51_base_color_b",
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Base,
                RgbChannel::B,
            ),
            0.6,
            0.0,
        );
        panel.properties_card.rows = vec![red, green, blue];
        panel.properties_card.current_values = vec![0.2, 0.4, 0.6];
        panel.properties_card.row_id_index.extend([
            (r.to_string(), 0),
            (g.to_string(), 1),
            (b.to_string(), 2),
        ]);
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        let actions = panel.rewrite_material_rgb_actions(vec![
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), g.clone()),
                ScrubPhase::Begin,
            ),
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), g.clone()),
                ScrubPhase::Move(ScrubValue::Scalar(0.8)),
            ),
            PanelAction::Scrub(ValueRef::Param(target, g.clone()), ScrubPhase::Commit),
        ]);
        assert!(
            matches!(&actions[0], PanelAction::Scrub(ValueRef::ParamRgb(_, ids), ScrubPhase::Begin) if ids == &[r.clone(), g.clone(), b.clone()])
        );
        assert!(matches!(
            actions[1],
            PanelAction::Scrub(
                ValueRef::ParamRgb(_, _),
                ScrubPhase::Move(ScrubValue::Rgb([0.2, 0.8, 0.6]))
            )
        ));
        assert!(matches!(
            actions[2],
            PanelAction::Scrub(ValueRef::ParamRgb(_, _), ScrubPhase::Commit)
        ));
    }

    #[test]
    fn material_inspector_rgb_uses_nonprimary_colour_and_scalar_fallback() {
        let mut row = material_test_row(
            "51_specular_r",
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Specular,
                RgbChannel::R,
            ),
            0.5,
            0.0,
        );
        row.value.driven = true;
        assert_eq!(
            ScenePanel::material_rgb_colour(&row),
            Some(crate::param_surface::MaterialColour::Specular)
        );
        let mut panel = ScenePanel::new();
        panel.properties_card.rows = vec![row.clone()];
        panel.properties_card.current_values = vec![0.5];
        panel
            .properties_card
            .row_id_index
            .insert(row.id.to_string(), 0);
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        let action = PanelAction::Scrub(
            ValueRef::Param(target, row.id.clone()),
            ScrubPhase::Move(ScrubValue::Scalar(0.7)),
        );
        let actions = panel.rewrite_material_rgb_actions(vec![action.clone()]);
        assert!(
            matches!(actions.as_slice(), [PanelAction::Scrub(ValueRef::Param(_, id), ScrubPhase::Move(ScrubValue::Scalar(0.7)))] if id == &row.id)
        );
    }

    #[test]
    fn material_inspector_uv_and_sampling_stay_out_of_panel() {
        for role in [
            MaterialParamRole::Placement(
                MaterialMapFamily::Base,
                crate::param_surface::UvComponent::M00,
            ),
            MaterialParamRole::Sampler(
                MaterialMapFamily::Normal,
                crate::param_surface::SamplerComponent::WrapU,
            ),
        ] {
            assert!(!ScenePanel::material_panel_row_visible(&material_test_row(
                "uv", role, 0.0, 0.0
            )));
        }
        assert!(ScenePanel::material_panel_row_visible(&material_test_row(
            "r",
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Base,
                RgbChannel::R
            ),
            0.0,
            0.0
        )));
    }

    #[test]
    fn material_inspector_seed_writes_follow_selected_material_mapping() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mode_51 = material_test_row(
            "51_coat_mode",
            MaterialParamRole::FeatureMode(feature),
            0.0,
            0.0,
        );
        let mut seed_51 = material_test_row(
            "51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        seed_51.spec.name = "Clearcoat".into();
        seed_51.mapping.osc_address = Some("/material/clearcoat".into());
        let mode_52 = material_test_row(
            "52_coat_mode",
            MaterialParamRole::FeatureMode(feature),
            0.0,
            0.0,
        );
        let seed_52 = material_test_row(
            "52_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        let mut panel = ScenePanel::new();
        panel.properties_card.rows = vec![mode_51.clone(), seed_51.clone(), mode_52, seed_52];
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("object"),
            },
            material: ModifierObjectRef {
                scope: Vec::new(),
                node: manifold_foundation::NodeId::new("material"),
            },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![
                ("coat_mode".into(), mode_51.id.clone()),
                ("clearcoat".into(), seed_51.id.clone()),
            ],
        });
        let writes =
            panel.material_feature_writes(&panel.properties_card.rows, feature, &mode_51.id);
        assert!(
            writes
                .iter()
                .any(|write| write.param_id == mode_51.id && write.value == 2.0)
        );
        assert!(
            writes
                .iter()
                .any(|write| write.param_id == seed_51.id && write.value == 1.0)
        );
        assert!(
            !writes
                .iter()
                .any(|write| write.param_id.as_ref() == "52_clearcoat")
        );
    }

    #[test]
    fn material_inspector_collapsed_rgb_uses_full_surface_channels() {
        let (_, mut surface) = super::super::tests::world_transform_vm();
        let mut red = surface.rows[0].clone();
        red.id = manifold_foundation::ParamId::from("51_base_color_r".to_string());
        red.value.base = 0.2;
        red.spec.material_role = Some(MaterialParamRole::Colour(
            MaterialGroup::Surface,
            crate::param_surface::MaterialColour::Base,
            RgbChannel::R,
        ));
        let mut green = red.clone();
        green.id = manifold_foundation::ParamId::from("51_base_color_g".to_string());
        green.value.base = 0.4;
        green.spec.material_role = Some(MaterialParamRole::Colour(
            MaterialGroup::Surface,
            crate::param_surface::MaterialColour::Base,
            RgbChannel::G,
        ));
        let mut blue = red.clone();
        blue.id = manifold_foundation::ParamId::from("51_base_color_b".to_string());
        blue.value.base = 0.6;
        blue.spec.material_role = Some(MaterialParamRole::Colour(
            MaterialGroup::Surface,
            crate::param_surface::MaterialColour::Base,
            RgbChannel::B,
        ));
        red.rgb_members = Some([red.id.clone(), green.id.clone(), blue.id.clone()]);
        surface.rows = vec![red.clone(), green, blue];
        let mut panel = ScenePanel::new();
        panel.full_params = Some(surface);
        panel.properties_card.rows = vec![red.clone()];
        panel.properties_card.current_values = vec![0.2];
        panel
            .properties_card
            .row_id_index
            .insert(red.id.to_string(), 0);
        assert_eq!(
            panel.material_full_value(&manifold_foundation::ParamId::from(
                "51_base_color_g".to_string()
            )),
            0.4
        );
        assert_eq!(
            panel.material_full_value(&manifold_foundation::ParamId::from(
                "51_base_color_b".to_string()
            )),
            0.6
        );
    }

    #[test]
    fn material_inspector_rgb_channels_are_always_reachable() {
        let r = manifold_foundation::ParamId::from("51_base_color_r".to_string());
        let g = manifold_foundation::ParamId::from("51_base_color_g".to_string());
        let b = manifold_foundation::ParamId::from("51_base_color_b".to_string());
        let mut primary = material_test_row(
            r.as_ref(),
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Base,
                RgbChannel::R,
            ),
            0.2,
            0.0,
        );
        primary.rgb_members = Some([r.clone(), g.clone(), b.clone()]);
        let green = material_test_row(
            g.as_ref(),
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Base,
                RgbChannel::G,
            ),
            0.4,
            0.0,
        );
        let blue = material_test_row(
            b.as_ref(),
            MaterialParamRole::Colour(
                MaterialGroup::Surface,
                crate::param_surface::MaterialColour::Base,
                RgbChannel::B,
            ),
            0.6,
            0.0,
        );
        let rows = [primary.clone(), green, blue];
        assert!(ScenePanel::material_panel_row_visible(&primary));
        assert!(ScenePanel::material_panel_row_visible(&rows[1]));
        assert!(ScenePanel::material_panel_row_visible(&rows[2]));
    }
}
