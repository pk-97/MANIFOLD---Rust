// node.render_scene: a cut-out draw's depth, drawn in the main pass ahead of
// its shading. A fragment shader that can discard stops Apple's hidden-
// surface removal, so stacked cut-out cards (a far forest) would each be
// fully shaded. This entry runs only the tests the shading entries discard
// on: the volume-optics nearest test, the appearance fade and the alpha
// cut-out. The same draw then shades with the solid-surface pipeline (no
// discard) under depth Equal, so each covered sample is shaded once.
//
// It returns the alpha the shading entries return for a cut-out, so
// alpha-to-coverage dithers an appearance fade the same way. Its colour is
// overwritten by the shading draw on every sample it covers. The signature
// spells `vec4f` and the volume test names `nearest_depth` so the aux and
// solid-surface substitutions, which rewrite the shading entries, leave this
// entry alone.

@fragment
fn fs_cutout_depth(in: VsOut) -> @location(0) vec4f {
    begin_surface(in.box_pos, in.box_normal, in.box_axis_x, in.box_axis_y, in.box_axis_z, in.world_pos, in.clip_pos.xy, in.uv, in.world_normal);
    if u.volume_optics.x > 0.5 {
        let nearest_depth = textureLoad(volume_nearest, vec2<i32>(in.clip_pos.xy), 0);
        if in.clip_pos.z < nearest_depth - max(nearest_depth * 0.00001, 0.0000001) { discard; }
    }
    let albedo = resolve_albedo(in.uv, in.vertex_color);
    if appearance_discard(in.appearance_weight) { discard; }
    if albedo.a < u.alpha_params.y { discard; }
    return vec4f(0.0, 0.0, 0.0, clamp(u.appearance.x * in.appearance_weight, 0.0, 1.0));
}
