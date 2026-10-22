//! `SceneVm`: what the viewport and the setup check read of a scene, built
//! from the scene document (SCENE_DOCUMENT_DESIGN.md section 7.1). A pure
//! function of the document and the layer's live param values: no GPU, no
//! graph, no cache. The Scene tab reads the document itself; this holds only
//! the declared shapes the gizmo draws and the render slots the selection
//! outline needs.

use manifold_core::NodeId;
use manifold_core::scene_doc::{Item, SceneDoc};
use manifold_foundation::ItemId;

mod setup_check;
pub use setup_check::{
    BodyAnchor, CopiesAnchor, RuntimeFit, SetupAnchor, SetupProblem, SetupProblemKind, SetupSite, SetupValues, body_anchors,
    body_problems, copies_anchors, copies_problems, problems_with_runtime,
};
mod shapes;
pub use shapes::{FluidRole, SceneShapeVm, ShapeOwner, ShapeParamVm, ShapeWrite};

/// `node.render_scene`'s own type_id string.
pub const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";

/// A write address for one graph node value: the exact addressing
/// `SetGraphNodeParamCommand::with_scope` takes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParamAddr {
    pub scope_path: Vec<u32>,
    pub node_doc_id: u32,
    pub param_id: String,
}

/// One `node.transform_3d`'s write addresses and current values: Position,
/// Rotation and Scale, each X/Y/Z. Water's domain box reads its setup from it.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformVm {
    pub node_doc_id: u32,
    pub node: NodeId,
    pub pos_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub pos_value: (f32, f32, f32),
    pub pos_driven: (bool, bool, bool),
    pub rot_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub rot_value: (f32, f32, f32),
    pub rot_driven: (bool, bool, bool),
    pub scale_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub scale_value: (f32, f32, f32),
    pub scale_driven: (bool, bool, bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneVm {
    /// Every declared shape: Objects and Groups today; the water, swarm and
    /// force lanes add theirs. Gizmo targets derive from these alone.
    pub shapes: Vec<SceneShapeVm>,
    /// Every setup shape that does not fit its liquid's domain.
    pub problems: Vec<SetupProblem>,
    /// Where each liquid's fill and column params sit, driven or not.
    pub setup_anchors: Vec<SetupAnchor>,
    /// Each liquid's setup as the check reads it, so a value change re-checks
    /// without a rebuild ([`SceneVm::recheck_setup`]).
    pub(crate) setup_sites: Vec<SetupSite>,
    /// The Objects the compiler draws, in render-slot order.
    drawn: Vec<ItemId>,
    /// Each item with the items under it (itself first), for the outline.
    families: Vec<(ItemId, Vec<ItemId>)>,
}

impl SceneVm {
    /// A scene of exactly these shapes, for the gizmo tests.
    #[cfg(test)]
    pub(crate) fn from_shapes(shapes: Vec<SceneShapeVm>) -> SceneVm {
        SceneVm {
            shapes,
            problems: Vec::new(),
            setup_anchors: Vec::new(),
            setup_sites: Vec::new(),
            drawn: Vec::new(),
            families: Vec::new(),
        }
    }

    /// The scene `doc` describes. `values` reads a live manifest param by its
    /// id (`<item id>.<property>`); a param it has no value for reads as its
    /// resting transform value.
    pub fn from_doc(doc: &SceneDoc, values: &dyn Fn(&str) -> Option<f32>) -> SceneVm {
        let mut families = Vec::new();
        doc.walk(|item, _| families.push((item.id.clone(), family(item))));
        SceneVm {
            shapes: shapes::item_shapes(doc, values),
            problems: Vec::new(),
            setup_anchors: Vec::new(),
            setup_sites: Vec::new(),
            drawn: crate::node_graph::scene_compile::drawn_items(doc),
            families,
        }
    }

    /// The render slots that draw `items` and everything inside them, for the
    /// selection outline. Written into `out`, sorted and deduplicated.
    pub fn outline_slots<'a>(&self, items: impl IntoIterator<Item = &'a ItemId>, out: &mut Vec<u32>) {
        out.clear();
        for item in items {
            let Some((_, members)) = self.families.iter().find(|(id, _)| id == item) else { continue };
            out.extend(members.iter().filter_map(|member| self.drawn.iter().position(|id| id == member)).map(|slot| slot as u32));
        }
        out.sort_unstable();
        out.dedup();
    }

    /// The setup check re-run with `value` overriding the authored value of
    /// any param it reads: what a rebuild at those values would find, without
    /// one. `problems` and `anchors` are cleared first.
    pub fn recheck_setup(&self, value: SetupValues<'_>, problems: &mut Vec<SetupProblem>, anchors: &mut Vec<SetupAnchor>) {
        problems.clear();
        anchors.clear();
        for site in &self.setup_sites {
            site.check(&self.shapes, value, problems, anchors);
        }
    }

    /// Every param the setup check reads, with whether a change to it
    /// changes the scene model's form (needs a rebuild) rather than only a
    /// value [`SceneVm::recheck_setup`] takes.
    pub fn setup_params(&self) -> impl Iterator<Item = (&ParamAddr, bool)> {
        self.setup_sites.iter().flat_map(|site| site.params(&self.shapes))
    }
}

fn family(item: &Item) -> Vec<ItemId> {
    let mut ids = vec![item.id.clone()];
    for child in &item.children {
        ids.extend(family(child));
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::scene_doc::{ItemFields, ItemKind};

    fn doc() -> SceneDoc {
        let mut doc = SceneDoc::base();
        let group = Item::new(ItemId::new("g"), "G", ItemFields::default_for(ItemKind::Group));
        let mut group = group;
        group.children.push(Item::new(ItemId::new("o2"), "O2", ItemFields::default_for(ItemKind::Object)));
        doc.items.push(group);
        doc
    }

    #[test]
    fn an_outline_covers_everything_inside_a_group() {
        let doc = doc();
        let vm = SceneVm::from_doc(&doc, &|_| None);
        let mut out = Vec::new();
        vm.outline_slots([&ItemId::new("g")], &mut out);
        assert_eq!(out.len(), 1, "the group draws only its one object: {out:?}");
        let mut all = Vec::new();
        vm.outline_slots(doc.items.iter().map(|i| &i.id), &mut all);
        assert!(all.len() >= out.len());
    }
}
