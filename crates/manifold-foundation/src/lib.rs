//! `manifold-foundation` — the shared primitive vocabulary.
//!
//! A zero-dependency leaf crate holding the value objects that the engine
//! (`manifold-core`) and the UI (`manifold-ui`) genuinely share: unit-typed
//! numbers, typed string ids, and the `ParamId` alias. An id is an id and a
//! beat is a beat on both sides, so these are *shared*, not mirrored — there is
//! exactly one definition.
//!
//! `manifold-core` re-exports every item here at its historical path, so
//! existing `manifold_core::Beats` / `manifold_core::id::LayerId` usages are
//! unchanged. `manifold-ui` depends on this crate instead of `manifold-core`,
//! which is what lets the UI compile without the engine.
//!
//! NOTHING with domain semantics belongs here — no registries, no project
//! model, no GPU. Domain types the UI needs are adapted at the app boundary
//! (`docs/UI_LAYERING_INVERSION.md`).

pub mod feature_flags;
pub mod hash;
pub mod id;
pub mod units;

pub use feature_flags::RELIGHT_FEATURE_ENABLED;
pub use hash::{fire_meter_key, fire_meter_key_for_clip_trigger, fire_meter_key_for_param};
pub use id::{AudioSendId, ClipId, EffectGroupId, EffectId, LayerId, MarkerId, NodeId};
pub use units::{Beats, Bpm, Seconds, beats_to_seconds, seconds_to_beats};

/// Stable identifier for a single parameter slot.
///
/// A plain `Cow<'static, str>` so registry-declared params can use cheap
/// `'static` borrows while per-instance user params own their string. The
/// namespace is shared between static and user-exposed params; consumers walk
/// both tiers transparently. (Historically `manifold_core::effects::ParamId`.)
pub type ParamId = std::borrow::Cow<'static, str>;
