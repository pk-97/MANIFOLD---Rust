//! Registry for per-member DERIVED-UNIFORM recompute (design: D7 / P0 amendment,
//! `docs/CINEMATIC_POST_DESIGN.md`).
//!
//! A fused `node.wgsl_compute` kernel has no per-member `run()` any more — the
//! member atoms were deleted and replaced by one merged dispatch. Any member that
//! declares `derived_uniforms()` (a frame-derived value like `dt_scaled`, or a
//! value recomputed from a wired CPU-struct external like a `Camera`'s forward
//! vector) still needs its uniform fields refreshed EVERY FRAME, from the SAME
//! ambient context its unfused `run()` would have read.
//!
//! This module is that per-frame source: a global, `inventory`-based registry —
//! `type_id: &'static str -> recompute fn` — the same "registry lookup by
//! type-id string, data-driven, no closures serialized into the def" shape the
//! `PrimitiveFactory` registry already uses (`node_graph/primitive.rs`,
//! `node_graph/persistence.rs::PrimitiveRegistry::with_builtin`). A primitive
//! registers its recompute once, next to its `primitive!` declaration, via
//! `inventory::submit! { DerivedUniformRecompute { type_id, recompute } }`.
//!
//! Consumers:
//! - `freeze/install.rs` — at fuse-build time, gates region eligibility with
//!   [`has_recompute`]: a member with non-empty `derived_uniforms()` and NO
//!   registered recompute fails the region closed (same fail-safe contract the
//!   old install-time name whitelist had — a refusal always renders unfused,
//!   which is always correct).
//! - `primitives/wgsl_compute.rs` — every frame, in `evaluate()`, calls
//!   [`recompute`] for each `// @derived_uniform_member:` marker the fused
//!   kernel carries, and packs the returned values into the uniform bytes that
//!   marker's fields occupy (see that module's `DerivedUniformMember`).
//!
//! Values travel as `f32` throughout — matching the EXISTING fused-path
//! precision for `frame_count`/etc. (the old whitelist's control wire was
//! `ScalarF32` end to end, cast `as u32` only at the final pack; see
//! `UniformMemberType::write_to`), so this is not a new precision loss, just
//! the same one sourced differently.

use ahash::AHashMap;
use std::sync::OnceLock;

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::FrameTime;

/// Ambient context a recompute fn may read. Mirrors exactly what an unfused
/// `run()` would have had available: the frame clock (`ctx.time`) and — when
/// the member declares a wired `Camera` input port — that camera's live value
/// (`ctx.inputs.camera(name)`). `camera` is `None` for a purely time-derived
/// member (the whole current time-family) and for a camera-derived member
/// whose camera happens to be unwired (defensive; install.rs's routing only
/// ever creates a `camera_ext_N` port when a wire exists, so this should
/// always be `Some` in practice for those members).
pub struct DerivedUniformContext<'a> {
    pub frame: &'a FrameTime,
    pub camera: Option<&'a Camera>,
}

/// One primitive's derived-uniform recompute, submitted via `inventory::submit!`
/// next to its `primitive!` declaration. `recompute` returns the member's
/// `DERIVED_UNIFORMS` values, FLATTENED in declaration order (a `"name:vec3"`
/// entry contributes 3 consecutive values, everything else contributes 1) —
/// exactly the word layout `freeze/codegen.rs`'s derived-uniform Params-struct
/// emission already uses on both the standalone and fused paths. `None` means
/// "can't produce a value this frame" (e.g. a camera-derived recompute called
/// with `ctx.camera = None`) — the caller leaves the field's prior/zeroed bytes,
/// never panics.
pub struct DerivedUniformRecompute {
    pub type_id: &'static str,
    pub recompute: RecomputeFn,
}
inventory::collect!(DerivedUniformRecompute);

/// A registered recompute function's signature (factored out to satisfy
/// clippy's `type_complexity` — see [`DerivedUniformRecompute::recompute`]).
pub type RecomputeFn = fn(&DerivedUniformContext<'_>) -> Option<Vec<f32>>;

fn registry() -> &'static AHashMap<&'static str, RecomputeFn> {
    static REGISTRY: OnceLock<AHashMap<&'static str, RecomputeFn>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        inventory::iter::<DerivedUniformRecompute>()
            .map(|e| (e.type_id, e.recompute))
            .collect()
    })
}

/// Whether `type_id` has a registered recompute. Consulted at fuse-build time
/// (`freeze/install.rs`) so a member with derived uniforms but NO registered
/// recompute fails the region closed — the fail-safe contract the deleted
/// install-time name whitelist had (an unknown name used to `return None`; an
/// unregistered type_id does the same now, data-driven instead of name-matched).
pub fn has_recompute(type_id: &str) -> bool {
    registry().contains_key(type_id)
}

/// Recompute `type_id`'s derived-uniform values for this frame. `None` if
/// unregistered (should not happen for a member that passed the install-time
/// [`has_recompute`] gate — the fused kernel would not carry this member's
/// `@derived_uniform_member` marker otherwise) or if the registered fn itself
/// declines (e.g. no camera wired).
pub fn recompute(type_id: &str, ctx: &DerivedUniformContext<'_>) -> Option<Vec<f32>> {
    registry().get(type_id).and_then(|f| f(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_type_id_has_no_recompute() {
        use manifold_core::{Beats, Seconds};

        assert!(!has_recompute("node.definitely_not_a_real_primitive"));
        let frame = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(0.0),
            frame_count: 0,
        };
        let ctx = DerivedUniformContext { frame: &frame, camera: None };
        assert!(recompute("node.definitely_not_a_real_primitive", &ctx).is_none());
    }
}
