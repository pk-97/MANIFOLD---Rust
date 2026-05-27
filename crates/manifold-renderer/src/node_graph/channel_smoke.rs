//! Phase 0 — End-to-end smoke test for the Channel type system.
//!
//! Throwaway. The types and predicates in this module exist ONLY to prove
//! that the design described in `docs/CHANNEL_TYPE_SYSTEM.md` §4-§5 works
//! end-to-end before Phase 1 hardens the real type infrastructure in
//! `ports.rs` / `validation.rs`. Phase 1 deletes this module wholesale
//! when the production types ship.
//!
//! Target: `EdgePair` — the smallest typed family in the catalog
//! (8 bytes, two `u32` channels `a_index` and `b_index`).

#![cfg(test)]

use crate::generators::mesh_common::EdgePair;

// ─── Stub types (deleted in Phase 1) ──────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ChannelName(u64);

impl ChannelName {
    const fn from_str(s: &'static str) -> Self {
        Self(const_fnv1a_64(s.as_bytes()))
    }
}

const fn const_fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(1099511628211);
        i += 1;
    }
    hash
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)] // I32 / Vec*F unused in the EdgePair smoke; Phase 1 ships them all
enum ChannelElementType {
    F32,
    I32,
    U32,
    Vec2F,
    Vec3F,
    Vec4F,
}

impl ChannelElementType {
    const fn size(self) -> u32 {
        match self {
            Self::F32 | Self::I32 | Self::U32 => 4,
            Self::Vec2F => 8,
            Self::Vec3F => 12,
            Self::Vec4F => 16,
        }
    }

    const fn alignment(self) -> u32 {
        match self {
            Self::F32 | Self::I32 | Self::U32 => 4,
            Self::Vec2F => 8,
            Self::Vec3F | Self::Vec4F => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ChannelSpec {
    name: ChannelName,
    ty: ChannelElementType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MatchMode {
    Exact,
    Permissive,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // item_align is part of the type contract but unused by the smoke predicate
struct ChannelArrayType {
    specs: &'static [ChannelSpec],
    item_size: u32,
    item_align: u32,
    match_mode: MatchMode,
}

// ─── Layout calculator (per §4.4) ─────────────────────────────────────

fn std430_layout(specs: &[ChannelSpec]) -> (Vec<u32>, u32, u32) {
    let mut offset: u32 = 0;
    let mut max_align: u32 = 4;
    let mut offsets = Vec::with_capacity(specs.len());
    for spec in specs {
        let align = spec.ty.alignment();
        let size = spec.ty.size();
        offset = round_up(offset, align);
        offsets.push(offset);
        offset += size;
        max_align = max_align.max(align);
    }
    let stride = round_up(offset, max_align);
    (offsets, stride, max_align)
}

fn round_up(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

fn make_array_type(
    specs: &'static [ChannelSpec],
    match_mode: MatchMode,
) -> ChannelArrayType {
    let (_, stride, align) = std430_layout(specs);
    ChannelArrayType {
        specs,
        item_size: stride,
        item_align: align,
        match_mode,
    }
}

// ─── Compatibility predicate (per §5.2) ───────────────────────────────

fn channels_compatible(producer: ChannelArrayType, consumer: ChannelArrayType) -> bool {
    match consumer.match_mode {
        MatchMode::Exact => producer.specs == consumer.specs,
        MatchMode::Permissive => true,
    }
}

// ─── Channel names + EdgePair signature ───────────────────────────────

const A_INDEX: ChannelName = ChannelName::from_str("a_index");
const B_INDEX: ChannelName = ChannelName::from_str("b_index");
const C_INDEX: ChannelName = ChannelName::from_str("c_index"); // negative-test only

const EDGE_PAIR_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: A_INDEX, ty: ChannelElementType::U32 },
    ChannelSpec { name: B_INDEX, ty: ChannelElementType::U32 },
];

// ─── Tests ────────────────────────────────────────────────────────────

#[test]
fn fnv_hash_is_deterministic_and_distinguishes_strings() {
    assert_eq!(
        ChannelName::from_str("a_index"),
        ChannelName::from_str("a_index"),
        "same string → same hash"
    );
    assert_ne!(
        ChannelName::from_str("a_index"),
        ChannelName::from_str("b_index"),
        "different strings → different hash"
    );
}

#[test]
fn stride_and_offsets_match_edgepair_struct() {
    let (offsets, stride, align) = std430_layout(EDGE_PAIR_SPECS);
    assert_eq!(offsets, vec![0, 4], "per-channel offsets");
    assert_eq!(
        stride as usize,
        std::mem::size_of::<EdgePair>(),
        "Channels stride must equal the Pod struct's size_of"
    );
    assert_eq!(stride, 8);
    assert_eq!(align, 4);
}

#[test]
fn matching_signatures_connect_under_exact() {
    let producer = make_array_type(EDGE_PAIR_SPECS, MatchMode::Exact);
    let consumer = make_array_type(EDGE_PAIR_SPECS, MatchMode::Exact);
    assert!(channels_compatible(producer, consumer));
}

#[test]
fn different_channel_count_rejects() {
    const THREE_CHANNELS: &[ChannelSpec] = &[
        ChannelSpec { name: A_INDEX, ty: ChannelElementType::U32 },
        ChannelSpec { name: B_INDEX, ty: ChannelElementType::U32 },
        ChannelSpec { name: C_INDEX, ty: ChannelElementType::U32 },
    ];
    let producer = make_array_type(EDGE_PAIR_SPECS, MatchMode::Exact);
    let consumer = make_array_type(THREE_CHANNELS, MatchMode::Exact);
    assert!(!channels_compatible(producer, consumer));
}

#[test]
fn different_channel_name_rejects() {
    const RENAMED_SECOND: &[ChannelSpec] = &[
        ChannelSpec { name: A_INDEX, ty: ChannelElementType::U32 },
        ChannelSpec { name: C_INDEX, ty: ChannelElementType::U32 },
    ];
    let producer = make_array_type(EDGE_PAIR_SPECS, MatchMode::Exact);
    let consumer = make_array_type(RENAMED_SECOND, MatchMode::Exact);
    assert!(!channels_compatible(producer, consumer));
}

#[test]
fn different_element_type_rejects() {
    const FLOAT_FIRST: &[ChannelSpec] = &[
        ChannelSpec { name: A_INDEX, ty: ChannelElementType::F32 },
        ChannelSpec { name: B_INDEX, ty: ChannelElementType::U32 },
    ];
    let producer = make_array_type(EDGE_PAIR_SPECS, MatchMode::Exact);
    let consumer = make_array_type(FLOAT_FIRST, MatchMode::Exact);
    assert!(!channels_compatible(producer, consumer));
}

#[test]
fn permissive_consumer_accepts_arbitrary_producer() {
    const UNRELATED: &[ChannelSpec] = &[
        ChannelSpec { name: A_INDEX, ty: ChannelElementType::Vec4F },
    ];
    let unrelated = make_array_type(UNRELATED, MatchMode::Exact);
    let permissive = make_array_type(EDGE_PAIR_SPECS, MatchMode::Permissive);
    assert!(channels_compatible(unrelated, permissive));

    let matching = make_array_type(EDGE_PAIR_SPECS, MatchMode::Exact);
    assert!(channels_compatible(matching, permissive));
}

#[test]
fn byte_layout_roundtrips_through_edgepair_pod_struct() {
    // The "bind → read" leg of the smoke test. Allocate a host-side buffer
    // sized through the new ArrayType's `item_size`; write via the
    // existing `EdgePair` Pod struct (what every `Array(EdgePair)` site
    // does at the GPU bind boundary today); read back via the new
    // std430-calculated offsets. Byte-for-byte match proves the new
    // wire-type tag describes the same allocation the existing path uses.
    const MAX_CAPACITY: usize = 16;

    let array_type = make_array_type(EDGE_PAIR_SPECS, MatchMode::Exact);
    let buffer_size = (array_type.item_size as usize) * MAX_CAPACITY;
    let mut buffer = vec![0u8; buffer_size];

    // Existing path writes through the Pod struct.
    let pairs = [
        EdgePair { a: 0, b: 1 },
        EdgePair { a: 2, b: 3 },
        EdgePair { a: u32::MAX, b: u32::MAX },
    ];
    let pair_bytes = bytemuck::cast_slice::<EdgePair, u8>(&pairs);
    buffer[..pair_bytes.len()].copy_from_slice(pair_bytes);

    // New path reads through Channels offsets.
    let (offsets, stride, _) = std430_layout(EDGE_PAIR_SPECS);
    let a_offset = offsets[0] as usize;
    let b_offset = offsets[1] as usize;
    let stride = stride as usize;

    for (i, expected) in pairs.iter().enumerate() {
        let base = i * stride;
        let a = u32::from_ne_bytes(
            buffer[base + a_offset..base + a_offset + 4].try_into().unwrap(),
        );
        let b = u32::from_ne_bytes(
            buffer[base + b_offset..base + b_offset + 4].try_into().unwrap(),
        );
        assert_eq!(a, expected.a, "pair {i} a");
        assert_eq!(b, expected.b, "pair {i} b");
    }
}
