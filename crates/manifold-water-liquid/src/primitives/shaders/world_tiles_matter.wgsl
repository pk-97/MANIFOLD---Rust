// The points of a world tiles build from matter points (`MatterPoint`, 160
// bytes, ten vec4: position and id first), bound
// at 13: one of the first point_count records is live when its id is
// non-zero and its position finite, binned by `ts_world_cell` like every
// other particle pass.

@group(0) @binding(13) var<storage, read> wt_matter: array<vec4<f32>>;

fn wt_point_live(i: u32) -> bool {
    if i >= wt.point_count {
        return false;
    }
    let r = wt_matter[10u * i];
    let e = bitcast<vec3<u32>>(r.xyz) & vec3<u32>(0x7f800000u);
    return bitcast<u32>(r.w) != 0u && all(e != vec3<u32>(0x7f800000u));
}

fn wt_point_cell(i: u32) -> vec3<i32> {
    return ts_world_cell(wt_matter[10u * i].xyz, wt.origin, wt.inv_cell);
}
