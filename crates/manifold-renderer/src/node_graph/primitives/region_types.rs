//! Wire records shared by the V2 region detector and tracker primitives.
//!
//! These records are copied directly through mapped Channels buffers. Their
//! layout is therefore part of the graph ABI, rather than an implementation
//! detail of either primitive.

use std::mem::{offset_of, size_of};

pub const MAX_REGIONS: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Region {
    pub label: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub area: f32,
    pub cx: f32,
    pub cy: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackRecord {
    pub id: u32,
    pub label: u32,
    pub observed: u32,
    pub age: f32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub cx: f32,
    pub cy: f32,
    pub vx: f32,
    pub vy: f32,
    pub area: f32,
    pub pad0: u32,
    pub pad1: u32,
    pub pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LegacyBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

const _: () = {
    assert!(size_of::<Region>() == 32);
    assert!(offset_of!(Region, label) == 0);
    assert!(offset_of!(Region, x) == 4);
    assert!(offset_of!(Region, y) == 8);
    assert!(offset_of!(Region, width) == 12);
    assert!(offset_of!(Region, height) == 16);
    assert!(offset_of!(Region, area) == 20);
    assert!(offset_of!(Region, cx) == 24);
    assert!(offset_of!(Region, cy) == 28);

    assert!(size_of::<TrackRecord>() == 64);
    assert!(offset_of!(TrackRecord, id) == 0);
    assert!(offset_of!(TrackRecord, label) == 4);
    assert!(offset_of!(TrackRecord, observed) == 8);
    assert!(offset_of!(TrackRecord, age) == 12);
    assert!(offset_of!(TrackRecord, x) == 16);
    assert!(offset_of!(TrackRecord, y) == 20);
    assert!(offset_of!(TrackRecord, width) == 24);
    assert!(offset_of!(TrackRecord, height) == 28);
    assert!(offset_of!(TrackRecord, cx) == 32);
    assert!(offset_of!(TrackRecord, cy) == 36);
    assert!(offset_of!(TrackRecord, vx) == 40);
    assert!(offset_of!(TrackRecord, vy) == 44);
    assert!(offset_of!(TrackRecord, area) == 48);
    assert!(offset_of!(TrackRecord, pad0) == 52);
    assert!(offset_of!(TrackRecord, pad1) == 56);
    assert!(offset_of!(TrackRecord, pad2) == 60);

    assert!(size_of::<LegacyBox>() == 16);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_v2_region_and_track_wire_layout() {
        assert_eq!(size_of::<Region>(), 32);
        assert_eq!(size_of::<TrackRecord>(), 64);
        assert_eq!(size_of::<LegacyBox>(), 16);
        assert_eq!(offset_of!(TrackRecord, area), 48);
        assert_eq!(offset_of!(TrackRecord, pad2), 60);
    }
}
