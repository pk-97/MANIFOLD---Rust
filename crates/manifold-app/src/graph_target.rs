//! Shared read-side graph selection for editor, export and preset commands.
use manifold_core::{GraphTarget, effect_graph_def::EffectGraphDef, project::Project};

/// The public macro owned by this local editor. Preparation controls have no
/// live mapping even if a malformed external file supplies a host binding.
pub(crate) fn modifier_host_binding<'a>(
    project: &'a Project,
    target: &GraphTarget,
    macro_id: &str,
) -> Option<&'a manifold_core::effect_graph_def::BindingDef> {
    use manifold_core::effect_graph_def::BindingTarget;
    let GraphTarget::SceneModifier { modifier_id, .. } = target else {
        return None;
    };
    let local = resolve(project, target)?.preset_metadata.as_ref()?;
    let host = resolve(project, target.host_target()?)?
        .preset_metadata
        .as_ref()?;
    let mut matches = host.bindings.iter().filter(|binding| binding.id == macro_id
        && matches!(&binding.target, BindingTarget::SceneModifier { modifier_id: mid, param_id }
            if mid == modifier_id
                && !local.scene_modifier.as_ref().is_some_and(|recipe| recipe.preparation_params.contains(param_id))
                && local.bindings.iter().any(|leaf| &leaf.id == param_id)));
    let binding = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    project.graph_target_owner(target)?.params.get(macro_id)?;
    Some(binding)
}

/// Borrow the authored graph, preserving an instance's local modifier snapshot.
/// Catalog defaults are resolved for the owner before selecting the local graph.
pub(crate) fn resolve<'a>(
    project: &'a Project,
    target: &GraphTarget,
) -> Option<&'a EffectGraphDef> {
    let owner = project.graph_target_owner(target)?;
    let default = manifold_renderer::node_graph::bundled_preset_def(owner.effect_type());
    project.graph_for_target(target, default)
}

/// Derived editor routes keep the authored local nodes while addressing the
/// owner's public controls. Called only when rebuilding a snapshot.
pub(crate) fn modifier_bindings(
    project: &Project,
    target: &GraphTarget,
) -> Option<Vec<manifold_core::effect_graph_def::BindingDef>> {
    use manifold_core::effect_graph_def::BindingTarget;
    let GraphTarget::SceneModifier { modifier_id, .. } = target else {
        return None;
    };
    let local = resolve(project, target)?.preset_metadata.as_ref()?;
    let owner = resolve(project, target.host_target()?)?
        .preset_metadata
        .as_ref()?;
    let mut bindings = Vec::new();
    for outer in &owner.bindings {
        let BindingTarget::SceneModifier {
            modifier_id: mid,
            param_id,
        } = &outer.target
        else {
            continue;
        };
        if mid != modifier_id {
            continue;
        }
        for leaf in local
            .bindings
            .iter()
            .filter(|binding| &binding.id == param_id)
        {
            let mut combined = leaf.clone();
            combined.id = outer.id.clone();
            combined.label = outer.label.clone();
            combined.default_value = outer.default_value;
            combined.scale = outer.scale * leaf.scale;
            combined.offset = outer.offset * leaf.scale + leaf.offset;
            bindings.push(combined);
        }
    }
    Some(bindings)
}

/// A complete host default, suitable for undoable first-edit materialization.
pub(crate) fn owner_default(project: &Project, target: &GraphTarget) -> Option<EffectGraphDef> {
    let owner = project.graph_target_owner(target)?;
    if let Some(graph) = owner.graph.as_ref() {
        return Some(graph.clone());
    }
    resolve(project, target.host_target()?).cloned()
}

/// Catalog comparison snapshot in host coordinates. Commands still receive a
/// complete owner graph; the canvas selects the local graph with its target.
pub(crate) fn catalog_default(project: &Project, target: &GraphTarget) -> Option<EffectGraphDef> {
    if matches!(target, GraphTarget::SceneModifier { .. }) {
        let mut owner = resolve(project, target.host_target()?)?.clone();
        let local = resolve(project, target)?;
        let baseline = match crate::modifier_preset::library_baseline(&owner, local) {
            Ok(baseline) => baseline?,
            Err(error) => {
                log::error!("[preset] editor baseline unavailable: {error}");
                return None;
            }
        };
        *target.graph_in_mut(&mut owner)? = baseline;
        Some(owner)
    } else {
        let owner = project.graph_target_owner(target)?;
        manifold_renderer::node_graph::bundled_preset_def(owner.effect_type()).cloned()
    }
}
