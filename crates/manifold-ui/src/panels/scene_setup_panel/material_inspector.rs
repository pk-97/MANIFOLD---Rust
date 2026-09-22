//! Material inspector grouping, controls, placement, and focused tests.

use super::*;

impl ScenePanel {
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
        // Keep the stored cutoff reachable without presenting it as active
        // opacity authoring in Solid/Fade. Its value remains intact in the
        // Advanced drawer until Cutout is selected.
        if self.material_param_named(row, "alpha_cutoff") && self.material_opacity_mode() != Some(1)
        {
            return Some("Advanced · Opacity".to_string());
        }
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

    fn material_value(&self, id: &manifold_foundation::ParamId) -> Option<f32> {
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
                        .map(|row| row.value.effective)
                })
            })
    }

    fn material_opacity_mode(&self) -> Option<i32> {
        self.full_params.as_ref()?.rows.iter().find_map(|row| {
            (row.spec.material_role == Some(MaterialParamRole::Scalar(MaterialGroup::Opacity))
                && self.material_param_named(row, "alpha_mode"))
            .then(|| {
                self.material_value(&row.id)
                    .unwrap_or(row.value.effective)
                    .round() as i32
            })
        })
    }

    pub(super) fn material_section_folded(&self, name: &str) -> bool {
        self.section_folded.get(name).copied().unwrap_or_else(|| {
            name == "Advanced"
                || name == "Advanced · Opacity"
                || name == "Advanced · Dormant Textures"
                || name.starts_with("Textures · ")
        })
    }

    pub(super) fn material_section_display_name(&self, name: &str) -> String {
        if let Some(family) = Self::material_family_from_section(name) {
            return format!(
                "Advanced · {} UV & sampling",
                Self::material_family_label(family)
            );
        }
        name.to_string()
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
                    &format!("{} · {} · mesh UV", texture.label, texture.source_label),
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

    fn material_family_index(family: MaterialMapFamily) -> usize {
        match family {
            MaterialMapFamily::Base => 0,
            MaterialMapFamily::Normal => 1,
            MaterialMapFamily::MetallicRoughness => 2,
            MaterialMapFamily::Occlusion => 3,
            MaterialMapFamily::Emission => 4,
        }
    }

    fn material_placement_rows(&self, family: MaterialMapFamily) -> Option<[ParamRow; 6]> {
        let surface = self.full_params.as_ref()?;
        let components = [
            UvComponent::M00,
            UvComponent::M01,
            UvComponent::M10,
            UvComponent::M11,
            UvComponent::Tx,
            UvComponent::Ty,
        ];
        let mut found: [Option<ParamRow>; 6] = std::array::from_fn(|_| None);
        for row in surface
            .rows
            .iter()
            .filter(|row| self.material_param_selected(row))
        {
            let Some(MaterialParamRole::Placement(row_family, component)) = row.spec.material_role
            else {
                continue;
            };
            if row_family != family {
                continue;
            }
            let Some(index) = components
                .iter()
                .position(|candidate| *candidate == component)
            else {
                continue;
            };
            if found[index].is_none() {
                found[index] = Some(row.clone());
            }
        }
        let [
            Some(m00),
            Some(m01),
            Some(m10),
            Some(m11),
            Some(tx),
            Some(ty),
        ] = found
        else {
            return None;
        };
        Some([m00, m01, m10, m11, tx, ty])
    }

    fn configure_material_placements(&mut self, info: &MaterialInspectorInfo) {
        let Some(target) = self
            .live_layer_id()
            .cloned()
            .map(GraphParamTarget::GeneratorOf)
        else {
            return;
        };
        for family in [
            MaterialMapFamily::Base,
            MaterialMapFamily::Normal,
            MaterialMapFamily::MetallicRoughness,
            MaterialMapFamily::Occlusion,
            MaterialMapFamily::Emission,
        ] {
            let index = Self::material_family_index(family);
            if !self.material_family_connected(family) {
                continue;
            }
            let Some(rows) = self.material_placement_rows(family) else {
                continue;
            };
            let refs = [&rows[0], &rows[1], &rows[2], &rows[3], &rows[4], &rows[5]];
            self.material_placement_widgets[index].configure(
                target.clone(),
                info.object.clone(),
                info.material.clone(),
                refs,
            );
            self.material_placement_active[index] = true;
        }
    }

    pub(super) fn material_family_from_section(name: &str) -> Option<MaterialMapFamily> {
        match name {
            "Textures · Base Color" => Some(MaterialMapFamily::Base),
            "Textures · Normal" => Some(MaterialMapFamily::Normal),
            "Textures · Metallic / Roughness" => Some(MaterialMapFamily::MetallicRoughness),
            "Textures · Occlusion" => Some(MaterialMapFamily::Occlusion),
            "Textures · Emission" => Some(MaterialMapFamily::Emission),
            _ => None,
        }
    }

    pub(super) fn build_material_placement(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        family: MaterialMapFamily,
    ) -> f32 {
        let index = Self::material_family_index(family);
        if !self.material_placement_active[index] || self.material_placement_built[index] {
            return cy;
        }
        self.material_placement_built[index] = true;
        let mut cy = cy;
        let source = self.active_material_info.as_ref().and_then(|info| {
            info.textures.iter().find(|texture| {
                texture.connected && Self::material_family_for_port(&texture.port) == Some(family)
            })
        });
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            &format!("{} texture", Self::material_family_label(family)),
            section_label_style(),
        );
        cy += ROW_H;
        if let Some(texture) = source {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                &texture.source_label,
                label_style(),
            );
            cy += ROW_H;
        }
        if let Some(reason) = self.material_placement_widgets[index].reason() {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                &format!("Placement — {reason}"),
                label_style(),
            );
            return cy + ROW_H + ROW_GAP;
        }
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Placement",
            section_label_style(),
        );
        self.material_placement_widgets[index].build(
            tree,
            Some(self.content_parent),
            Rect::new(inner_x, cy + ROW_H, inner_w, inner_w),
            98_000 + index as u64 * 128,
        ) + ROW_H
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

    pub(super) fn material_rgb_row_visible(&self, rows: &[ParamRow], row: &ParamRow) -> bool {
        let Some(anchor) = rows.iter().find_map(|candidate| {
            candidate
                .rgb_members
                .as_ref()
                .and_then(|members| members.contains(&row.id).then(|| candidate.id.clone()))
        }) else {
            return true;
        };
        row.id == anchor || self.material_rgb_expanded.contains(&anchor)
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
        if self.material_object_gain(row) {
            return 2 + crate::param_surface::MaterialFeature::Emission as usize;
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
        if (self.material_param_named(&info, "metallic")
            || self.material_param_named(&info, "roughness"))
            && self.material_family_connected(MaterialMapFamily::MetallicRoughness)
        {
            info.spec.inactive_reason =
                Some("From texture — scalar applies without this map".into());
        }
        if self.material_object_gain(&info) {
            info.spec.name = "Object gain".into();
        }
        match info.spec.material_role {
            Some(MaterialParamRole::Placement(_, component)) => {
                info.spec.name = match component {
                    UvComponent::M00 => "M00",
                    UvComponent::M01 => "M01",
                    UvComponent::M10 => "M10",
                    UvComponent::M11 => "M11",
                    UvComponent::Tx => "Offset U",
                    UvComponent::Ty => "Offset V",
                }
                .into()
            }
            Some(MaterialParamRole::Sampler(_, component)) => {
                info.spec.name = match component {
                    crate::param_surface::SamplerComponent::WrapU => "Wrap U",
                    crate::param_surface::SamplerComponent::WrapV => "Wrap V",
                    crate::param_surface::SamplerComponent::MagFilter => "Magnification",
                    crate::param_surface::SamplerComponent::MinFilter => "Minification",
                }
                .into()
            }
            _ => {}
        }
        if self.material_param_named(&info, "alpha_mode") {
            info.spec.value_labels = Some(vec![
                "Solid".to_string(),
                "Cutout".to_string(),
                "Fade".to_string(),
            ]);
        } else if self.material_param_named(&info, "alpha_cutoff") {
            if self.material_opacity_mode() != Some(1) {
                info.spec.inactive_reason = Some("Only used in Cutout".to_string());
            }
        } else if self.material_param_named(&info, "color_a")
            && self.material_opacity_mode() == Some(0)
        {
            info.spec.inactive_reason = Some("Ignored in Solid".to_string());
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
            if !self.material_rgb_expanded.contains(&info.id) {
                return row_cy;
            }
        }

        // Trigger parameters use the same momentary button and ParamFire
        // dispatch as generator cards; a numeric slider cannot fire Reset.
        if info.spec.is_trigger {
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
        if let Some(MaterialParamRole::FeatureMode(feature)) = info.spec.material_role
            && let Some(slider) = &self.properties_card.row_host.slider_ids[slot]
        {
            let widget = |node: NodeId| tree.widget_of(node);
            self.properties_card.row_host.row_index.insert(
                widget(slider.track),
                slot,
                RowRole::MaterialFeatureToggle(feature),
            );
            self.properties_card.row_host.row_index.insert(
                widget(slider.value_text),
                slot,
                RowRole::MaterialFeatureToggle(feature),
            );
            if let Some(label) = slider.label {
                self.properties_card.row_host.row_index.insert(
                    widget(label),
                    slot,
                    RowRole::MaterialFeatureToggle(feature),
                );
            }
        }

        new_cy
    }

    fn material_full_value(&self, id: &manifold_foundation::ParamId) -> f32 {
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
                bg_color: Color32::new(to_byte(rgb[0]), to_byte(rgb[1]), to_byte(rgb[2]), 255),
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: if rgb.iter().copied().sum::<f32>() > 1.65 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                },
                font_size: color::FONT_LABEL,
                corner_radius: color::SMALL_RADIUS,
                ..btn_style()
            },
            &label,
            MATERIAL_SWATCH_KEY_BASE + slot as u64,
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
            self.configure_material_placements(&material);
            cy = self.build_material_header(tree, inner_x, inner_w, cy, &material);
            cy = self.build_material_feature_actions(tree, inner_x, inner_w, cy, &material);
            cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections);
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
        let scope = match info.shared_object_count {
            Some(1) => "Applies to 1 object".to_string(),
            Some(n) => format!("Applies to {n} objects"),
            None => "Shared scope unknown".to_string(),
        };
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            &scope,
            label_style(),
        );
        cy += ROW_H;
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Custom material",
            section_label_style(),
        );
        cy += ROW_H;
        if info.shared_object_count.is_some() {
            let looks = [
                (MaterialLook::Matte, "Matte", 0_u64),
                (MaterialLook::Coated, "Coated", 1_u64),
                (MaterialLook::BrushedMetal, "Brushed Metal", 2_u64),
                (MaterialLook::Glass, "Glass", 3_u64),
            ];
            let gap = ROW_GAP;
            let button_w = ((inner_w - gap * 3.0) / 4.0).max(0.0);
            for (look, label, offset) in looks {
                let id = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x + offset as f32 * (button_w + gap),
                    cy,
                    button_w,
                    ROW_H,
                    btn_style(),
                    label,
                    MATERIAL_LOOK_KEY_BASE + offset,
                );
                self.material_look_ids
                    .push((id, look, info.object.clone(), info.material.clone()));
            }
        } else {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Looks unavailable — shared scope unknown",
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
                MATERIAL_LOOK_KEY_BASE + 32 + index as u64,
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

    pub(super) fn handle_material_placement_event(
        &mut self,
        event: &UIEvent,
        tree: &mut UITree,
    ) -> Option<Vec<PanelAction>> {
        if let Some(index) = self
            .material_placement_active
            .iter()
            .copied()
            .enumerate()
            .find_map(|(index, active)| {
                (active && self.material_placement_widgets[index].is_dragging()).then_some(index)
            })
            && let Some(actions) = self.material_placement_widgets[index].handle_event(event, tree)
        {
            return Some(actions);
        }
        for (index, active) in self.material_placement_active.iter().copied().enumerate() {
            if active
                && let Some(actions) =
                    self.material_placement_widgets[index].handle_event(event, tree)
            {
                return Some(actions);
            }
        }
        None
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
            let current = param.value.base.round() as i32;
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
    fn material_inspector_texture_families_split_connected_and_dormant_drawers() {
        let placement = material_test_row(
            "51_base_uv_m00",
            MaterialParamRole::Placement(
                MaterialMapFamily::Base,
                crate::param_surface::UvComponent::M00,
            ),
            1.0,
            1.0,
        );
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
            textures: vec![MaterialTextureInfo {
                port: "base_color_map".into(),
                label: "base color".into(),
                source_label: "Connected · albedo.png".into(),
                connected: true,
                graph_source: false,
            }],
            params: Vec::new(),
        });
        assert_eq!(
            panel.material_section_name(&placement).as_deref(),
            Some("Textures · Base Color")
        );
        assert!(panel.material_section_folded("Textures · Base Color"));
        panel.active_material_info.as_mut().unwrap().textures[0].connected = false;
        assert_eq!(
            panel.material_section_name(&placement).as_deref(),
            Some("Advanced · Dormant Textures")
        );
        assert!(panel.material_section_folded("Advanced · Dormant Textures"));
        assert_eq!(
            ScenePanel::material_family_for_port("clearcoat_normal_map"),
            None
        );
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
    fn material_inspector_expanded_rgb_keeps_primary_channel_reachable() {
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
        let mut panel = ScenePanel::new();
        panel.material_rgb_expanded.insert(r.clone());
        let rows = vec![primary.clone(), green, blue];
        assert!(panel.material_rgb_row_visible(&rows, &primary));
        assert!(panel.material_rgb_row_visible(&rows, &rows[1]));
        assert!(panel.material_rgb_row_visible(&rows, &rows[2]));
    }
}
