//! Unified target identifier for graph-editing commands.
//!
//! Effects and generators both carry an internal `EffectGraphDef` that
//! the graph editor mutates. The editing commands (Add / Remove /
//! Connect / Disconnect / Move / SetParam / Revert) are uniform across
//! both — they don't care whether the graph backs an effect instance
//! or a layer's generator. [`GraphTarget`] is the typed handle that
//! tells the command where to find (and persist) the graph.
//!
//! Resolution sites:
//!
//! - [`GraphTarget::Effect`] resolves to
//!   [`crate::effects::PresetInstance::graph`] on the effect with the
//!   given [`crate::id::EffectId`]. Version counter:
//!   [`crate::effects::PresetInstance::graph_version`].
//! - [`GraphTarget::Generator`] resolves to
//!   [`crate::layer::Layer::generator_graph`] on the layer with the
//!   given [`crate::id::LayerId`]. Version counter:
//!   [`crate::layer::Layer::generator_graph_version`].
//!
//! Both override fields are `Option<EffectGraphDef>`: `None` means the
//! runtime uses the catalog default (the bundled JSON preset for that
//! effect/generator type), `Some(def)` means the user has edited the
//! graph through the editor and the override is authoritative.

use serde::{Deserialize, Serialize};

use crate::id::{EffectId, LayerId};

/// Identifies which graph an editing command should mutate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GraphTarget {
    /// An effect instance's per-card graph. Resolves to
    /// `PresetInstance::graph` via `Project::find_effect_by_id_mut`.
    Effect(EffectId),
    /// A layer's per-layer generator graph. Resolves to
    /// `Layer::generator_graph` via `Project::timeline::find_layer_by_id_mut`.
    Generator(LayerId),
}

impl GraphTarget {
    /// Short human-readable string suitable for logs / error messages.
    pub fn label(&self) -> String {
        match self {
            Self::Effect(eid) => format!("effect/{}", eid.as_str()),
            Self::Generator(lid) => format!("generator/{}", lid.as_str()),
        }
    }
}
