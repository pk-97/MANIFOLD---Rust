//! Project parts and their change stamps (docs/UI_INVALIDATION_DESIGN.md D1).
//!
//! A `Part` names a slice of `Project` with one set of consumers. Every edit
//! declares the parts it touches as a `ChangeSet`; `EditingService` bumps the
//! matching stamps in `PartStamps`. Stamps are never serialized: a load moves
//! every part.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Layers,
    Clips,
    Automation,
    Markers,
    Effects,
    Overrides,
    Values,
    Shapes,
    Mappings,
    MappingRanges,
    EmbeddedPresets,
    Settings,
    Led,
    AudioSetup,
    Media,
    Midi,
    Session,
    ViewPrefs,
}

pub const PART_COUNT: usize = 18;

impl Part {
    pub const ALL: [Part; PART_COUNT] = [
        Part::Layers,
        Part::Clips,
        Part::Automation,
        Part::Markers,
        Part::Effects,
        Part::Overrides,
        Part::Values,
        Part::Shapes,
        Part::Mappings,
        Part::MappingRanges,
        Part::EmbeddedPresets,
        Part::Settings,
        Part::Led,
        Part::AudioSetup,
        Part::Media,
        Part::Midi,
        Part::Session,
        Part::ViewPrefs,
    ];

    const fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ChangeSet(u32);

impl ChangeSet {
    pub const NONE: Self = Self(0);
    pub const ALL: Self = Self((1 << PART_COUNT) - 1);
    /// Live scalars: only ever in-place dependencies, never rebuild inputs (D22).
    pub const LIVE: Self = Self::of(&[Part::Values, Part::Shapes, Part::MappingRanges]);

    pub const fn of(parts: &[Part]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < parts.len() {
            bits |= parts[i].bit();
            i += 1;
        }
        Self(bits)
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn contains(self, part: Part) -> bool {
        self.0 & part.bit() != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn parts(self) -> impl Iterator<Item = Part> {
        Part::ALL.into_iter().filter(move |p| self.contains(*p))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PartStamps([u64; PART_COUNT]);

impl PartStamps {
    pub fn get(&self, part: Part) -> u64 {
        self.0[part as usize]
    }

    pub fn bump(&mut self, set: ChangeSet) {
        for part in set.parts() {
            self.0[part as usize] += 1;
        }
    }

    pub fn moved_since(&self, earlier: &PartStamps) -> ChangeSet {
        let mut bits = 0;
        for part in Part::ALL {
            if self.0[part as usize] != earlier.0[part as usize] {
                bits |= part.bit();
            }
        }
        ChangeSet(bits)
    }

    /// Sum of the stamps in `set`. Stamps only grow, so the key changes exactly
    /// when a part in `set` moved. For consumers that keep one `u64` cache key.
    pub fn key(&self, set: ChangeSet) -> u64 {
        set.parts()
            .map(|p| self.0[p as usize])
            .fold(0u64, u64::wrapping_add)
    }
}

/// How a consumer of a live scalar sees it (D25).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reads {
    /// Reads the project field every tick; no reaction needed.
    Fresh,
    /// Keeps a copy; the Shapes reaction row refreshes it in place.
    CachedWithRow,
}

/// Every Values and Shapes field with its consumers (INV-20). A new live
/// scalar is listed here with each consumer, read fresh or cached with a
/// reaction row; a cached consumer without a row never sees a live drag.
pub type ConsumerRow = (&'static str, Part, &'static [(&'static str, Reads)]);

pub const LIVE_SCALAR_CONSUMERS: &[ConsumerRow] = &[
    (
        "param base and value",
        Part::Values,
        &[("modulation", Reads::Fresh), ("render", Reads::Fresh)],
    ),
    (
        "layer and master opacity",
        Part::Values,
        &[("compositor", Reads::Fresh)],
    ),
    (
        "macro values",
        Part::Values,
        &[("modulation", Reads::Fresh)],
    ),
    (
        "led_brightness, led_gain",
        Part::Values,
        &[("led output", Reads::Fresh)],
    ),
    (
        "driver, envelope and audio-mod shapes",
        Part::Shapes,
        &[("modulation", Reads::Fresh)],
    ),
    (
        "clip-trigger shape",
        Part::Shapes,
        &[("live trigger", Reads::Fresh)],
    ),
    (
        "param-mapping reshape",
        Part::Shapes,
        &[("modulation", Reads::Fresh), ("render", Reads::Fresh)],
    ),
    (
        "unbound graph node param values",
        Part::Shapes,
        &[("render", Reads::Fresh)],
    ),
    (
        "layer.audio_gain_db",
        Part::Shapes,
        &[("audio layer playback", Reads::Fresh)],
    ),
    (
        "audio send gain_db",
        Part::Shapes,
        &[
            ("modulation", Reads::Fresh),
            ("capture GainBank", Reads::CachedWithRow),
        ],
    ),
    (
        "band split low_hz, mid_hz",
        Part::Shapes,
        &[("analysis", Reads::Fresh)],
    ),
];

/// A key over every card param's row fact (`ParamSpecDef::row_fact`): what
/// its value decides about which inspector rows exist. A value write that
/// moves the key changed membership, so it touches Effects whatever it
/// declared (D21). Walks the instances whose row facts the change audit
/// owns under Effects; allocates nothing.
pub fn row_facts_key(project: &crate::project::Project) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut put = |instance: &crate::effects::PresetInstance| {
        for param in instance.params.iter() {
            if let Some(fact) = param.spec.row_fact(param.base) {
                hasher.write_i32(fact);
            }
        }
    };
    project.settings.master_effects.iter().for_each(&mut put);
    for layer in &project.timeline.layers {
        layer.effects.iter().flatten().for_each(&mut put);
        layer.gen_params().into_iter().for_each(&mut put);
    }
    hasher.finish()
}

#[cfg(feature = "change-audit")]
pub mod audit;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_moves_only_for_parts_in_the_set() {
        let mut s = PartStamps::default();
        let k = s.key(ChangeSet::of(&[Part::Layers, Part::Effects]));
        s.bump(ChangeSet::of(&[Part::Values]));
        assert_eq!(k, s.key(ChangeSet::of(&[Part::Layers, Part::Effects])));
        s.bump(ChangeSet::of(&[Part::Effects]));
        assert_ne!(k, s.key(ChangeSet::of(&[Part::Layers, Part::Effects])));
    }

    /// D25: live scalars live only in Values or Shapes, and the one cached
    /// consumer today is the send-gain `GainBank` (the `sync_gains` row).
    #[test]
    fn live_scalar_table_names_only_live_parts() {
        let mut cached = Vec::new();
        for (field, part, consumers) in LIVE_SCALAR_CONSUMERS {
            assert!(
                matches!(part, Part::Values | Part::Shapes),
                "{field} is not a live scalar part"
            );
            assert!(!consumers.is_empty(), "{field} lists no consumer");
            cached.extend(
                consumers
                    .iter()
                    .filter(|(_, r)| *r == Reads::CachedWithRow)
                    .map(|(c, _)| (*field, *c)),
            );
        }
        assert_eq!(cached, vec![("audio send gain_db", "capture GainBank")]);
    }

    #[test]
    fn moved_since_names_bumped_parts() {
        let a = PartStamps::default();
        let mut b = a;
        b.bump(ChangeSet::of(&[Part::Clips, Part::Shapes]));
        assert_eq!(
            b.moved_since(&a),
            ChangeSet::of(&[Part::Clips, Part::Shapes])
        );
        assert!(ChangeSet::ALL.contains(Part::ViewPrefs));
        assert!(!ChangeSet::LIVE.contains(Part::Effects));
    }
}
