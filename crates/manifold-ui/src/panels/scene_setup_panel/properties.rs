//! The selected item's controls (SCENE_DOCUMENT_DESIGN.md section 8): the
//! rows it owns, in manifest order, with its structural field rows, grouped
//! by control group in declaration order, one fold section per group, each
//! group's switch first. Hidden rows are left out unless attached or holding
//! a stored modulator; rows the item's current mode does not read are left
//! out unless attached (D6), and an attached unread row stays, dimmed, the
//! reason on hover.
//!
//! Every row goes through the shared `build_param_row`/`RowHost` core.
//! Structural fields are `RowAffordance::Field` rows of the same card, so a
//! disabled field is an ordinary dimmed row with its reason on hover. The
//! Material an item draws with is inline in its Material section
//! (`material_block.rs`).

use super::material_block::{material_bucket, row_feature};
use crate::panels::param_card::{ParamCardPanel, row_geometry};
use super::scale_link::SCALE_AXES;
use super::*;
use crate::param_surface::{MaterialFeature, MaterialParamRole, RowAffordance, RowValue};
use manifold_foundation::ParamId;

/// One item whose rows a properties pass lays out.
pub(super) struct Block<'a> {
    pub item: &'a SceneItemVm,
    /// The block opens with its own header (a light inside Lighting).
    pub sub_header: bool,
}

/// What the draw loop needs to know about one retained row.
struct Slot {
    group: ControlGroup,
    section: String,
    feature: Option<MaterialFeature>,
    material: bool,
}

/// A field row, as a row of the shared param surface.
fn field_param_row(item: &ItemId, field: &FieldRowVm) -> ParamRow {
    let mut row = placeholder_param_info();
    row.id = ParamId::from(format!("{item}#{}", field.id));
    row.spec.name = field.label.clone();
    row.spec.whole_numbers = true;
    row.spec.max = 1.0;
    row.spec.value_labels = Some(vec![match &field.control {
        FieldControlVm::Pick { current, .. } => current.clone(),
        FieldControlVm::File { path, .. } => {
            if path.is_empty() {
                "None".to_owned()
            } else {
                std::path::Path::new(path).file_name().map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned())
            }
        }
    }]);
    row.spec.inactive_reason = field.disabled.clone();
    row.spec.scene = Some(manifold_foundation::scene_surface::SceneParamDesc {
        role: SurfaceRole::Setup,
        group: field.section,
        band: None,
        card_order: 0,
        card_label: manifold_foundation::scene_surface::CardLabel::Param,
        gate: None,
        switch: None,
        colour: None,
    });
    row.value = RowValue { base: 0.0, effective: 0.0, exposed: false, driven: false };
    row.affordance = Some(RowAffordance::Field { item: item.clone(), field: field.id.clone(), control: field.control.clone() });
    row
}

impl ScenePanel {
    /// The selected item's header over its controls (D8).
    pub(super) fn build_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        selected: SceneSelection,
    ) -> f32 {
        match selected {
            SceneSelection::Item(id) => {
                let Some(item) = vm.item(&id) else { return cy };
                cy = self.build_item_header(tree, (inner_x, inner_w), cy, item);
                self.build_blocks(tree, inner_x, inner_w, cy, &[Block { item, sub_header: false }])
            }
            SceneSelection::Lighting => {
                let name = tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Lighting", header_label_style());
                tree.set_name(name, "scene_setup.properties.name_value");
                cy += ROW_H + ROW_GAP;
                let mut lights: Vec<&SceneItemVm> = vm.items.iter().filter(|item| item.kind == ItemKind::Light).collect();
                // The environment leads: its source and exposure come first.
                lights.sort_by_key(|item| !item.environment);
                let blocks: Vec<Block<'_>> = lights.into_iter().map(|item| Block { item, sub_header: true }).collect();
                self.build_blocks(tree, inner_x, inner_w, cy, &blocks)
            }
        }
    }

    /// Lay out `blocks`' rows as one card: every block's rows are retained
    /// into one filtered card (one id join, one set of widget ids), then drawn
    /// block by block.
    fn build_blocks(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, blocks: &[Block<'_>]) -> f32 {
        let Some(mut config) = self.full_params.clone() else {
            self.properties_card.resize(0);
            return cy;
        };
        // BUG-292: rows dispatch to the panel's own bound layer.
        let Some(target) = self.live_layer_id().cloned().map(GraphParamTarget::GeneratorOf) else {
            self.properties_card.resize(0);
            return cy;
        };
        let manifest_len = config.rows.len();
        let mut retained: Vec<usize> = Vec::new();
        let mut slots: Vec<Slot> = Vec::new();
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for block in blocks {
            let start = retained.len();
            self.retain_block_rows(&mut config, manifest_len, block, &mut retained, &mut slots);
            ranges.push(start..retained.len());
        }
        self.properties_card.configure_from_filtered(&config, &retained);
        self.prepare_pending_parameter_properties();
        self.properties_card.restore_live(&target);

        let RowGeometry { label_width, slider_w } = row_geometry(inner_w, false);
        self.properties_card.row_host.section_header_ids.clear();
        for (block, range) in blocks.iter().zip(ranges) {
            if block.sub_header {
                cy = self.build_light_header(tree, (inner_x, inner_w), cy, block.item);
            }
            cy = self.draw_block(tree, (inner_x, inner_w), cy, block, range, &slots, (label_width, slider_w), &target);
        }
        cy + ROW_GAP
    }

    /// Choose and order `block`'s rows: its own manifest rows, its structural
    /// field rows, the Material block's rows and the Scale link.
    fn retain_block_rows(
        &mut self,
        config: &mut ParamSurface,
        manifest_len: usize,
        block: &Block<'_>,
        retained: &mut Vec<usize>,
        slots: &mut Vec<Slot>,
    ) {
        let item = block.item;
        let owned: ahash::AHashSet<&str> = item.parameter_ids.iter().map(String::as_str).collect();
        // The item's mode switches, read from their base values (D12).
        let modes: Vec<(Feature, u32)> = config.rows[..manifest_len]
            .iter()
            .filter(|row| owned.contains(row.id.as_ref()))
            .filter_map(|row| {
                let feature = row.spec.scene?.switch.filter(|feature| feature.has_modes())?;
                Some((feature, row.value.base.round().max(0.0) as u32))
            })
            .collect();
        // (sort key, index into config.rows)
        let mut picked: Vec<((usize, u32), usize)> = Vec::new();
        for i in 0..manifest_len {
            if config.rows[i].spec.material_role.is_some() {
                continue;
            }
            let unread = match config.rows[i].spec.scene.and_then(|scene| scene.gate) {
                Some(Gate::Only(feature, read_in)) => modes
                    .iter()
                    .find(|(switch, _)| *switch == feature)
                    .is_some_and(|&(_, mode)| !read_in.contains(mode))
                    .then_some(feature),
                _ => None,
            };
            if let Some(feature) = unread
                && config.rows[i].is_attached()
            {
                config.rows[i].spec.inactive_reason = Some(feature.off_reason().into());
            }
            // Why the item cannot use the row now (an Impulse with nothing to
            // push, a body the world has no room for): dimmed, reason on hover.
            if let Some((_, reason)) = item.row_reasons.iter().find(|(id, _)| id.as_str() == config.rows[i].id.as_ref()) {
                config.rows[i].spec.inactive_reason = Some(reason.clone());
            }
            let p = &config.rows[i];
            if !owned.contains(p.id.as_ref()) {
                continue;
            }
            // A Hidden control shows while a modulator is stored on it, armed
            // or not: the row is the only way back to a disarmed one.
            let hidden_role = p.spec.scene.is_some_and(|scene| scene.role == SurfaceRole::Hidden) && !(p.modulator_stored || p.is_attached());
            let hidden = (hidden_role || (unread.is_some() && !p.is_attached())) && !self.property_explicitly_revealed(p);
            // Render Mode is drawn in the header, beside Show gizmos.
            let in_header = p.spec.scene.is_some_and(|scene| scene.group == ControlGroup::RenderMode);
            if hidden || in_header {
                continue;
            }
            let switch_first = p.spec.scene.is_none_or(|scene| scene.switch.is_none());
            picked.push(((self.group_key(p), 1 + u32::from(switch_first)), i));
        }
        // Structural fields: the item's own, then its Material's, each
        // addressed at the item that holds it.
        let mut field_rows: Vec<(&ItemId, &FieldRowVm)> = item.fields.iter().map(|field| (&item.id, field)).collect();
        if let Some(material) = item.material.as_ref().filter(|material| material.item != item.id) {
            field_rows.extend(material.fields.iter().map(|field| (&material.item, field)));
        }
        for (owner, field) in field_rows {
            let index = config.rows.len();
            config.rows.push(field_param_row(owner, field));
            picked.push(((field.section as usize * 2, 0), index));
        }
        // The Material block's rows.
        if let Some(material) = item.material.as_ref() {
            let ids: ahash::AHashSet<&str> = material.parameter_ids.iter().map(String::as_str).collect();
            for i in 0..manifest_len {
                let p = &config.rows[i];
                let Some(role) = p.spec.material_role else { continue };
                if !ids.contains(p.id.as_ref()) || matches!(role, MaterialParamRole::FeatureMode(_)) {
                    continue;
                }
                let revealed = self.property_explicitly_revealed(p);
                let dormant = matches!(role, MaterialParamRole::Placement(..) | MaterialParamRole::Sampler(..));
                let feature_off = row_feature(p).is_some_and(|feature| !material.features.contains(&feature));
                if (dormant || feature_off) && !revealed {
                    continue;
                }
                picked.push(((ControlGroup::Material as usize * 2, 10 + material_bucket(p, &material.features) as u32), i));
            }
        }
        picked.sort_by_key(|(key, _)| *key);
        let mut order: Vec<usize> = picked.into_iter().map(|(_, index)| index).collect();
        // The Scale link sits right under the last Scale axis.
        let last_scale = order.iter().rposition(|&index| {
            let id = config.rows[index].id.as_ref();
            id.split_once('.').is_some_and(|(owner, property)| owner == item.id.as_str() && SCALE_AXES.contains(&property))
        });
        if let Some(position) = last_scale {
            let index = config.rows.len();
            config.rows.push(self.scale_link_row(&item.id));
            order.insert(position + 1, index);
        }
        for index in order {
            let row = &config.rows[index];
            let group = self.property_group(row);
            let section = self.material_section_name(row).unwrap_or_default();
            slots.push(Slot { group, section, feature: row_feature(row), material: row.spec.material_role.is_some() });
            retained.push(index);
        }
    }

    /// The sort position of a row's group. Inside Lighting the camera's
    /// exposure is its own section, after the environment's.
    fn group_key(&self, row: &ParamRow) -> usize {
        if self.lighting_section_title(row).is_some() {
            return ControlGroup::Environment as usize * 2 + 1;
        }
        self.property_group(row) as usize * 2
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_block(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        block: &Block<'_>,
        range: std::ops::Range<usize>,
        slots: &[Slot],
        (label_width, slider_w): (f32, f32),
        target: &GraphParamTarget,
    ) -> f32 {
        let item = block.item;
        let mut i = range.start;
        while i < range.end {
            let name = slots[i].section.clone();
            let run_end = (i..range.end).find(|&j| slots[j].section != name).unwrap_or(range.end);
            let feature = slots[i].feature.filter(|_| slots[i].material);
            let removable = feature.zip(item.material.as_ref());
            let header_w = if removable.is_some() { inner_w - ROW_H - ROW_GAP } else { inner_w };
            let folded = self.material_section_folded(&name);
            self.section_folded.entry(name.clone()).or_insert(folded);
            let folded = self.section_folded.get(&name).copied().unwrap_or(folded);
            let first_row_id = self.properties_card.rows[i].id.clone();
            let header_id = ParamCardPanel::build_section_header(
                tree,
                Some(self.content_parent),
                Rect::new(inner_x, cy, header_w, ROW_H),
                &name,
                folded,
                run_end - i,
                param_row_key_base(&first_row_id),
            );
            tree.set_name(header_id, format!("scene_setup.group.{name}"));
            if slots[i].material {
                tree.set_name(header_id, format!("material.section.{first_row_id}"));
            }
            self.properties_card.row_host.section_header_ids.push((header_id, name.clone()));
            self.properties_card.row_host.row_index.insert(tree.widget_of(header_id), i, RowRole::SectionHeader);
            if let Some((feature, material)) = removable {
                self.build_feature_remove(tree, inner_x + inner_w - ROW_H, cy, &material.item, feature);
            }
            cy += ROW_H;
            if !folded {
                for row in i..run_end {
                    cy = self.build_properties_block(tree, inner_x, cy, row, label_width, slider_w, target.clone());
                }
                // The Material's summary closes the Material section's fields:
                // on an Object or Group with no Material too, which gets New
                // Material there.
                if !slots[i].material
                    && slots[i].group == ControlGroup::Material
                    && (item.material.is_some() || matches!(item.kind, ItemKind::Object | ItemKind::Group | ItemKind::Water))
                {
                    cy = self.build_material_summary(tree, (inner_x, inner_w), cy, item, item.material.as_ref(), (label_width, slider_w));
                }
            }
            // "+ Add feature" closes the last of the Material's sections.
            if let Some(material) = item.material.as_ref()
                && slots[run_end - 1].material
                && (run_end == range.end || !slots[run_end].material)
            {
                cy = self.build_add_feature(tree, (inner_x, inner_w), cy, material);
            }
            i = run_end;
        }
        cy
    }

    /// The group a properties row sorts and titles under: Material rows
    /// belong to Material, rows the scene table does not describe to
    /// Transform.
    pub(super) fn property_group(&self, row: &ParamRow) -> ControlGroup {
        if row.spec.material_role.is_some() {
            return ControlGroup::Material;
        }
        row.spec.scene.map_or(ControlGroup::Transform, |scene| scene.group)
    }

    /// Inside Lighting the camera's exposure is its own section.
    pub(super) fn lighting_section_title(&self, row: &ParamRow) -> Option<&'static str> {
        let lighting = self.state.as_live().is_some_and(|vm| self.selection.get(&vm.layer_id) == Some(&SceneSelection::Lighting));
        (lighting && row.spec.scene.is_some_and(|scene| scene.group == ControlGroup::Lens)).then_some("Exposure")
    }

    /// One properties-card row as the Scene tab walks them: a colour row
    /// draws its header then, when open, its three channel sliders; a G or B
    /// channel under a colour row on this card draws there, not here.
    pub(super) fn build_properties_block(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        cy: f32,
        slot: usize,
        label_width: f32,
        slider_w: f32,
        target: GraphParamTarget,
    ) -> f32 {
        let rows = &self.properties_card.rows;
        if self.properties_card.row_host.is_colour_tail(rows, slot, true) {
            return cy;
        }
        if rows[slot].kind(true) != RowKind::Colour {
            return self.build_properties_row(tree, inner_x, cy, slot, label_width, slider_w, target);
        }
        let host = &self.properties_card.row_host;
        let open = host.colour_open(&rows[slot]);
        let rgb = host.swatch_rgb_of(slot).unwrap_or([0.0; 3]);
        let Some(ids) = build_colour_row(
            tree,
            Some(self.content_parent),
            inner_x,
            cy,
            slider_w,
            label_width,
            &rows[slot],
            rgb,
            open,
            Some(param_row_key_base(rows[slot].id.as_ref())),
        ) else {
            return cy;
        };
        let mut cy = ids.new_cy;
        if open {
            for channel in RowHost::colour_channels(rows, slot).into_iter().flatten() {
                cy = self.build_properties_row(tree, inner_x, cy, channel, label_width, slider_w, target.clone());
            }
        }
        self.properties_card.row_host.install_colour_row(tree, slot, &ids);
        cy
    }

    /// One properties-card row, built through the SAME shared core every
    /// effect/generator card row uses: `slot` indexes
    /// `self.properties_card.rows` directly, whose `id` IS the real exposed
    /// param (or a field row's synthetic id) — the dispatch identity every
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
        let mut info = self.properties_card.rows[slot].clone();
        // The colour row's R channel draws here as its plain slider; the
        // header is `build_properties_block`'s.
        if info.kind(true) == RowKind::Colour {
            info.rgb_members = None;
        }
        // Switches, pick lists and fire buttons use the shared button row.
        let kind = info.kind(true);
        if kind != RowKind::Number {
            info.spec.default = self
                .properties_card
                .last_pushed_values
                .get(slot)
                .copied()
                .filter(|value| !value.is_nan())
                .unwrap_or(self.properties_card.current_values[slot]);
            let row = build_button_row(
                tree,
                Some(self.content_parent),
                inner_x,
                cy,
                slider_w,
                label_width,
                &info,
                kind,
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
            host.toggle_ids[slot] = Some(row.button_ids());
            host.envelope_btn_ids[slot] = row.envelope_btn;
            host.driver_btn_ids[slot] = row.driver_btn;
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
            _ => self.properties_card.current_values.get(slot).copied().unwrap_or(info.spec.default),
        };
        let display_value = self.properties_card.row_host.active_param_value(&target, &info.id).unwrap_or(display_value);

        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            cy,
            slider_w,
            &info,
            &self.properties_card.mod_state,
            slot,
            target,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true,
            label_width,
            self.properties_card.mod_active_tab.get(slot).copied().unwrap_or(ModTab::Driver),
            true,
            Some(param_row_key_base(info.id.as_ref())),
            None,
            Some(display_value),
        );
        let new_cy = built.new_cy;
        self.properties_card.row_host.install_row(tree, slot, built);

        new_cy
    }

    /// Route a resolved `(row, role)` hit on the properties card to its
    /// `PanelAction` by delegating to the shared [`RowHost::row_action`] —
    /// the same core `ParamCardPanel` uses (WIDGET_TREE_DESIGN section 4/5b).
    pub(super) fn properties_row_action(
        &mut self,
        tree: &UITree,
        row: usize,
        role: RowRole,
        node: NodeId,
        target: GraphParamTarget,
    ) -> Vec<PanelAction> {
        let card = &mut self.properties_card;
        let mut copied_flash = CopyToClipboardLabelState::default();
        card.row_host.row_action(
            tree,
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
