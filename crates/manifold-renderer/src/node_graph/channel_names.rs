//! Canonical channel-name registry for the Channel type system.
//!
//! Every primitive that produces or consumes a channel with a meaning
//! shared across the catalog uses a constant from `well_known` below.
//! Primitives that need a name not in the registry either (a) add it
//! to the registry (if reusable), or (b) declare it as an inline
//! string literal (if genuinely local — see §7.4).
//!
//! Hard rule: a primitive declaring a `Channels[...]` port should
//! reach for `well_known::*` constants by default. Inline string
//! literals are the deliberate exception, not the rule.
//!
//! ## Adding a name
//!
//! Append one line inside the `well_known_channels!` invocation below
//! in the appropriate category. The `pub const` and the collision-
//! check coverage are emitted from the same source list — see §7.5.

use crate::node_graph::ports::ChannelName;

/// Generates the `well_known::*` constants and the corresponding hash
/// collision test from a single source list.
///
/// Syntax: a flat list of `NAME = "string"` entries separated by `;`.
/// Categories are marked by `// ─── ... ───` line comments inside the
/// invocation — purely documentary, ignored by the macro.
///
/// Emits, into the surrounding module:
/// - `pub const NAME: ChannelName = ChannelName::from_str("string");`
///   for each entry.
/// - `pub static ALL_WELL_KNOWN: &[(ChannelName, &str)]` — every entry
///   in declaration order, used by `debug_name` lookup.
/// - `#[cfg(test)] mod collision_tests` with a pairwise-distinct hash
///   assertion across the whole list.
macro_rules! well_known_channels {
    ( $( $name:ident = $string:literal );* $(;)? ) => {
        $(
            #[doc = concat!("`", $string, "` — canonical channel name.")]
            pub const $name: ChannelName = ChannelName::from_str($string);
        )*

        /// Every well-known channel name paired with its source string,
        /// in declaration order. Used by [`debug_name`] to recover the
        /// original string for error messages and editor display.
        pub static ALL_WELL_KNOWN: &[(ChannelName, &'static str)] = &[
            $( ($name, $string), )*
        ];

        #[cfg(test)]
        mod collision_tests {
            use super::*;

            #[test]
            fn all_well_known_channel_names_have_distinct_hashes() {
                for (i, (ch_a, name_a)) in ALL_WELL_KNOWN.iter().enumerate() {
                    for (ch_b, name_b) in &ALL_WELL_KNOWN[i + 1..] {
                        assert_ne!(
                            ch_a, ch_b,
                            "Channel-name FNV-1a-64 hash collision: \
                             well_known::{} (\"{}\") and well_known::{} (\"{}\") \
                             both hash to {:#018x}. Rename one of the constants \
                             in well_known_channels! to break the collision.",
                            name_a.to_uppercase().replace(' ', "_"),
                            name_a,
                            name_b.to_uppercase().replace(' ', "_"),
                            name_b,
                            ch_a.hash(),
                        );
                    }
                }
            }

            #[test]
            fn registry_has_canonical_names_present() {
                // Spot-check a handful of names that downstream primitive
                // migrations are documented to reach for. If any go
                // missing the corresponding migration fails compile;
                // catching it here surfaces the missing entry with a
                // cleaner error than a downstream "undefined symbol".
                for required in [
                    "x", "y", "z", "w",
                    "position", "velocity", "normal", "uv",
                    "width", "height",
                    "r", "g", "b", "a", "color",
                    "a_index", "b_index",
                    "life", "age", "seed",
                    "pos_scale", "rot",
                    "value", "t", "index",
                    "magnitude", "confidence",
                ] {
                    let needle = ChannelName::from_str(required);
                    assert!(
                        ALL_WELL_KNOWN.iter().any(|(c, _)| *c == needle),
                        "well_known registry missing required canonical name `{required}`",
                    );
                }
            }
        }
    };
}

pub mod well_known {
    use super::ChannelName;

    well_known_channels! {
        // ─── Spatial axes ───────────────────────────────────────────
        X = "x";
        Y = "y";
        Z = "z";
        W = "w";

        // ─── Vector positions (when not decomposed into x/y/z) ──────
        POSITION = "position";
        VELOCITY = "velocity";
        NORMAL   = "normal";
        TANGENT  = "tangent";
        UV       = "uv";
        XY       = "xy";

        // ─── Rectangle / box geometry ───────────────────────────────
        WIDTH  = "width";
        HEIGHT = "height";

        // ─── Color ──────────────────────────────────────────────────
        R     = "r";
        G     = "g";
        B     = "b";
        A     = "a";
        COLOR = "color";

        // ─── Edge topology ──────────────────────────────────────────
        A_INDEX = "a_index";
        B_INDEX = "b_index";

        // ─── Particle attributes ────────────────────────────────────
        LIFE = "life";
        AGE  = "age";
        SEED = "seed";

        // ─── Instance transforms ────────────────────────────────────
        POS_SCALE = "pos_scale";
        ROT       = "rot";

        // ─── Generic scalar / control ───────────────────────────────
        VALUE     = "value";
        T         = "t";
        INDEX     = "index";
        MAGNITUDE = "magnitude";
        PHASE     = "phase";
        FREQ      = "freq";

        // ─── Confidence / probability / weight (DNN, FFI, classifiers) ─
        CONFIDENCE = "confidence";
        WEIGHT     = "weight";

        // ─── Optical flow / motion vectors (Texture2D channel layouts) ──
        FLOW_X = "flow_x";
        FLOW_Y = "flow_y";
        VALID  = "valid";

        // ─── Per-pixel mask / coverage (Texture2D channel layouts) ──────
        // Broadcast SDF / focus / coverage values from a mask generator
        // to a downstream consumer's R channel. Pair with VALID on alpha
        // for the standard mask-texture convention.
        MASK = "mask";
        DEPTH = "depth";

        // ─── 4x4 matrix columns (joint palette buffers) ──────────────
        // GLTF_ANIMATION_DESIGN.md A2: node.gltf_skeleton_pose's
        // Array(JointMatrix) output — one skin matrix per joint, column-
        // major (matches gltf_load::Mat4's own convention).
        MAT_COL0 = "mat_col0";
        MAT_COL1 = "mat_col1";
        MAT_COL2 = "mat_col2";
        MAT_COL3 = "mat_col3";
    }
}

/// Best-effort lookup of the source string for a `ChannelName`.
///
/// Two registries consulted in order:
/// 1. Compile-time [`well_known::ALL_WELL_KNOWN`] — canonical channel
///    names declared once per process. Linear scan; fast for the
///    well-known set's few-dozen entries.
/// 2. Runtime overflow map populated via [`register_runtime_name`] —
///    covers names introduced by `wgsl_compute` shader field parsing
///    (per `docs/CHANNEL_TYPE_SYSTEM.md` §8.4). Bounded by the
///    distinct field-name set across all `wgsl_compute` shaders
///    loaded in a session; in practice tiny.
///
/// Returns `None` for names that have never been seen by either
/// registry — those render as a hex hash in error messages and
/// editor tooltips.
pub fn debug_name(ch: ChannelName) -> Option<&'static str> {
    if let Some(s) = well_known::ALL_WELL_KNOWN
        .iter()
        .find(|(c, _)| *c == ch)
        .map(|(_, s)| *s)
    {
        return Some(s);
    }
    runtime_names()
        .read()
        .expect("runtime channel-name registry poisoned")
        .get(&ch)
        .copied()
}

/// Register a runtime-introduced channel name so [`debug_name`] can
/// recover its source string. `name` must be `'static` — `wgsl_compute`
/// already leaks WGSL field names through `leak_str`; callers with a
/// non-`'static` String should leak via
/// `Box::leak(s.to_string().into_boxed_str())`.
///
/// Idempotent: re-registering an identical (ch, name) pair is fine.
/// If a different name registers against the same hash later, the
/// later one wins (an outcome the hash-collision test in
/// `well_known::collision_tests` makes astronomically unlikely for
/// the well-known set; runtime names can in principle collide with
/// each other but the practical risk is the same).
pub fn register_runtime_name(ch: ChannelName, name: &'static str) {
    // Fast path: already registered with the same name.
    if let Some(existing) = runtime_names()
        .read()
        .expect("runtime channel-name registry poisoned")
        .get(&ch)
        && *existing == name
    {
        return;
    }
    runtime_names()
        .write()
        .expect("runtime channel-name registry poisoned")
        .insert(ch, name);
}

fn runtime_names()
-> &'static std::sync::RwLock<ahash::AHashMap<ChannelName, &'static str>>
{
    static MAP: std::sync::OnceLock<
        std::sync::RwLock<ahash::AHashMap<ChannelName, &'static str>>,
    > = std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::RwLock::new(ahash::AHashMap::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_name_recovers_well_known() {
        assert_eq!(debug_name(well_known::X), Some("x"));
        assert_eq!(debug_name(well_known::POSITION), Some("position"));
        assert_eq!(debug_name(well_known::COLOR), Some("color"));
    }

    #[test]
    fn debug_name_returns_none_for_unknown() {
        let unknown = ChannelName::from_str("not_in_registry_qqq");
        assert_eq!(debug_name(unknown), None);
    }

    #[test]
    fn debug_name_method_on_channel_name_matches_free_fn() {
        let ch = well_known::POSITION;
        assert_eq!(ch.debug_name(), debug_name(ch));
        assert_eq!(ch.debug_name(), Some("position"));
    }
}
