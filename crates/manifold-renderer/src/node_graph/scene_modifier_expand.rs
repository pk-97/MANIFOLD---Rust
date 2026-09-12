//! Structural preparation of authored scene modifiers.
//!
//! The canonical snapshot is never mutated by preparation. Live controls are
//! ordinary graph bindings; no per-frame attachment work belongs here.

mod bindings;
mod buffer_budget;
pub use bindings::SceneModifierBindingSource;
pub use buffer_budget::{
    MODIFIER_BUFFER_LIMIT_BYTES, ModifierBufferUsage, PreparedModifierBufferBudget,
};
mod compiler;
mod control_state;
pub use control_state::PreparedModifierControlState;
mod frames;
mod index;
mod namespace;
mod parameter_guards;
pub(crate) use parameter_guards::PreparedModifierParameterGuards;
mod routes;
mod value_sources;
mod value_writes;
pub use value_sources::{SceneModifierValueSource, SceneModifierValueSourcePlan};
pub use value_writes::PreparedGraphValueWrites;

pub use compiler::{expand_scene_modifiers, prepare_scene_modifiers, validate_modifier_attachment, validate_modifier_runtime};
pub use routes::{PreparedSceneModifierGraph, SceneModifierNodeCopy, SceneModifierNodeRoute};

pub use frames::{resolve_modifier_mesh_frames, validate_modifier_mesh_frames};

use manifold_core::scene_modifier_preset::SceneModifierSchemaError;

macro_rules! expansion_errors {
    ($($name:ident),+ $(,)?) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum SceneModifierExpandError {
            $($name { path: String, detail: String }),+
        }

        impl std::fmt::Display for SceneModifierExpandError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(Self::$name { path, detail } =>
                        write!(f, "{} at {path}: {detail}", stringify!($name))),+
                }
            }
        }

        impl std::error::Error for SceneModifierExpandError {}
    };
}

expansion_errors! {
    UnsupportedVersion, MissingScene, AmbiguousScene, MissingTarget,
    DuplicateIdentity, InvalidRecipe, UnsupportedEndpoint,
    UnsupportedCoordinateFrame, UnsupportedRenderMode, MissingInput,
    ConflictingSource, RecursiveModifier, InvalidBinding, CapacityExceeded,
}

impl From<SceneModifierSchemaError> for SceneModifierExpandError {
    fn from(error: SceneModifierSchemaError) -> Self {
        match error {
            SceneModifierSchemaError::UnsupportedVersion { path, detail } => {
                Self::UnsupportedVersion { path, detail }
            }
            SceneModifierSchemaError::MissingTarget { path, detail } => {
                Self::MissingTarget { path, detail }
            }
            SceneModifierSchemaError::DuplicateIdentity { path, detail } => {
                Self::DuplicateIdentity { path, detail }
            }
            SceneModifierSchemaError::InvalidRecipe { path, detail } => {
                Self::InvalidRecipe { path, detail }
            }
            SceneModifierSchemaError::UnsupportedCoordinateFrame { path, detail } => {
                Self::UnsupportedCoordinateFrame { path, detail }
            }
            SceneModifierSchemaError::RecursiveModifier { path, detail } => {
                Self::RecursiveModifier { path, detail }
            }
            SceneModifierSchemaError::InvalidBinding { path, detail } => {
                Self::InvalidBinding { path, detail }
            }
            SceneModifierSchemaError::CapacityExceeded { path, detail } => {
                Self::CapacityExceeded { path, detail }
            }
        }
    }
}
