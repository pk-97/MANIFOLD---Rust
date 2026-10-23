//! The scene card: a scene generator's performance surface
//! (SCENE_DOCUMENT_DESIGN.md section 7.17, SCENE_SURFACES_DESIGN.md D4).
//!
//! Rows stay ordinary manifest rows with their real ids, values, modulation
//! and mappings. This module only chooses, names, sections and orders them,
//! from two facts: each control's scene description (the joined
//! `Param.spec.scene`) and the scene item that owns it (the item id every
//! scene param id starts with). It never reads a param name: every per-type
//! fact is a row of the scene table.

use manifold_core::effects::PresetInstance;
use manifold_core::params::{Param, ParamOrigin};
use manifold_core::scene_doc::{Item, ItemKind, SceneDoc, split_param_id};
use manifold_core::scene_surface::{CardBand, CardLabel, CardPin, Feature, Gate, SurfaceRole};
use manifold_ui::param_surface::{ColourGroup, ParamRow, RgbMembers};

/// Curate a scene generator's rows into the card's bands (Camera, Scene,
/// Forces, Lighting, Look).
pub(super) fn scene_card_rows(rows: &mut Vec<ParamRow>, inst: &PresetInstance, doc: &SceneDoc) {
    let mut items: Vec<&Item> = Vec::new();
    doc.walk(|item, _| items.push(item));
    let owner = |id: &str| split_param_id(id).and_then(|(item, _)| items.iter().position(|candidate| candidate.id.as_str() == item));
    let switches = FeatureSwitches::read(rows, inst, &owner);
    let colours = item_colours(rows, inst, &items, &owner);

    let mut placed: Vec<(SortKey, ParamRow)> = Vec::with_capacity(rows.len());
    for (manifest_index, mut row) in std::mem::take(rows).into_iter().enumerate() {
        let Some(param) = inst.params.get(row.id.as_ref()) else { continue };
        let owner_index = owner(row.id.as_ref());
        let item = owner_index.map(|index| items[index]);
        // A pinned row stays whatever its gate: Show on Card and every
        // modulator attach write the pin (D6 (attached rows)).
        let pinned = param.choices.pin == Some(CardPin::Shown);
        // A Scene Loop row only a Loop camera reads waits for one; pinned,
        // it stays dimmed.
        if let (Some(item), Some((_, property))) = (item, split_param_id(row.id.as_ref()))
            && manifold_core::scene_schema::waits_for_loop_camera(doc, item, property)
        {
            if !pinned {
                continue;
            }
            row.spec.inactive_reason = Some(manifold_core::scene_schema::NO_LOOP_CAMERA.into());
        }
        let Some(place) = Placement::of(param, item) else {
            continue;
        };
        if !pinned && !row.value.exposed {
            continue;
        }
        match place.gate {
            Some(Gate::Fold(feature)) if !pinned && !switches.is_on(feature) => continue,
            // A pinned row stays with its feature off, dimmed like a Dim row.
            Some(Gate::Fold(feature) | Gate::Dim(feature)) if !switches.is_on(feature) => {
                row.spec.inactive_reason = Some(feature.off_reason().into());
            }
            _ => {}
        }
        if place.label == CardLabel::ItemName
            && let Some(index) = owner_index
        {
            row.rgb_members = colours.iter().find(|(item, _)| *item == index).map(|(_, members)| members.clone());
        }
        if param.choices.rename.is_none() {
            match (place.label, item) {
                (CardLabel::ItemName, Some(item)) => {
                    row.spec.name = format!("{} {}", item.name, row.spec.name.to_lowercase());
                }
                (CardLabel::GroupName, _) => {
                    if let Some(scene) = param.spec.scene {
                        row.spec.name = scene.group.title().into();
                    }
                }
                _ => {}
            }
        }
        // One section per item in the Scene and Forces bands; the rest share
        // their band's title.
        let per_item = matches!(place.band, CardBand::Scene | CardBand::Forces);
        row.spec.section = Some(match (per_item, item) {
            (true, Some(item)) => item.name.clone(),
            _ => place.band.title().into(),
        });
        let item_rank = owner_index.unwrap_or(items.len());
        let key = if per_item {
            (place.band, item_rank, place.card_order, 0, manifest_index)
        } else {
            (place.band, 0, place.card_order, item_rank, manifest_index)
        };
        placed.push((key, row));
    }
    placed.sort_by_key(|(key, _)| *key);
    *rows = placed.into_iter().map(|(_, row)| row).collect();
    super::material::colour_rows(rows, inst);
    // A light colour shown on the card as its own colour row takes the
    // swatch off the light's fader: one swatch per colour.
    for index in 0..rows.len() {
        let Some(ids) = rows[index].rgb_members.as_ref().filter(|m| m.ids[0] != rows[index].id).map(|m| m.ids.clone()) else {
            continue;
        };
        if rows.iter().any(|row| row.rgb_members.as_ref().is_some_and(|m| m.ids == ids && m.ids[0] == row.id)) {
            rows[index].rgb_members = None;
        }
    }
}

/// Name the World Camera row's values after the cameras holding those cut
/// numbers. The compiled row carries no names, so a rename reaches the row
/// here without recompiling the scene.
pub(super) fn name_cuts(rows: &mut [ParamRow], doc: &SceneDoc) {
    let Some(id) = doc.camera_pick_param() else { return };
    if let Some(row) = rows.iter_mut().find(|row| row.id.as_ref() == id) {
        row.spec.value_labels = Some(doc.cut_labels());
    }
}

/// Lighting starts folded past this many light rows (D11 (lights)).
const LIGHT_ROWS_BEFORE_FOLD: usize = 8;

/// The card's folded section titles, from curated `rows`: the user's saved
/// fold choices (D7 (user choices)) over the default, which folds Lighting
/// past eight light rows.
pub(super) fn folded_sections(inst: &PresetInstance, doc: &SceneDoc, rows: &[ParamRow]) -> Vec<String> {
    let lighting = CardBand::Lighting.title();
    let light_rows = rows
        .iter()
        .filter(|row| row.spec.section.as_deref() == Some(lighting))
        .filter(|row| {
            split_param_id(row.id.as_ref())
                .and_then(|(item, _)| doc.find(&item.into()))
                .is_some_and(|item| item.kind() == ItemKind::Light)
        })
        .count();
    let mut titles: Vec<&str> = rows.iter().filter_map(|row| row.spec.section.as_deref()).collect();
    titles.dedup();
    let saved = |list: &[String], title: &str| list.iter().any(|saved| saved == title);
    titles
        .into_iter()
        .filter(|title| {
            saved(&inst.folded_sections, title)
                || (!saved(&inst.unfolded_sections, title) && *title == lighting && light_rows > LIGHT_ROWS_BEFORE_FOLD)
        })
        .map(str::to_string)
        .collect()
}

/// (band, section rank, card order, item rank, manifest order). A per-item
/// band ranks its sections by item; a shared band orders by card order
/// first, so Lighting reads Exposure, Environment, then each light.
type SortKey = (CardBand, usize, u8, usize, usize);

/// Where one row goes on the card, or `None` when it stays off.
struct Placement {
    band: CardBand,
    card_order: u8,
    label: CardLabel,
    gate: Option<Gate>,
}

impl Placement {
    /// The card shows only what the user put there: a Show on Card or a
    /// mapping (both write the pin, D6), or a control the user added. A
    /// scene control's role never places it, so a new scene's card starts
    /// empty.
    fn of(param: &Param, item: Option<&Item>) -> Option<Self> {
        let pin = param.choices.pin;
        let user_added = matches!(param.origin, ParamOrigin::UserAdded);
        let shown = pin == Some(CardPin::Shown) || (user_added && pin != Some(CardPin::Removed));
        let Some(scene) = param.spec.scene else {
            // An authored control follows its card visibility, in the Scene band.
            let on = shown || (param.spec.card_visible && pin != Some(CardPin::Removed));
            return on.then_some(Self { band: CardBand::Scene, card_order: 0, label: CardLabel::Param, gate: None });
        };
        if !shown {
            return None;
        }
        let band = scene.band.filter(|_| scene.role == SurfaceRole::Performance).unwrap_or_else(|| item_band(item));
        Some(Self { band, card_order: scene.card_order, label: scene.card_label, gate: scene.gate })
    }
}

/// The band an item's off-role rows join when they are shown anyway.
fn item_band(item: Option<&Item>) -> CardBand {
    match item.map(Item::kind) {
        Some(ItemKind::Camera) => CardBand::Camera,
        Some(ItemKind::Light) => CardBand::Lighting,
        Some(ItemKind::Force) => CardBand::Forces,
        _ => CardBand::Scene,
    }
}

/// Each feature's switch, read from the authored base value (D12 (row sets
/// follow authored values)): a driver flickering the effective value never
/// rebuilds the card. A feature with no owned switch is off.
struct FeatureSwitches {
    on: Vec<Feature>,
}

impl FeatureSwitches {
    fn read(rows: &[ParamRow], inst: &PresetInstance, owner: &dyn Fn(&str) -> Option<usize>) -> Self {
        let on = rows
            .iter()
            .filter_map(|row| {
                let param = inst.params.get(row.id.as_ref())?;
                let feature = param.spec.scene?.switch?;
                (owner(row.id.as_ref()).is_some() && param.base > 0.5).then_some(feature)
            })
            .collect();
        Self { on }
    }

    fn is_on(&self, feature: Feature) -> bool {
        self.on.contains(&feature)
    }
}

/// Each light's colour swatch, from the rows its item owns whose scene
/// description marks one colour channel. The row the card names after the
/// item carries it, so recolouring a light is one gesture on its fader. An
/// item with more than one colour, or channels a single RGB write cannot
/// own, gets no swatch.
fn item_colours(rows: &[ParamRow], inst: &PresetInstance, items: &[&Item], owner: &dyn Fn(&str) -> Option<usize>) -> Vec<(usize, RgbMembers)> {
    let mut channels: Vec<(usize, [Option<&ParamRow>; 3], bool)> = Vec::new();
    for row in rows {
        let Some(channel) = inst.params.get(row.id.as_ref()).and_then(|p| p.spec.scene).and_then(|s| s.colour) else {
            continue;
        };
        let Some(item) = owner(row.id.as_ref()).filter(|&i| items[i].kind() == ItemKind::Light) else {
            continue;
        };
        let index = match channels.iter().position(|(i, _, _)| *i == item) {
            Some(index) => index,
            None => {
                channels.push((item, [None; 3], true));
                channels.len() - 1
            }
        };
        let (_, members, single) = &mut channels[index];
        *single &= members[channel.index()].is_none();
        members[channel.index()] = Some(row);
    }
    channels
        .into_iter()
        .filter_map(|(item, members, single)| match members {
            [Some(r), Some(g), Some(b)] if single => super::material::rgb_members(inst, ColourGroup::Light, [r, g, b]).map(|m| (item, m)),
            _ => None,
        })
        .collect()
}
