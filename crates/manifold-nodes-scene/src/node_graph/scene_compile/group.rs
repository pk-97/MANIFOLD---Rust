//! Group: a transform every item inside sits in (transforms chain through
//! nested groups, BUG-8k8ko.2 (nested groups)), and a Visible that
//! multiplies down the tree, so hiding an outer group hides everything in
//! it. Walks the tree and hands each Object its enclosing frame. Lights and
//! cameras inside a group follow its frame too.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Item, ItemFields};

use super::builder::Builder;
use super::copies;

/// What an item inside a group reads from it: the transform node it sits
/// in, the node whose `out` is its visibility, and the copies body whose
/// instances draw it. Under copies, `transform` is the frame inside one copy
/// (`None` at the copied group itself): each instance already carries the
/// copies' world pose.
pub(crate) struct Frame {
    pub transform: Option<u32>,
    pub visible: u32,
    pub copies: Option<u32>,
}

/// `node.math`'s Multiply.
const MULTIPLY: u32 = 2;

/// Emit every Group and Object, depth first in tree order, then put each
/// light and camera inside a group in its frame.
pub(crate) fn emit_tree(b: &mut Builder<'_>) {
    let items = b.doc.items.clone();
    walk(b, &items, None);
    place_rigs(b);
}

fn walk(b: &mut Builder<'_>, items: &[Item], frame: Option<&Frame>) {
    for item in items {
        match &item.fields {
            ItemFields::Group(_) => {
                let inner = emit(b, item, frame);
                let inner = super::physics::group(b, item, inner);
                // Lights and cameras follow the group's world frame: its own
                // transform when its copies place it, nothing extra inside
                // a copy (they follow the copied group).
                let rig_frame = match (inner.transform, inner.copies) {
                    (Some(transform), None) => Some(transform),
                    (None, Some(_)) => b.id(item.id.as_str(), "transform"),
                    _ => None,
                };
                if let Some(rig_frame) = rig_frame {
                    b.group_frames.insert(item.id.clone(), rig_frame);
                }
                if !super::mesh::emit_combined(b, item, &inner) {
                    walk(b, &item.children, Some(&inner));
                }
                b.clock_rows(item);
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
            if let Some(parent) = outer.transform {
                b.wire(parent, "transform", transform, "parent");
            }
            let chain = b.add(owner, "visible_chain", "node.math");
            b.set(chain, "op", SerializedParamValue::Enum { value: MULTIPLY });
            b.wire(own_visible, "out", chain, "a");
            b.wire(outer.visible, "out", chain, "b");
            chain
        }
    };
    // A group with Copies on is placed by its copies: the body takes the
    // group's transform, and what is inside sits at each copy's pose. Inside
    // another group's copies, Copies is that group's.
    let enclosing = outer.and_then(|outer| outer.copies);
    // A Swarm that draws this group as its particles lays it out instead
    // (its copies node, not a copies body).
    let swarm = b.swarm_draws.get(&item.id).copied().filter(|_| enclosing.is_none());
    let own = match &item.fields {
        ItemFields::Group(_) if swarm.is_some() => {
            // With Copies on, the swarm carries the group's own layout.
            if let Some(nest) = swarm.filter(|nest| b.nests.contains_key(nest)) {
                super::swarm::nest_layout(b, item, transform, nest);
            }
            swarm
        }
        ItemFields::Group(group) if group.copies && enclosing.is_none() => {
            Some(copies::emit_group(b, item, transform, group.physics))
        }
        _ => None,
    };
    b.group_visible.insert(item.id.clone(), visible);
    b.rows(item);
    match own {
        Some(body) => Frame { transform: None, visible, copies: Some(body) },
        None => Frame { transform: Some(transform), visible, copies: enclosing },
    }
}

/// Wire each light's and camera's enclosing group frame into its `parent`.
fn place_rigs(b: &mut Builder<'_>) {
    let mut placed = Vec::new();
    b.doc.walk(|item, ancestors| {
        let role = match item.fields {
            ItemFields::Light(_) => "light",
            ItemFields::Camera(_) => "rig",
            _ => return,
        };
        let frame = ancestors.iter().rev().find_map(|group| b.group_frames.get(&group.id).copied());
        if let Some(frame) = frame {
            placed.push((item.id.clone(), role, frame));
        }
    });
    for (item, role, frame) in placed {
        // An Environment has no node of its own to place.
        if let Some(node) = b.id(item.as_str(), role) {
            b.wire(frame, "transform", node, "parent");
        }
    }
}
