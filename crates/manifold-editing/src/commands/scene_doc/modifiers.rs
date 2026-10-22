//! The Modifiers chain (SCENE_DOCUMENT_DESIGN.md section 7.19 (Modifiers)):
//! Modifier items at the top of the scene, applied in tree order. Adding a
//! card is one Add, reordering is one Reparent, and toggling, swapping its
//! Type or changing its targets is one Set, so each is one undo step. A
//! card's dials are params addressed by `<card id>.<param>`, so a reorder
//! never moves a mapping.

use manifold_core::LayerId;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, ModifierFields, ModifierTargets, ModifierType, SceneDoc};
use manifold_core::scene_schema::modifier_type;

use super::commands::{AddSceneItemCommand, ReparentSceneItemCommand};

/// A new card of `modifier`, named after its type, aimed at the Objects and
/// Groups in `selection`, or at the whole scene when none is selected.
pub fn new_modifier(doc: &SceneDoc, id: ItemId, modifier: ModifierType, selection: &[ItemId]) -> Item {
    let aimed: Vec<ItemId> = selection
        .iter()
        .filter(|id| doc.find(id).is_some_and(|item| matches!(item.kind(), ItemKind::Object | ItemKind::Group)))
        .cloned()
        .collect();
    let targets = if aimed.is_empty() { ModifierTargets::Scene } else { ModifierTargets::Items(aimed) };
    let fields = ModifierFields { modifier, targets, ..ModifierFields::default() };
    Item::new(id, modifier_type(modifier).label, ItemFields::Modifier(fields))
}

/// Add `card` at the end of the chain.
pub fn add_modifier(layer: &LayerId, doc: &SceneDoc, card: Item) -> AddSceneItemCommand {
    AddSceneItemCommand::with_subtree(layer.clone(), None, doc.items.len(), card)
}

/// Move `card` to place `to` in the chain (0 is applied first).
pub fn move_modifier(
    layer: &LayerId,
    doc: &SceneDoc,
    card: &ItemId,
    to: usize,
) -> Result<ReparentSceneItemCommand, String> {
    let Some(ItemKind::Modifier) = doc.find(card).map(Item::kind) else {
        return Err("That card is no longer in the scene".into());
    };
    // Reparent places the card after taking it out, so count without it.
    let rest: Vec<&Item> = doc.items.iter().filter(|item| &item.id != card).collect();
    let chain: Vec<usize> =
        rest.iter().enumerate().filter(|(_, item)| item.kind() == ItemKind::Modifier).map(|(at, _)| at).collect();
    let index = match chain.get(to) {
        Some(at) => *at,
        None => chain.last().map_or(rest.len(), |last| last + 1),
    };
    Ok(ReparentSceneItemCommand::new(layer.clone(), card.clone(), None, index))
}
