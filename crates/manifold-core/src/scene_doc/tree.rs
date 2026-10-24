//! Tree operations on a scene document: find, place, take, copy, and the
//! reference cleanup a removal does. Pure document edits; the commands in
//! `manifold-editing` wrap them in undo steps.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{
    DrawAs, EmitFrom, ForceTargets, Item, ItemFields, ItemId, ItemKind, LightType, ModifierTargets,
    ModifierType, SceneDoc,
};

/// Why a document edit was refused. The document is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneEditError {
    UnknownItem(ItemId),
    DuplicateId(ItemId),
    /// `parent` cannot hold an item of `kind` (None is the top of the tree).
    CannotHold {
        parent: Option<ItemKind>,
        kind: ItemKind,
    },
    /// A move would put an item inside itself.
    IntoItself(ItemId),
    /// A scene has one World.
    SecondWorld,
    /// A field set may not change an item's kind.
    KindChange {
        from: ItemKind,
        to: ItemKind,
    },
    /// A scene holds one card of this modifier type; the text says which.
    SecondOfType(&'static str),
}

impl std::fmt::Display for SceneEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneEditError::UnknownItem(id) => write!(f, "no item {id} in this scene"),
            SceneEditError::DuplicateId(id) => write!(f, "item id {id} is already used"),
            SceneEditError::CannotHold {
                parent: Some(parent),
                kind,
            } => {
                write!(f, "a {parent:?} cannot hold a {kind:?}")
            }
            SceneEditError::CannotHold { parent: None, kind } => {
                write!(f, "a {kind:?} cannot sit at the top of the scene")
            }
            SceneEditError::IntoItself(id) => write!(f, "item {id} cannot move inside itself"),
            SceneEditError::SecondWorld => f.write_str("a scene has one World"),
            SceneEditError::KindChange { from, to } => {
                write!(
                    f,
                    "a {from:?} cannot become a {to:?}; remove it and add a {to:?}"
                )
            }
            SceneEditError::SecondOfType(reason) => f.write_str(reason),
        }
    }
}

/// Why a scene refuses a second Environment light.
pub const ONE_ENVIRONMENT: &str = "A scene has one Environment";

fn is_environment(item: &Item) -> bool {
    matches!(&item.fields, ItemFields::Light(light) if matches!(light.light, LightType::Environment { .. }))
}

/// Why a Water refuses a second Fill.
pub const ONE_FILL: &str = "This Water already has a Fill; a Water takes one";

fn is_fill(item: &Item) -> bool {
    matches!(&item.fields, ItemFields::WaterSource(source) if source.role == super::SourceRole::Fill)
}

/// Whether `item` draws with its effective Material: an Object, Group or
/// Water, and a Swarm whose particles are its own solids (one drawing
/// another item draws that item's Material).
pub fn draws_with_material(item: &Item) -> bool {
    match &item.fields {
        ItemFields::Object(_) | ItemFields::Group(_) | ItemFields::Water(_) => true,
        ItemFields::Swarm(swarm) => !matches!(swarm.draw_as, DrawAs::Item(_)),
        _ => false,
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
        Self {
            id,
            name: name.into(),
            fields,
            children: Vec::new(),
        }
    }

    pub fn kind(&self) -> ItemKind {
        self.fields.kind()
    }

    fn walk<'a>(
        &'a self,
        ancestors: &mut Vec<&'a Item>,
        f: &mut impl FnMut(&'a Item, &[&'a Item]),
    ) {
        f(self, ancestors);
        ancestors.push(self);
        for child in &self.children {
            child.walk(ancestors, f);
        }
        ancestors.pop();
    }

    /// This item and its subtree with every id `map` names swapped, the
    /// references between items included; an id `map` does not name stays.
    pub fn remapped(&self, map: &BTreeMap<ItemId, ItemId>) -> Item {
        let swap = |id: &ItemId| map.get(id).cloned().unwrap_or_else(|| id.clone());
        let mut fields = self.fields.clone();
        match &mut fields {
            ItemFields::Group(g) => {
                g.material = g.material.as_ref().map(swap);
                g.copies_surface = g.copies_surface.as_ref().map(swap);
            }
            ItemFields::Object(o) => {
                o.material = o.material.as_ref().map(swap);
                o.copies_surface = o.copies_surface.as_ref().map(swap);
            }
            ItemFields::Water(w) => w.material = w.material.as_ref().map(swap),
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
            id: swap(&self.id),
            name: self.name.clone(),
            fields,
            children: self
                .children
                .iter()
                .map(|child| child.remapped(map))
                .collect(),
        }
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
            items.iter().find_map(|item| {
                if &item.id == id {
                    Some(item)
                } else {
                    find_in(&item.children, id)
                }
            })
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
        fn locate_in(
            items: &[Item],
            parent: Option<&ItemId>,
            id: &ItemId,
        ) -> Option<(Option<ItemId>, usize)> {
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
            Some(id) => Ok(&mut self
                .find_mut(id)
                .ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?
                .children),
        }
    }

    fn parent_kind(&self, parent: Option<&ItemId>) -> Result<Option<ItemKind>, SceneEditError> {
        match parent {
            None => Ok(None),
            Some(id) => Ok(Some(
                self.find(id)
                    .ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?
                    .kind(),
            )),
        }
    }

    /// Check that `item` (with its subtree) may go under `parent`.
    fn check_place(&self, parent: Option<&ItemId>, item: &Item) -> Result<(), SceneEditError> {
        let kind = item.kind();
        if !can_hold(self.parent_kind(parent)?, kind) {
            return Err(SceneEditError::CannotHold {
                parent: self.parent_kind(parent)?,
                kind,
            });
        }
        let mut incoming = Vec::new();
        let (mut worlds, mut environments) = (0, 0);
        item.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| {
            incoming.push(inner.id.clone());
            worlds += usize::from(inner.kind() == ItemKind::World);
            environments += usize::from(is_environment(inner));
        });
        if let Some(id) = incoming.iter().find(|id| self.find(id).is_some()) {
            return Err(SceneEditError::DuplicateId(id.clone()));
        }
        let (mut existing_worlds, mut existing_environments) = (0, 0);
        self.walk(|inner, _| {
            existing_worlds += usize::from(inner.kind() == ItemKind::World);
            existing_environments += usize::from(is_environment(inner));
        });
        if worlds + existing_worlds > 1 {
            return Err(SceneEditError::SecondWorld);
        }
        // A second Environment would compile to nothing: the scene lights
        // from one environment.
        if environments + existing_environments > 1 {
            return Err(SceneEditError::SecondOfType(ONE_ENVIRONMENT));
        }
        if is_fill(item)
            && self.parent_kind(parent)? == Some(ItemKind::Water)
            && self.fills_under(parent) > 0
        {
            return Err(SceneEditError::SecondOfType(ONE_FILL));
        }
        if let Some(reason) = self.second_of_type(item) {
            return Err(SceneEditError::SecondOfType(reason));
        }
        Ok(())
    }

    /// How many Fill sources sit directly under `parent`.
    fn fills_under(&self, parent: Option<&ItemId>) -> usize {
        let children = match parent {
            Some(id) => self
                .find(id)
                .map_or(&[][..], |item| item.children.as_slice()),
            None => self.items.as_slice(),
        };
        children.iter().filter(|child| is_fill(child)).count()
    }

    /// Why the Water holding `item` refuses it: a second Fill. A field set
    /// checks this after it writes, as an insert checks before.
    pub fn second_fill(&self, item: &ItemId) -> Option<&'static str> {
        let (parent, _) = self.locate(item)?;
        let water = self
            .find(parent.as_ref()?)
            .filter(|water| water.kind() == ItemKind::Water)?;
        (water.children.iter().filter(|child| is_fill(child)).count() > 1).then_some(ONE_FILL)
    }

    /// Why `item` would be a second card of a type a scene holds one of.
    fn second_of_type(&self, item: &Item) -> Option<&'static str> {
        let ItemFields::Modifier(card) = &item.fields else {
            return None;
        };
        self.second_modifier(card.modifier, Some(&item.id))
    }

    /// Why the scene cannot take a card of `modifier` beside the one it
    /// holds, other than `except` (the card being changed). Modifier cards sit
    /// at the top of the scene only.
    pub fn second_modifier(
        &self,
        modifier: ModifierType,
        except: Option<&ItemId>,
    ) -> Option<&'static str> {
        let reason = modifier.once_per_scene()?;
        self.items
            .iter()
            .any(|other| {
                Some(&other.id) != except
                    && matches!(&other.fields, ItemFields::Modifier(m) if m.modifier == modifier)
            })
            .then_some(reason)
    }

    /// Put `item`, new to the scene, under `parent` at `index` (clamped to the
    /// end). Every Camera in it gets a fresh cut number, so a duplicate, a
    /// paste or an import never takes another camera's cuts.
    pub fn insert(
        &mut self,
        parent: Option<&ItemId>,
        index: usize,
        mut item: Item,
    ) -> Result<(), SceneEditError> {
        self.check_place(parent, &item)?;
        self.number_cameras(&mut item);
        self.place(parent, index, item)
    }

    /// Put `item` under `parent` at `index` as it is, once checked.
    fn place(
        &mut self,
        parent: Option<&ItemId>,
        index: usize,
        item: Item,
    ) -> Result<(), SceneEditError> {
        let children = self.children_mut(parent)?;
        let index = index.min(children.len());
        children.insert(index, item);
        Ok(())
    }

    /// The cut number the next Camera to enter the scene gets.
    pub fn upcoming_cut(&self) -> u32 {
        self.next_cut.max(self.cut_count())
    }

    /// Give every Camera in `item` the next cut number. Counts on from past
    /// every number the scene holds, so a document built by hand never
    /// hands one out twice.
    pub(super) fn number_cameras(&mut self, item: &mut Item) {
        let mut next = self.upcoming_cut();
        fn visit(item: &mut Item, next: &mut u32) {
            if let ItemFields::Camera(camera) = &mut item.fields {
                camera.cut = *next;
                *next += 1;
            }
            item.children
                .iter_mut()
                .for_each(|child| visit(child, next));
        }
        visit(item, &mut next);
        self.next_cut = next;
    }

    /// Replace `id`'s structural fields, keeping its kind and, for a Camera,
    /// its cut number.
    pub fn set_fields(
        &mut self,
        id: &ItemId,
        mut fields: ItemFields,
    ) -> Result<(), SceneEditError> {
        let item = self
            .find_mut(id)
            .ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
        if item.kind() != fields.kind() {
            return Err(SceneEditError::KindChange {
                from: item.kind(),
                to: fields.kind(),
            });
        }
        if let (ItemFields::Camera(old), ItemFields::Camera(new)) = (&item.fields, &mut fields) {
            new.cut = old.cut;
        }
        item.fields = fields;
        Ok(())
    }

    /// Take an item and its subtree out of the tree, with where it was.
    pub fn take(&mut self, id: &ItemId) -> Result<(Item, Option<ItemId>, usize), SceneEditError> {
        let (parent, index) = self
            .locate(id)
            .ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
        let item = self.children_mut(parent.as_ref())?.remove(index);
        Ok((item, parent, index))
    }

    /// Move an item under `parent` at `index`. Also reorders within one
    /// parent. Refused, leaving the document as it was, when the move would
    /// put the item inside itself or under a parent that cannot hold it.
    pub fn reparent(
        &mut self,
        id: &ItemId,
        parent: Option<&ItemId>,
        index: usize,
    ) -> Result<(), SceneEditError> {
        if let Some(parent_id) = parent {
            let moving = self
                .find(id)
                .ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
            let mut inside = false;
            moving.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| {
                inside |= &inner.id == parent_id
            });
            if inside {
                return Err(SceneEditError::IntoItself(id.clone()));
            }
        }
        let (item, old_parent, old_index) = self.take(id)?;
        if let Err(error) = self
            .check_place(parent, &item)
            .and_then(|()| self.place(parent, index, item.clone()))
        {
            self.children_mut(old_parent.as_ref())?
                .insert(old_index, item);
            return Err(error);
        }
        Ok(())
    }

    /// Remove an item and its subtree. Anything that referenced a removed
    /// item falls back to its default in the same edit (section 7.2).
    pub fn remove(&mut self, id: &ItemId) -> Result<Item, SceneEditError> {
        let (item, _, _) = self.take(id)?;
        let mut removed = Vec::new();
        item.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| {
            removed.push(inner.id.clone())
        });
        self.drop_references(&removed);
        Ok(item)
    }

    /// Clear every reference to an item in `removed`.
    fn drop_references(&mut self, removed: &[ItemId]) {
        self.retain_references(|id| !removed.contains(id));
    }

    /// Clear every reference to an item `keep` refuses; each falls back to
    /// its default, as a removal does.
    pub fn retain_references(&mut self, keep: impl Fn(&ItemId) -> bool) {
        let gone = |id: &ItemId| !keep(id);
        fn each(items: &mut [Item], f: &mut impl FnMut(&mut ItemFields)) {
            for item in items {
                f(&mut item.fields);
                each(&mut item.children, f);
            }
        }
        each(&mut self.items, &mut |fields| match fields {
            ItemFields::Group(g) => {
                if g.material.as_ref().is_some_and(gone) {
                    g.material = None;
                }
                if g.copies_surface.as_ref().is_some_and(gone) {
                    g.copies_surface = None;
                }
            }
            ItemFields::Object(o) => {
                if o.material.as_ref().is_some_and(gone) {
                    o.material = None;
                }
                if o.copies_surface.as_ref().is_some_and(gone) {
                    o.copies_surface = None;
                }
            }
            ItemFields::Water(w) if w.material.as_ref().is_some_and(gone) => w.material = None,
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
        let source = self
            .find(id)
            .ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
        let mut map = BTreeMap::new();
        source.walk(&mut Vec::new(), &mut |inner: &Item, _: &[&Item]| {
            map.insert(inner.id.clone(), fresh(&inner.id));
        });
        Ok((source.remapped(&map), map))
    }

    /// [`Self::effective_material`] of every item that has one, in one walk.
    pub fn effective_materials(&self) -> HashMap<ItemId, ItemId> {
        let mut materials = HashSet::new();
        self.walk(|item, _| {
            if item.kind() == ItemKind::Material {
                materials.insert(item.id.clone());
            }
        });
        let mut effective = HashMap::new();
        self.walk(|item, ancestors| {
            let found = std::iter::once(item)
                .chain(ancestors.iter().rev().copied())
                .filter_map(|holder| holder.fields.material())
                .find(|material| materials.contains(*material));
            if let Some(material) = found {
                effective.insert(item.id.clone(), material.clone());
            }
        });
        effective
    }

    /// How many items draw with each Material ([`draws_with_material`]).
    pub fn material_users(&self) -> HashMap<ItemId, usize> {
        let effective = self.effective_materials();
        let mut users = HashMap::new();
        self.walk(|item, _| {
            if draws_with_material(item)
                && let Some(material) = effective.get(&item.id)
            {
                *users.entry(material.clone()).or_insert(0) += 1;
            }
        });
        users
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
                .find(|material| {
                    self.find(material)
                        .is_some_and(|m| m.kind() == ItemKind::Material)
                })
                .cloned();
        });
        found
    }

    /// The scene's World, if it has one.
    pub fn world(&self) -> Option<&Item> {
        self.items
            .iter()
            .find(|item| item.kind() == ItemKind::World)
    }

    /// Every Camera, in tree order.
    pub fn cameras(&self) -> Vec<&ItemId> {
        let mut cameras = Vec::new();
        self.walk(|item, _| {
            if item.kind() == ItemKind::Camera {
                cameras.push(&item.id);
            }
        });
        cameras
    }

    /// Every Camera with its cut number, lowest number first; the first is
    /// the camera a number no camera holds shows.
    pub fn cuts(&self) -> Vec<(u32, &ItemId)> {
        let mut cuts = Vec::new();
        self.walk(|item, _| {
            if let ItemFields::Camera(camera) = &item.fields {
                cuts.push((camera.cut, &item.id));
            }
        });
        cuts.sort();
        cuts
    }

    /// One past the highest cut number a live camera holds: the Camera row
    /// spans the live cameras, so a deleted camera's number past them takes
    /// no range.
    pub fn cut_count(&self) -> u32 {
        self.cuts().last().map_or(0, |&(cut, _)| cut + 1)
    }

    /// The Camera row's label for each number up to [`Self::cut_count`], and
    /// one past it: the camera's name, or empty for a number no camera
    /// holds. The empty last label names every value past the live cameras,
    /// a deleted camera's, which shows the first camera.
    pub fn cut_labels(&self) -> Vec<String> {
        let mut labels = vec![String::new(); self.cut_count() as usize + 1];
        for (cut, id) in self.cuts() {
            if let (Some(label), Some(item)) = (labels.get_mut(cut as usize), self.find(id))
                && label.is_empty()
            {
                label.clone_from(&item.name);
            }
        }
        labels
    }

    /// The param id of the World's Camera row.
    pub fn camera_pick_param(&self) -> Option<String> {
        self.world()
            .map(|world| super::param_id(&world.id, super::CAMERA_PICK))
    }

    /// The camera a Camera row value picks: the camera holding that cut
    /// number, else the first camera.
    pub fn picked_camera(&self, pick: f32) -> Option<&ItemId> {
        let cuts = self.cuts();
        let wanted = pick.is_finite().then(|| pick.round().max(0.0) as u32);
        cuts.iter()
            .find(|&&(cut, _)| Some(cut) == wanted)
            .or(cuts.first())
            .map(|&(_, id)| id)
    }

    /// Whether physics moves `id`, an item at or under it: it, something
    /// inside it, or a group around it has a physics setting.
    pub fn physical_under(&self, id: &ItemId) -> bool {
        let mut any = false;
        self.walk(|item, ancestors| {
            let physical = item.fields.physics() != super::PhysicsSetting::None
                || ancestors
                    .iter()
                    .any(|holder| holder.fields.physics() != super::PhysicsSetting::None);
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

    /// Every file the compile reads facts from: a Group's source, a Mesh
    /// shape's file, a Material's model and its picture file (for its size).
    /// Empty paths included.
    pub fn model_files<'a>(&'a self, mut f: impl FnMut(&'a str)) {
        use super::{Shape, TextureSource};
        self.walk(|item, _| match &item.fields {
            ItemFields::Group(group) => group.source.as_deref().into_iter().for_each(&mut f),
            ItemFields::Object(object) => {
                if let Shape::Mesh { file, .. } = &object.shape {
                    f(file);
                }
            }
            ItemFields::Material(material) => {
                if let TextureSource::Model { file, .. } | TextureSource::File { path: file } =
                    &material.texture
                {
                    f(file);
                }
            }
            _ => {}
        });
    }

    /// Every file path the document names, with the family it collects into:
    /// an import's model file (its Group's source, its parts' meshes, its
    /// materials), an Environment's HDRI, a Material's texture file and a
    /// Displace by Texture file. Empty paths included; the one list Collect,
    /// save and load relink read.
    pub fn files_mut(&mut self, mut f: impl FnMut(crate::file_loader::AssetFamily, &mut String)) {
        use super::{EnvironmentSource, ModifierTexture, Shape, TextureSource};
        use crate::file_loader::AssetFamily;
        fn each(items: &mut [Item], f: &mut impl FnMut(AssetFamily, &mut String)) {
            for item in items {
                match &mut item.fields {
                    ItemFields::Group(group) => {
                        if let Some(source) = &mut group.source {
                            f(AssetFamily::Mesh, source);
                        }
                    }
                    ItemFields::Object(object) => {
                        if let Shape::Mesh { file, .. } = &mut object.shape {
                            f(AssetFamily::Mesh, file);
                        }
                    }
                    ItemFields::Light(light) => {
                        if let LightType::Environment {
                            source:
                                EnvironmentSource::Hdri { file }
                                | EnvironmentSource::SeaHorizon { file },
                        } = &mut light.light
                        {
                            f(AssetFamily::Hdri, file);
                        }
                    }
                    ItemFields::Material(material) => match &mut material.texture {
                        TextureSource::File { path } => f(AssetFamily::Images, path),
                        TextureSource::Model { file, .. } => f(AssetFamily::Mesh, file),
                        TextureSource::None | TextureSource::Layer { .. } => {}
                    },
                    ItemFields::Modifier(modifier) => {
                        if let ModifierTexture::File { path } = &mut modifier.texture {
                            f(AssetFamily::Images, path);
                        }
                    }
                    ItemFields::Camera(_)
                    | ItemFields::Water(_)
                    | ItemFields::WaterSource(_)
                    | ItemFields::Swarm(_)
                    | ItemFields::Force(_)
                    | ItemFields::World(_) => {}
                }
                each(&mut item.children, f);
            }
        }
        each(&mut self.items, &mut f);
    }

    /// Whether any Object or Group has a physics setting.
    pub fn any_physics(&self) -> bool {
        let mut any = false;
        self.walk(|item, _| any |= item.fields.physics() != super::PhysicsSetting::None);
        any
    }
}
