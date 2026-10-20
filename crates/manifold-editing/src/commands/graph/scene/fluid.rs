//! Fluid roles: a scene object's role in a liquid domain. Adding the liquid
//! itself is a scene template (`crate::scene_template`).

#[cfg(test)]
mod tests;

mod roles;
pub(super) use roles::{
    disconnect_scene_object_fluid_roles, duplicate_scene_object_fluid_roles,
    loose_scene_object_has_fluid_roles, remove_loose_scene_object_fluid_roles,
};
pub use roles::{
    AssignSceneFluidRoleCommand, RemoveSceneFluidRoleCommand, RetargetSceneFluidRoleCommand,
    SceneFluidRoleAssignment, restore_scene_object_fluid_roles, scene_fluid_role_assignments,
    scene_fluid_role_eligibility,
};
