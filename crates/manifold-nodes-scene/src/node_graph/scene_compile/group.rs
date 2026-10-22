//! Group: a transform every item inside sits in (transforms chain through
//! nested groups, BUG-8k8ko.2 (nested groups)), and a Visible that
//! multiplies down the tree, so hiding an outer group hides everything in
//! it. Walks the tree and hands each Object its enclosing frame.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Item, ItemFields};

use super::builder::Builder;

/// What an item inside a group reads from it: the group's transform node and
/// the node whose `out` is its visibility, both outputs.
pub(crate) struct Frame {
    pub transform: u32,
    pub visible: u32,
}

/// `node.math`'s Multiply.
const MULTIPLY: u32 = 2;

/// Emit every Group and Object, depth first in tree order.
pub(crate) fn emit_tree(b: &mut Builder<'_>) {
    let items = b.doc.items.clone();
    walk(b, &items, None);
}

fn walk(b: &mut Builder<'_>, items: &[Item], frame: Option<&Frame>) {
    for item in items {
        match &item.fields {
            ItemFields::Group(_) => {
                let inner = emit(b, item, frame);
                walk(b, &item.children, Some(&inner));
            }
            ItemFields::Object(_) => super::object::emit(b, item, frame),
            _ => {}
        }
    }
}

fn emit(b: &mut Builder<'_>, item: &Item, outer: Option<&Frame>) -> Frame {
    let owner = item.id.as_str();
    let transform = b.add(owner, "transform", "node.transform_3d");
    let own_visible = b.add(owner, "visible", "node.value");
    let visible = match outer {
        None => own_visible,
        Some(outer) => {
            b.wire(outer.transform, "transform", transform, "parent");
            let chain = b.add(owner, "visible_chain", "node.math");
            b.set(chain, "op", SerializedParamValue::Enum { value: MULTIPLY });
            b.wire(own_visible, "out", chain, "a");
            b.wire(outer.visible, "out", chain, "b");
            chain
        }
    };
    b.rows(item);
    Frame { transform, visible }
}
