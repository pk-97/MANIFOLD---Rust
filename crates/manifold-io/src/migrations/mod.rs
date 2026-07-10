//! Quarantined one-time schema migrations that are big/self-contained
//! enough to deserve their own module instead of a function in
//! `crate::migrate`. See `docs/PARAM_STORAGE_DESIGN.md` §4 (D4) for the
//! first (and so far only) resident.

pub mod param_storage_v14;
pub mod scene_transform_v1120;
