//! Tree operations on a scene document: find, place, take, copy, and the
//! reference cleanup a removal does. Pure document edits; the commands in
//! `manifold-editing` wrap them in undo steps.

use std::collections::BTreeMap;

use super::{
    DrawAs, EmitFrom, ForceTargets, Item, ItemFields, ItemId, ItemKind, ModifierTargets, SceneDoc,
};

/// Why a document edit was refused. The document is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneEditError {
    UnknownItem(ItemId),
    DuplicateId(ItemId),
    /// `parent` cannot hold an item of `kind` (None is the top of the tree).
    CannotHold { parent: Option<ItemKind>, kind: ItemKind },
    /// A move would put an item inside itself.
    IntoItself(ItemId),
    /// A scene has one World.
    SecondWorld,
    /// A field set may not change an item's kind.
    KindChange { from: ItemKind, to: ItemKind },
}

impl std::fmt::Display for SceneEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneEditError::UnknownItem(id) => write!(f, "no item {id} in this scene"),
            SceneEditError::DuplicateId(id) => write!(f, "item id {id} is already used"),
            SceneEditError::CannotHold { parent: Some(parent), kind } => {
                write!(f, "a {parent:?} cannot hold a {kind:?}")
            }
            SceneEditError::CannotHold { parent: None, kind } => {
                write!(f, "a {kind:?} cannot sit at the top of the scene")
            }
            SceneEditError::IntoItself(id) => write!(f, "item {id} cannot move inside itself"),
            SceneEditError::SecondWorld => f.write_str("a scene has one World"),
            SceneEditError::KindChange { from, to } => {
                write!(f, "a {from:?} cannot become a {to:?}; remove it and add a {to:?}")
            }
        }
    }
}

/// Whether an item of `kind` may sit under `parent` (None is the top).
/// Groups hold anything placeable; a Water holds its sources and looks;
/// a Water Source sits only in a Water; the World, Forces and Modifiers sit
/// only at the top (their own sections of the tree).
pub fn can_hold(parent: Option<ItemKind>, kind: ItemKind) -> bool {
    match (parent, kind) {
        (_, ItemKind::World | ItemKind::Force | ItemKind::Modifier) => parent.is_none(),
        (Some(ItemKind::Water), ItemKind::WaterSource | ItemKind::Material) => true,
        (_, ItemKind::WaterSource) => false,
        (None | Some(ItemKind::Group), _) => true,
        _ => false,
    }
}

impl Item {
    pub fn new(id: ItemId, name: impl Into<String>, fields: ItemFields) -> Self {
        Self { id, name: name.into(), fields, children: Vec::new() }
    }

    pub fn kind(&self) -> ItemKind {
        self.fields.kind()
    }

    fn walk<'a>(&'a self, ancestors: &mut Vec<&'a Item>, f: &mut impl FnMut(&'a Item, &[&'a Item])) {
        f(self, ancestors);
        ancestors.push(self);
        for child in &self.children {
            child.walk(ancestors, f);
        }
        ancestors.pop();
    }
}

impl SceneDoc {
    /// Every item, depth first in tree order, with its ancestors outermost
    /// first.
    pub fn walk<'a>(&'a self, mut f: impl FnMut(&'a Item, &[&'a Item])) {
        let mut ancestors = Vec::new();
        for item in &self.items {
            item.walk(&mut ancestors, &mut f);
        }
    }

    pub fn find(&self, id: &ItemId) -> Option<&Item> {
        fn find_in<'a>(items: &'a [Item], id: &ItemId) -> Option<&'a Item> {
            items.iter().find_map(|item| if &item.id == id { Some(item) } else { find_in(&item.children, id) })
        }
        find_in(&self.items, id)
    }

    pub fn find_mut(&mut self, id: &ItemId) -> Option<&mut Item> {
        fn find_in<'a>(items: &'a mut [Item], id: &ItemId) -> Option<&'a mut Item> {
            for item in items {
                if &item.id == id {
                    return Some(item);
                }
                if let Some(found) = find_in(&mut item.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find_in(&mut self.items, id)
    }

    /// Where an item sits: its parent (None at the top) and its index there.
    pub fn locate(&self, id: &ItemId) -> Option<(Option<ItemId>, usize)> {
        fn locate_in(items: &[Item], parent: Option<&ItemId>, id: &ItemId) -> Option<(Option<ItemId>, usize)> {
            for (index, item) in items.iter().enumerate() {
                if &item.id == id {
                    return Some((parent.cloned(), index));
                }
                if let Some(found) = locate_in(&item.children, Some(&item.id), id) {
                    return Some(found);
                }
            }
            None
        }
        locate_in(&self.items, None, id)
    }

    /// Every item id, in tree order.
    pub fn ids(&self) -> Vec<ItemId> {
        let mut ids = Vec::new();
        self.walk(|item, _| ids.push(item.id.clone()));
        ids
    }

    /// The children list under `parent` (None is the top).
    fn children_mut(&mut self, parent: Option<&ItemId>) -> Result<&mut Vec<Item>, SceneEditError> {
        match parent {
            None => Ok(&mut self.items),
            Some(id) => Ok(&mut self.find_mut(id).ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?.children),
        }
    }

    fn parent_kind(&self, parent: Option<&ItemId>) -> Result<Option<ItemKind>, SceneEditError> {
        match parent {
            None => Ok(None),
            Some(id) => Ok(Some(self.find(id).ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?.kind())),
        }
    }

    /// Check that `item` (with its subtree) may go under `parent`.
    fn check_place(&self, parent: Option<&ItemId>, item: &Item) -> Result<(), SceneEditError> {
        let kind = item.kind();
        if !can_hold(self.parent_kind(parent)?, kind) {
            return Err(SceneEditError::CannotHold { parent: self.parent_kind(parent)?, kind });
        }
        let mut incoming = Vec::new();
        let mut worlds = 0;
        item.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| {
            incoming.push(inner.id.clone());
            worlds += usize::from(inner.kind() == ItemKind::World);
        });
        if let Some(id) = incoming.iter().find(|id| self.find(id).is_some()) {
            return Err(SceneEditError::DuplicateId(id.clone()));
        }
        let mut existing_worlds = 0;
        self.walk(|inner, _| existing_worlds += usize::from(inner.kind() == ItemKind::World));
        if worlds + existing_worlds > 1 {
            return Err(SceneEditError::SecondWorld);
        }
        Ok(())
    }

    /// Put `item` under `parent` at `index` (clamped to the end).
    pub fn insert(&mut self, parent: Option<&ItemId>, index: usize, item: Item) -> Result<(), SceneEditError> {
        self.check_place(parent, &item)?;
        let children = self.children_mut(parent)?;
        let index = index.min(children.len());
        children.insert(index, item);
        Ok(())
    }

    /// Take an item and its subtree out of the tree, with where it was.
    pub fn take(&mut self, id: &ItemId) -> Result<(Item, Option<ItemId>, usize), SceneEditError> {
        let (parent, index) = self.locate(id).ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
        let item = self.children_mut(parent.as_ref())?.remove(index);
        Ok((item, parent, index))
    }

    /// Move an item under `parent` at `index`. Also reorders within one
    /// parent. Refused, leaving the document as it was, when the move would
    /// put the item inside itself or under a parent that cannot hold it.
    pub fn reparent(&mut self, id: &ItemId, parent: Option<&ItemId>, index: usize) -> Result<(), SceneEditError> {
        if let Some(parent_id) = parent {
            let moving = self.find(id).ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
            let mut inside = false;
            moving.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| inside |= &inner.id == parent_id);
            if inside {
                return Err(SceneEditError::IntoItself(id.clone()));
            }
        }
        let (item, old_parent, old_index) = self.take(id)?;
        if let Err(error) = self.insert(parent, index, item.clone()) {
            self.children_mut(old_parent.as_ref())?.insert(old_index, item);
            return Err(error);
        }
        Ok(())
    }

    /// Remove an item and its subtree. Anything that referenced a removed
    /// item falls back to its default in the same edit (section 7.2).
    pub fn remove(&mut self, id: &ItemId) -> Result<Item, SceneEditError> {
        let (item, _, _) = self.take(id)?;
        let mut removed = Vec::new();
        item.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| removed.push(inner.id.clone()));
        self.drop_references(&removed);
        Ok(item)
    }

    /// Clear every reference to an item in `removed`.
    fn drop_references(&mut self, removed: &[ItemId]) {
        let gone = |id: &ItemId| removed.contains(id);
        fn each(items: &mut [Item], f: &mut impl FnMut(&mut ItemFields)) {
            for item in items {
                f(&mut item.fields);
                each(&mut item.children, f);
            }
        }
        each(&mut self.items, &mut |fields| match fields {
            ItemFields::Group(g) if g.material.as_ref().is_some_and(gone) => g.material = None,
            ItemFields::Object(o) if o.material.as_ref().is_some_and(gone) => o.material = None,
            ItemFields::Swarm(s) => {
                if s.material.as_ref().is_some_and(gone) {
                    s.material = None;
                }
                if matches!(&s.draw_as, DrawAs::Item(id) if gone(id)) {
                    s.draw_as = DrawAs::default();
                }
                if matches!(&s.emit_from, EmitFrom::Item(id) if gone(id)) {
                    s.emit_from = EmitFrom::default();
                }
            }
            ItemFields::Force(f) => {
                if let ForceTargets::Items(ids) = &mut f.targets {
                    ids.retain(|id| !gone(id));
                }
            }
            // A card left with no target acts on nothing.
            ItemFields::Modifier(m) => {
                if let ModifierTargets::Items(ids) = &mut m.targets {
                    ids.retain(|id| !gone(id));
                }
            }
            ItemFields::Camera(c) if c.target.as_ref().is_some_and(gone) => c.target = None,
            _ => {}
        });
    }

    /// A copy of `id`'s subtree with every id replaced by `fresh(old id)`,
    /// references inside the copy pointing at the copy, and the old-to-new
    /// id map (Add with a source, section 7.2).
    pub fn copy_subtree(
        &self,
        id: &ItemId,
        mut fresh: impl FnMut(&ItemId) -> ItemId,
    ) -> Result<(Item, BTreeMap<ItemId, ItemId>), SceneEditError> {
        let source = self.find(id).ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
        let mut map = BTreeMap::new();
        source.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| {
            map.insert(inner.id.clone(), fresh(&inner.id));
        });
        fn remap(item: &Item, map: &BTreeMap<ItemId, ItemId>) -> Item {
            let swap = |id: &ItemId| map.get(id).cloned().unwrap_or_else(|| id.clone());
            let mut fields = item.fields.clone();
            match &mut fields {
                ItemFields::Group(g) => g.material = g.material.as_ref().map(swap),
                ItemFields::Object(o) => o.material = o.material.as_ref().map(swap),
                ItemFields::Camera(c) => c.target = c.target.as_ref().map(swap),
                ItemFields::Swarm(s) => {
                    s.material = s.material.as_ref().map(swap);
                    if let DrawAs::Item(id) = &mut s.draw_as {
                        *id = swap(id);
                    }
                    if let EmitFrom::Item(id) = &mut s.emit_from {
                        *id = swap(id);
                    }
                }
                ItemFields::Force(f) => {
                    if let ForceTargets::Items(ids) = &mut f.targets {
                        ids.iter_mut().for_each(|id| *id = swap(id));
                    }
                }
                ItemFields::Modifier(m) => {
                    if let ModifierTargets::Items(ids) = &mut m.targets {
                        ids.iter_mut().for_each(|id| *id = swap(id));
                    }
                }
                _ => {}
            }
            Item {
                id: swap(&item.id),
                name: item.name.clone(),
                fields,
                children: item.children.iter().map(|child| remap(child, map)).collect(),
            }
        }
        Ok((remap(source, &map), map))
    }

    /// The material `id` draws with: its own binding, else the nearest
    /// ancestor's, else none (section 7.6, nearest wins). A binding to an item
    /// that is not a Material is ignored.
    pub fn effective_material(&self, id: &ItemId) -> Option<ItemId> {
        let mut found = None;
        self.walk(|item, ancestors| {
            if &item.id != id {
                return;
            }
            found = std::iter::once(item)
                .chain(ancestors.iter().rev().copied())
                .filter_map(|holder| holder.fields.material())
                .find(|material| self.find(material).is_some_and(|m| m.kind() == ItemKind::Material))
                .cloned();
        });
        found
    }

    /// The scene's World, if it has one.
    pub fn world(&self) -> Option<&Item> {
        self.items.iter().find(|item| item.kind() == ItemKind::World)
    }

    /// Every Camera, in tree order: the World's Camera row picks by index
    /// into this list.
    pub fn cameras(&self) -> Vec<&ItemId> {
        let mut cameras = Vec::new();
        self.walk(|item, _| {
            if item.kind() == ItemKind::Camera {
                cameras.push(&item.id);
            }
        });
        cameras
    }

    /// The param id of the World's Camera row.
    pub fn camera_pick_param(&self) -> Option<String> {
        self.world().map(|world| super::param_id(&world.id, super::CAMERA_PICK))
    }

    /// The camera a Camera row value picks: rounded, held to the cameras
    /// there are.
    pub fn picked_camera(&self, pick: f32) -> Option<&ItemId> {
        let cameras = self.cameras();
        let last = cameras.len().checked_sub(1)?;
        let index = if pick.is_finite() { pick.round().clamp(0.0, last as f32) as usize } else { 0 };
        cameras.get(index).copied()
    }

    /// Whether physics moves `id`, an item at or under it: it, something
    /// inside it, or a group around it has a physics setting.
    pub fn physical_under(&self, id: &ItemId) -> bool {
        let mut any = false;
        self.walk(|item, ancestors| {
            let physical = item.fields.physics() != super::PhysicsSetting::None
                || ancestors.iter().any(|holder| holder.fields.physics() != super::PhysicsSetting::None);
            any |= physical && (&item.id == id || ancestors.iter().any(|holder| &holder.id == id));
        });
        any
    }

    /// Whether any Object or Group has Copies on.
    pub fn any_copies(&self) -> bool {
        let mut any = false;
        self.walk(|item, _| any |= item.fields.copies());
        any
    }

    /// Whether the scene has a Swarm.
    pub fn any_swarm(&self) -> bool {
        let mut any = false;
        self.walk(|item, _| any |= item.kind() == super::ItemKind::Swarm);
        any
    }

    /// Whether any Object or Group has a physics setting.
    pub fn any_physics(&self) -> bool {
        let mut any = false;
        self.walk(|item, _| any |= item.fields.physics() != super::PhysicsSetting::None);
        any
    }
}
