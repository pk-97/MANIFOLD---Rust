//! Explicit authored modifier addresses for runtime previews.
use manifold_core::{NodeId, scene_modifier_preset::SceneNodeRef};
use crate::node_graph::scene_modifier_expand::SceneModifierNodeRoute;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifierPreviewContext {
    pub modifier_id: NodeId,
    pub scope: Vec<NodeId>,
    pub object: Option<SceneNodeRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierPreviewError {
    MissingNode,
    SelectObject,
    MissingObject,
}

impl ModifierPreviewError {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingNode => "This authored node has no prepared preview output.",
            Self::SelectObject => "Select an object to preview this modifier's per-object output.",
            Self::MissingObject => "The selected object has no prepared copy of this node.",
        }
    }
}

pub(super) fn resolve<'a>(
    routes: &'a [SceneModifierNodeRoute],
    context: &ModifierPreviewContext,
    node: &NodeId,
) -> Result<&'a NodeId, ModifierPreviewError> {
    let route = routes.iter().find(|route| route.modifier_id == context.modifier_id
        && route.local.scope == context.scope && &route.local.node == node)
        .ok_or(ModifierPreviewError::MissingNode)?;
    if let Some(object) = &context.object {
        // Shared controls have one output regardless of object selection.
        return route.copies.iter().find(|copy| copy.object.as_ref() == Some(object))
            .or_else(|| route.copies.iter().find(|copy| copy.object.is_none()))
            .map(|copy| &copy.node_id).ok_or(ModifierPreviewError::MissingObject);
    }
    match route.copies.as_slice() {
        [copy] => Ok(&copy.node_id),
        [] => Err(ModifierPreviewError::MissingNode),
        _ => Err(ModifierPreviewError::SelectObject),
    }
}

impl super::PresetRuntime {
    pub fn modifier_preview_local_node<'a>(
        &'a self,
        context: &ModifierPreviewContext,
        generated: &str,
    ) -> Option<&'a NodeId> {
        self.modifier_preview_routes.iter().filter(|route|
            route.modifier_id == context.modifier_id && route.local.scope == context.scope)
            .find(|route| resolve(&self.modifier_preview_routes, context, &route.local.node)
                .is_ok_and(|node| node.as_str() == generated))
            .map(|route| &route.local.node)
    }

    /// Resolve through prepared routes without allocating or choosing a copy
    /// by ordering. A failed request clears the previously captured output.
    pub fn set_modifier_preview_node(
        &mut self,
        context: &ModifierPreviewContext,
        node: Option<&NodeId>,
    ) -> Result<(), ModifierPreviewError> {
        let resolved = node.map(|node| resolve(&self.modifier_preview_routes, context, node).cloned()).transpose();
        self.set_preview_node(resolved.as_ref().ok().and_then(Option::as_ref));
        resolved.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::scene_modifier_expand::SceneModifierNodeCopy;
    fn address(name: &str) -> SceneNodeRef { SceneNodeRef { scope: vec![], node: NodeId::new(name) } }

    #[test]
    fn scene_modifier_preview_requires_explicit_copy_and_scoped_identity() {
        let route = SceneModifierNodeRoute {
            modifier_id: NodeId::new("a"), local: address("same"),
            copies: vec![
                SceneModifierNodeCopy { object: Some(address("one")), node_id: NodeId::new("generated-one") },
                SceneModifierNodeCopy { object: Some(address("two")), node_id: NodeId::new("generated-two") },
            ],
        };
        let mut nested = route.clone(); nested.local.scope.push(NodeId::new("group"));
        nested.copies[1].node_id = NodeId::new("nested-two");
        let routes = [route, nested];
        let mut context = ModifierPreviewContext { modifier_id: NodeId::new("a"), scope: vec![], object: None };
        assert_eq!(resolve(&routes, &context, &NodeId::new("same")), Err(ModifierPreviewError::SelectObject));
        context.object = Some(address("two"));
        assert_eq!(resolve(&routes, &context, &NodeId::new("same")).unwrap().as_str(), "generated-two");
        context.scope.push(NodeId::new("group"));
        assert_eq!(resolve(&routes, &context, &NodeId::new("same")).unwrap().as_str(), "nested-two");
        context.object = Some(address("deleted"));
        assert_eq!(resolve(&routes, &context, &NodeId::new("same")), Err(ModifierPreviewError::MissingObject));
        context.modifier_id = NodeId::new("b");
        assert_eq!(resolve(&routes, &context, &NodeId::new("same")), Err(ModifierPreviewError::MissingNode));
    }
}
