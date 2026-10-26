// node.render_scene — multi-object 3D scene renderer: N objects sharing
// ONE depth buffer (real occlusion) lit by up to 4 lights. Forked from
// render_3d_mesh.wgsl (the single-object renderer): keeps every
// Material-system M4/M6 addition (resolve_albedo / resolve_metallic,
// alpha-cutout discard, view-facing normal flip) byte-identical, and
// adds exactly two things render_3d_mesh doesn't need:
//
//   1. A per-object `model` matrix, composed CPU-side from that
//      object's pos/rot/scale params (see
//      node_graph/primitives/render_scene.rs::model_matrix). One draw
//      call per object, same shared depth texture — the CPU side draws
//      object 0 with GpuLoadAction::Clear and objects 1..N with
//      GpuLoadAction::Load, so the depth test resolves real occlusion
//      between objects instead of each rendering into its own buffer.
//   2. A runtime-sized `lights: array<vec4<f32>>` buffer (4
//      vec4s each — position/direction and mode, premultiplied colour
//      and caster slot, range/falloff/cone cosines, then spot forward) so the PBR/Cel
//      entry points sum every wired light's direct term instead of
//      reading exactly one `light_dir`/`light_color` pair. Ambient and
//      emission are added exactly once (after the light loop), not
//      blended per light.
//
// Shadows (P2) via the caster table + PCF below; atmosphere/fog (P3).
// Per-object surface textures:
// base_color_map (M6 addendum) plus normal_map (tangent-space, D4
// cotangent-frame reconstruction), mr_map (glTF-packed roughness/metallic),
// occlusion_map, and emissive_map — each gated by a texture_flags/
// texture_flags2 presence bit, unwired = always-bind dummy stub (P8
// pattern), byte-identical output. ONE envmap is shared across every PBR
// object in the scene (an environment map is scene-wide, not per-object).
//
// MeshVertex layout (80 bytes including authored tangent and RGBA colour), entry point
// names, and per-kind dispatch: identical to render_3d_mesh.wgsl.

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
// Per-map-family samplers, each built
// Rust-side from that glTF texture's OWN wrapS/wrapT/filter (default
// REPEAT+linear when the texture has no explicit `sampler`) — no longer one
// scene-wide hardcoded-REPEAT sampler. envmap_sampler (binding 3) stays
// clamp-V for the equirect poles and is never reused here; sharing it with
// material maps was the 2026-07-15 whole-helmet-smeared-to-the-texture-edge
// bug that motivated the original single material_sampler fix (`85b5bb9d`),
// which this per-family split still preserves (unwired/no-sampler default
// is byte-identical REPEAT+linear).
@group(0) @binding(22) var base_color_sampler: sampler;
@group(0) @binding(23) var normal_sampler: sampler;
@group(0) @binding(24) var mr_sampler: sampler;
@group(0) @binding(25) var occlusion_sampler: sampler;
@group(0) @binding(26) var emissive_sampler: sampler;
// binding(4): normal_map_n (D3/F-P2) — tangent-space glTF normal map,
// gated by texture_flags.x, reconstructed via the screen-space cotangent
// frame in resolve_normal below (D4). Unwired binds the 1×1 dummy, flag
// stays 0 — always-bind stub pattern (render_scene.rs P8 precedent).
// binding(5)/(7): roughness_map/metallic_map — permanently dead stubs
// (D3 rejected a channel-select mode flag on these; superseded by the
// dedicated mr_map at binding(19) below). Always bound to the 1×1 dummy;
// texture_flags.y/w never set from Rust.
@group(0) @binding(4) var normal_map: texture_2d<f32>;
@group(0) @binding(5) var roughness_map: texture_2d<f32>;
@group(0) @binding(6) var base_color_map: texture_2d<f32>;
@group(0) @binding(7) var metallic_map: texture_2d<f32>;

// Optional closed-volume optical paths, embedded bubble density and nearest surface.
@group(0) @binding(53) var volume_path: texture_2d<f32>;
@group(0) @binding(54) var volume_density: texture_2d<f32>;
@group(0) @binding(55) var volume_nearest: texture_depth_2d;
// Last frame's vertex positions (xyz, w = 1), one record per vertex record of
// `verts`, for a draw whose mesh deforms. Read only when `u.render_mode.z` is
// set; every other draw binds a one-record stand-in.
@group(0) @binding(56) var<storage, read> prev_positions: array<vec4<f32>>;
// Last frame's instance records, one per instance this draw reads, for a draw
// whose instance array moves on its own. Read only when `u.render_mode.w` is
// set; every other draw binds the identity instance stub.
@group(0) @binding(57) var<storage, read> prev_instances: array<Instance>;

// Per-object instancing (REALTIME_3D_DESIGN.md section 10 D11+P8). One always-bound
// storage buffer per object: wired instances_n binds the real buffer,
// unwired binds a cached 1-entry identity stub (pos_scale [0,0,0,1],
// rot_pad zeros) drawn with instance_count 1 — the same always-bind
// ABI-stub pattern as the shadow bindings above. Layout matches
// generators::mesh_common::InstanceTransform (32 bytes) and
// render_instanced_3d_mesh.wgsl's `Instance` exactly.
struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};
@group(0) @binding(15) var<storage, read> instances: array<Instance>;

// The three NEW per-object maps (normal
// reuses binding(4) above). Always bound (same always-bind ABI-stub
// discipline as bindings 4/5/7/10-14) — unwired binds the 1×1 dummy,
// gated off by texture_flags2's presence bits.
@group(0) @binding(19) var mr_map: texture_2d<f32>;
@group(0) @binding(20) var occlusion_map: texture_2d<f32>;
@group(0) @binding(21) var emissive_map: texture_2d<f32>;

// Sheen/iridescence/anisotropy extension textures.
// Always bound (same always-bind ABI-stub discipline as bindings 19-21),
// gated by the presence flags packed into texture_flags/texture_flags2/
// anisotropy_dispersion_params/transmission_volume_params's reserved `w`/
// `y` slots (render_scene.rs's E3/E4/E5 comment). Sampled with the
// existing base_color_sampler/mr_sampler bindings rather than five more
// dedicated samplers — see render_scene.rs's binding(29) comment.
@group(0) @binding(29) var sheen_color_map: texture_2d<f32>;
@group(0) @binding(30) var sheen_roughness_map: texture_2d<f32>;
@group(0) @binding(31) var iridescence_map: texture_2d<f32>;
@group(0) @binding(32) var iridescence_thickness_map: texture_2d<f32>;
@group(0) @binding(33) var anisotropy_map: texture_2d<f32>;

// Texture completion: clearcoat/specular/transmission/volume-thickness extension
// textures. Same always-bind ABI-stub discipline + shared-sampler
// simplification as 29-33 above. Every reserved single-flag `w` slot the
// E1 migration left was already spent by E3/E4/E5 (see the struct's
// `pbr_specular_tint`/`volume_attenuation_color` field comments), so these
// seven presence flags are packed as BITMASKS into those two `w` slots
// instead of growing the uniform again — see `specular_family_flags`/
// `clearcoat_family_flags` below.
@group(0) @binding(34) var clearcoat_map: texture_2d<f32>;
@group(0) @binding(35) var clearcoat_roughness_map: texture_2d<f32>;
@group(0) @binding(36) var clearcoat_normal_map: texture_2d<f32>;
@group(0) @binding(37) var specular_map: texture_2d<f32>;
@group(0) @binding(38) var specular_color_map: texture_2d<f32>;
@group(0) @binding(39) var transmission_map: texture_2d<f32>;
@group(0) @binding(40) var volume_thickness_map: texture_2d<f32>;
// Optional per-vertex appearance weights. Unwired draws bind their existing
// vertex buffer here as an unused ABI dummy; the vertex shader never reads it
// unless `u.appearance.y` is set.
@group(0) @binding(46) var<storage, read> weights: array<f32>;
@group(0) @binding(47) var diffuse_transmission_map: texture_2d<f32>;
@group(0) @binding(48) var diffuse_transmission_color_map: texture_2d<f32>;

struct MaterialMapUniform {
    matrix: vec4<f32>,
    offset_set: vec4<f32>,
    sampling: vec4<u32>,
};
struct MaterialMaps { maps: array<MaterialMapUniform, 19>, };
@group(0) @binding(51) var<uniform> subsurface_binding: vec4<f32>;
@group(0) @binding(52) var subsurface_radiance: texture_2d<f32>;
@group(0) @binding(50) var<uniform> material_maps: MaterialMaps;

fn material_uv(uv: vec4<f32>, index: u32) -> vec2<f32> {
    let m = material_maps.maps[index];
    let xy = select(uv.xy, uv.zw, m.offset_set.z == 1.0);
    return vec2<f32>(dot(m.matrix.xy, xy), dot(m.matrix.zw, xy)) + m.offset_set.xy;
}

fn map_address(i: i32, size: i32, mode: u32) -> i32 {
    if mode == 1u { return ((i % size) + size) % size; }
    if mode == 2u {
        let mirrored = ((i % (2 * size)) + 2 * size) % (2 * size);
        return min(mirrored, 2 * size - mirrored - 1);
    }
    return clamp(i, 0, size - 1);
}

fn map_texel(tex: texture_2d<f32>, p: vec2<i32>, level: i32, wrap: vec2<u32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(tex, level));
    let valid = !((wrap.x == 3u && (p.x < 0 || p.x >= size.x))
        || (wrap.y == 3u && (p.y < 0 || p.y >= size.y)));
    let xy = vec2<i32>(map_address(p.x, size.x, wrap.x), map_address(p.y, size.y, wrap.y));
    return select(vec4<f32>(0.0), textureLoad(tex, xy, level), valid);
}

fn map_level(tex: texture_2d<f32>, uv: vec2<f32>, level: i32, wrap: vec2<u32>, linear: bool) -> vec4<f32> {
    let p = uv * vec2<f32>(textureDimensions(tex, level));
    if !linear { return map_texel(tex, vec2<i32>(floor(p)), level, wrap); }
    let base = vec2<i32>(floor(p - 0.5));
    let t = fract(p - 0.5);
    let a = mix(map_texel(tex, base, level, wrap), map_texel(tex, base + vec2<i32>(1, 0), level, wrap), t.x);
    let b = mix(map_texel(tex, base + vec2<i32>(0, 1), level, wrap), map_texel(tex, base + vec2<i32>(1, 1), level, wrap), t.x);
    return mix(a, b, t.y);
}

// Extension maps keep independent wrap, texel and mip filters without
// allocating a hardware sampler slot for every possible map family.
fn sample_extension_map(tex: texture_2d<f32>, uv: vec4<f32>, index: u32) -> vec4<f32> {
    let coord = material_uv(uv, index);
    let settings = material_maps.maps[index].sampling;
    let dim = vec2<f32>(textureDimensions(tex, 0));
    let footprint = max(length(dpdx(coord) * dim), length(dpdy(coord) * dim));
    let lod = clamp(log2(max(footprint, 1e-8)) + u.render_mode.y, 0.0, f32(textureNumLevels(tex) - 1u));
    let linear = select((settings.z & 1u) != 0u, (settings.z & 2u) != 0u, footprint > 1.0);
    if settings.w == 0u { return map_level(tex, coord, 0, settings.xy, linear); }
    if settings.w == 1u { return map_level(tex, coord, i32(round(lod)), settings.xy, linear); }
    let low = i32(floor(lod));
    let high = min(low + 1, i32(textureNumLevels(tex)) - 1);
    return mix(map_level(tex, coord, low, settings.xy, linear),
        map_level(tex, coord, high, settings.xy, linear), fract(lod));
}

// Decoded once per fragment (uniform per-draw-call data, cheap): bit i of
// `pbr_specular_tint.w` = (specular_map, specular_color_map,
// transmission_map, volume_thickness_map) present; bit i of
// `volume_attenuation_color.w` = (clearcoat_map, clearcoat_roughness_map,
// clearcoat_normal_map) present. Default 0.0 (no bits set) is
// byte-identical to before this bitmask existed.
fn specular_family_flags() -> u32 {
    return u32(round(u.pbr_specular_tint.w));
}
fn clearcoat_family_flags() -> u32 {
    return u32(round(u.volume_attenuation_color.w));
}

// The EMIT_VELOCITY pipeline variant's
// FsOut struct is text-substituted in here (replacing this comment) so
// fs_unlit/fs_pbr/fs_cel can return a second MRT output
// (`@location(1) velocity`) alongside `color`. Inert (a no-op comment) in
// the base/velocity-off compile — GBUFFER_FSOUT_VELOCITY_STRUCT is the
// substitution marker `render_scene.rs` targets.
// GBUFFER_FSOUT_VELOCITY_STRUCT

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec4<f32>,
    // EMIT_VELOCITY substitutes
    // `@location(3) clip_now: vec4<f32>, @location(4) clip_prev: vec4<f32>,`
    // here — the CURRENT and PREVIOUS clip-space positions, carried as
    // plain (perspective-correctly interpolated) varyings rather than
    // reusing `@builtin(position)` (whose fragment-stage value is already
    // viewport-transformed with `w` replaced by `1/w_clip` — not usable
    // for the NDC divide the velocity fragment code needs). Inert comment
    // in the velocity-off compile.
    // GBUFFER_VSOUT_VELOCITY_FIELDS
    // BUG-wfxe: authored tangent (xyz world dir, w bitangent sign).
    // w == 0 = no authored tangent → derived-frame fallback. @location(5)
    // because EMIT_VELOCITY takes 3 and 4.
    @location(5) world_tangent: vec4<f32>,
    @location(6) appearance_weight: f32,
    @location(8) vertex_color: vec4<f32>,
    @location(9) @interpolate(flat) instance_scale: f32,
};

// world = model · T_instance · instance_inner. Object space passes an
// identity `instance_inner`, so the instance TRS applies first and the
// object's `model` second (D11); Group space passes the part's own
// transform as `instance_inner`, so each instance places the whole group.
// An identity `instance_inner` reproduces every input exactly. An identity instance
// (unwired instances_n) collapses `rot` to the identity matrix and
// `inst_pos`/`inst_normal` to `v.position`/`v.normal` unchanged, so this
// is byte-identical to the pre-P8 vertex shader for every unwired object.
//
// The body is a shared helper (not inlined in vs_main) so vs_points can
// reuse it verbatim — ONE source of truth for the instance/mirror/tangent
// transform chain. spirv-opt's InlineExhaustive pass inlines it back into
// every caller, so vs_main's compiled output is unchanged by the split.
fn scene_vs_body(vid: u32, iid: u32) -> VsOut {
    let v = verts[vid];
    let inst = instances[iid];
    let rot = euler_xyz(inst.rot_pad.xyz);
    // Mirrored instances store rotation R' = M R M, translation
    // t' = M(t - d a) + d a, scale kept positive. Exactness (two lines):
    // world' = R'(w M v) + t'
    //             = (M R M)(M w v) + t' = M R (w v) + M(t - d a) + d a,
    // the true planar reflection; the normal follows identically,
    // R'(M n) = M R n — so the flip lands BEFORE the stored rotation
    // (flipped after, it would be R M n, wrong by the same conjugation).
    // Unmarked instances take msign = (1,1,1) — byte-identical to the
    // pre-mirror shader.
    let msign = mirror_sign(inst.rot_pad.w);
    let inner = u.instance_inner;
    let inner_cofactor = mat3x3<f32>(
        cross(inner[1].xyz, inner[2].xyz),
        cross(inner[2].xyz, inner[0].xyz),
        cross(inner[0].xyz, inner[1].xyz),
    );
    let inner_sign = select(1.0, -1.0, dot(inner[0].xyz, inner_cofactor[0]) < 0.0);
    let local_pos = (inner * vec4<f32>(v.position, 1.0)).xyz;
    let local_normal = inner_cofactor * v.normal * inner_sign;
    let local_tangent = (inner * vec4<f32>(v.tangent.xyz, 0.0)).xyz;
    let inst_pos = rot * ((local_pos * msign) * inst.pos_scale.w) + inst.pos_scale.xyz;
    // Read only by the velocity variant's GBUFFER_VS_VELOCITY_BODY. A vertex
    // of a deforming mesh starts from where it was last frame; any other
    // vertex has not moved in its own space.
    var prev_vertex = v.position;
    if u.render_mode.z > 0.5 {
        prev_vertex = prev_positions[vid].xyz;
    }
    // An instance of an array that moves on its own starts from where it was
    // last frame; any other instance has not moved in its own space.
    var prev_rot = rot;
    var prev_msign = msign;
    var prev_pos_scale = inst.pos_scale;
    if u.render_mode.w > 0.5 {
        let prev_inst = prev_instances[iid];
        prev_rot = euler_xyz(prev_inst.rot_pad.xyz);
        prev_msign = mirror_sign(prev_inst.rot_pad.w);
        prev_pos_scale = prev_inst.pos_scale;
    }
    let prev_local_pos = (u.prev_instance_inner * vec4<f32>(prev_vertex, 1.0)).xyz;
    let prev_inst_pos = prev_rot * ((prev_local_pos * prev_msign) * prev_pos_scale.w) + prev_pos_scale.xyz;
    let scale_sign = select(1.0, -1.0, inst.pos_scale.w < 0.0);
    let inst_normal = rot * (local_normal * msign * scale_sign);

    var out: VsOut;
    let world = u.model * vec4<f32>(inst_pos, 1.0);
    out.world_pos = world.xyz;
    out.vertex_color = v.color;
    out.instance_scale = abs(inst.pos_scale.w);
    out.clip_pos = u.view_proj * world;
    // EMIT_VELOCITY substitutes
    // `out.clip_now = out.clip_pos; let prev_world = u.prev_model *
    // vec4<f32>(prev_inst_pos, 1.0); out.clip_prev = u.prev_view_proj *
    // prev_world;` here. `prev_inst_pos` carries the object's and the
    // instance space's previous transforms and, for a mesh that deforms, the
    // vertex's own previous position and, for an instance array that moves on
    // its own (a swarm, physics copies, a scatter that re-scatters), the
    // instance's own previous record. Any other instance reuses this frame's
    // TRS. Inert comment in the velocity-off compile.
    // GBUFFER_VS_VELOCITY_BODY
    // Cofactors give the inverse transpose up to determinant magnitude.
    // Preserve its sign for reflected models, without dividing by a tiny scale.
    let cofactor = mat3x3<f32>(
        cross(u.model[1].xyz, u.model[2].xyz),
        cross(u.model[2].xyz, u.model[0].xyz),
        cross(u.model[0].xyz, u.model[1].xyz),
    );
    let model_sign = select(1.0, -1.0, dot(u.model[0].xyz, cofactor[0]) < 0.0);
    out.world_normal = normalize(cofactor * inst_normal * model_sign);
    out.uv = vec4<f32>(v.uv, v._pad2);
    // BUG-wfxe: tangent transforms with the same instance-rotation + model
    // chain as the normal (a direction); the mirror marker flips it with
    // the same sign vector (M R t = R' M t). w carries the bitangent sign
    // through unscaled; w == 0 (no authored tangent) passes the zero
    // sentinel through — normalize() of a zero vector would be NaN, so the
    // direction is selected, not branched (uniform control flow).
    let t_world = normalize((u.model * vec4<f32>(rot * (local_tangent * msign * scale_sign), 0.0)).xyz);
    let handedness = model_sign * inner_sign * msign.x * msign.y * msign.z * scale_sign;
    out.world_tangent = vec4<f32>(select(t_world, vec3<f32>(0.0), v.tangent.w == 0.0), v.tangent.w * handedness);
    if u.appearance.y > 0.5 {
        out.appearance_weight = weights[vid];
    } else {
        out.appearance_weight = 1.0;
    }
    return out;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VsOut {
    return scene_vs_body(vid, iid);
}

// SCENE_RENDER_MODE_DESIGN.md D8 (P3): the Points pipeline's vertex output.
// Identical user varyings to VsOut (the points pipeline pairs with
// fs_unlit, which reads locations 0/2/6 only — the rest ride along
// untouched) plus the point size. The size member is authored as a plain
// `@location(7) f32` because WGSL has no point_size builtin; the pipeline
// is created via `create_render_pipeline_depth_msaa_point_size`, which
// rebinds this member to `BuiltIn::PointSize` before SPIR-V emission so
// SPIRV-Cross emits `[[point_size]]`. A separate struct (not VsOut plus a
// field) because a PointSize builtin may not appear in a fragment entry's
// inputs — and every fs below takes `VsOut`.
struct VsOutPoints {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec4<f32>,
    @location(5) world_tangent: vec4<f32>,
    @location(6) appearance_weight: f32,
    @location(7) point_size: f32,
    @location(8) vertex_color: vec4<f32>,
    @location(9) @interpolate(flat) instance_scale: f32,
};

// Points mode draws the SAME vertex buffers with point topology — every
// vertex becomes a dot. No velocity/ao_mask fields: the points pipeline is
// the plain (no-aux) variant, so under Points those targets stay at their
// clear values for the frame (documented v1 scope — Points is a shading
// audit mode, not a temporal-upscale input).
@vertex
fn vs_points(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VsOutPoints {
    let o = scene_vs_body(vid, iid);
    return VsOutPoints(
        o.clip_pos,
        o.world_pos,
        o.world_normal,
        o.uv,
        o.world_tangent,
        o.appearance_weight,
        u.render_mode.x,
        o.vertex_color,
        o.instance_scale,
    );
}

// Appearance weights are a coverage/brightness control. Values below one
// reduce alpha (and therefore coverage where alpha-to-coverage is active),
// while values above one brighten HDR output without exceeding opacity 1.
fn apply_appearance(rgb: vec3<f32>, alpha: f32, weight: f32) -> vec4<f32> {
    let level = u.appearance.x * weight;
    let coverage = select(1.0, alpha, u.alpha_params.x == 2.0);
    return vec4<f32>(rgb * max(level, 1.0), coverage * clamp(level, 0.0, 1.0));
}

fn appearance_discard(weight: f32) -> bool {
    return u.appearance.x * weight <= 0.0;
}

// Tangent-space (glTF-convention) normal
// mapping. The FALLBACK frame is the screen-space cotangent frame
// (Mikkelsen's derivation, the technique three.js/filament use when no
// vertex tangents exist) — used when the mesh carries no authored TANGENT
// (`tangent.w == 0`, every procedural mesh and pre-BUG-wfxe import). Built
// purely from `dpdx`/`dpdy` of `world_pos` and `uv` — both screen-space
// derivatives, so the reconstructed T/B are a function of the surface's
// own UV parameterization, independent of camera or screen resolution.
// Uniform per-object branch (texture_flags.x is a per-draw-call uniform,
// not per-fragment data), so `dpdx`/`dpdy` inside the `if` are legal (same
// discipline the PCSS branches above already rely on).
fn cotangent_frame(n: vec3<f32>, p: vec3<f32>, uv: vec2<f32>) -> mat3x3<f32> {
    let dp1 = dpdx(p);
    let dp2 = dpdy(p);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);

    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    let b = dp2perp * duv1.y + dp1perp * duv2.y;

    let inv_max = inverseSqrt(max(max(dot(t, t), dot(b, b)), 1e-16));
    return mat3x3<f32>(t * inv_max, b * inv_max, n);
}

// BUG-wfxe: the TBN selector. Both frames are COMPUTED unconditionally
// and selected column-wise — cotangent_frame uses `dpdx`/`dpdy`, which
// require uniform control flow, so a per-fragment `if tangent.w != 0`
// branch around it fails naga validation (fs_pbr). The authored arm
// (Gram-Schmidt + glTF handedness sign: B = cross(N,T) * w) produces NaN
// on the zero sentinel (normalize of a zero vector) — discarded by the
// select, never read.
fn tbn_for(n: vec3<f32>, p: vec3<f32>, uv: vec2<f32>, tangent: vec4<f32>) -> mat3x3<f32> {
    let derived = cotangent_frame(n, p, uv);
    let t = normalize(tangent.xyz - n * dot(tangent.xyz, n));
    let b = cross(n, t) * tangent.w;
    let authored = mat3x3<f32>(t, b, n);
    let use_derived = tangent.w == 0.0;
    return mat3x3<f32>(
        select(authored[0], derived[0], use_derived),
        select(authored[1], derived[1], use_derived),
        n,
    );
}

// Identical to render_3d_mesh.wgsl's SIGNATURE — see that file for the
// world-space-map path it still uses. render_scene's normal_map_n (D3) is
// tangent-space (glTF convention: R/G = tangent-space X/Y in [-1,1] packed
// to [0,1], B = tangent-space Z), reconstructed into world space via the
// authored tangent frame when present (BUG-wfxe), the cotangent frame
// otherwise.
fn resolve_normal(uv: vec4<f32>, vertex_normal: vec3<f32>, world_pos: vec3<f32>, tangent: vec4<f32>) -> vec3<f32> {
    if u.texture_flags.x > 0.5 {
        if (u32(u.appearance.z) & 1u) != 0u {
            return normalize(textureSampleBias(normal_map, normal_sampler, uv.xy, u.render_mode.y).xyz);
        }
        let n = normalize(vertex_normal);
        let uv_t = material_uv(uv, 1u);
        let sampled = textureSampleBias(normal_map, normal_sampler, uv_t, u.render_mode.y).rgb;
        var tangent_normal = sampled * 2.0 - vec3<f32>(1.0);
        tangent_normal = vec3<f32>(tangent_normal.xy * u.normal_uv_t.z, tangent_normal.z);
        // glTF TANGENT is the authored normal-map frame. Only meshes
        // without it derive a frame from the selected, transformed UVs.
        let tbn = tbn_for(n, world_pos, uv_t, tangent);
        return normalize(tbn * tangent_normal);
    }
    return normalize(vertex_normal);
}

// Apply the base-color
// KHR_texture_transform's folded affine — `uv' = M*uv + t`, folded ONCE at
// import time (gltf_load::fold_uv_transform), never per frame. Identity
// (m=(1,0,0,1), t=(0,0)) reduces to `uv` exactly (no branch needed: 1*u +
// 0*v + 0 == u bit-for-bit in f32).
fn apply_uv_transform(uv: vec2<f32>, m: vec4<f32>, t: vec4<f32>) -> vec2<f32> {
    return vec2<f32>(
        m.x * uv.x + m.y * uv.y + t.x,
        m.z * uv.x + m.w * uv.y + t.y,
    );
}

fn resolve_albedo(uv: vec4<f32>, color: vec4<f32>) -> vec4<f32> {
    if u.texture_flags.z > 0.5 {
        let uv_t = material_uv(uv, 0u);
        let t = textureSampleBias(base_color_map, base_color_sampler, uv_t, u.render_mode.y);
        return u.base_color * t * color;
    }
    return u.base_color * color;
}

// glTF metallic-roughness packing
// (G = roughness, B = metallic) in ONE dedicated texture — NOT a
// channel-select mode on the (now-dead) roughness_map/metallic_map
// bindings, per D3's explicit rejection of that shape. Returns
// (roughness, metallic).
fn resolve_mr(uv: vec4<f32>) -> vec2<f32> {
    if u.texture_flags2.x > 0.5 {
        let uv_t = material_uv(uv, 2u);
        let t = textureSampleBias(mr_map, mr_sampler, uv_t, u.render_mode.y);
        // glTF's packed channels multiply the material factors.  A texture
        // is a modulation map, not a replacement for the authored values.
        return vec2<f32>(
            max(u.pbr_metallic_roughness.y * t.g, 0.01),
            clamp(u.pbr_metallic_roughness.x * t.b, 0.0, 1.0),
        );
    }
    var roughness = u.pbr_metallic_roughness.y;
    var metallic = u.pbr_metallic_roughness.x;
    // Separate red-channel maps are the original mesh-node absolute inputs.
    if (u32(u.appearance.z) & 2u) != 0u {
        roughness = textureSample(roughness_map, envmap_sampler, uv.xy).r;
    }
    if (u32(u.appearance.z) & 4u) != 0u {
        metallic = textureSample(metallic_map, envmap_sampler, uv.xy).r;
    }
    return vec2<f32>(max(roughness, 0.01), clamp(metallic, 0.0, 1.0));
}

// R-channel ambient occlusion. Unwired
// = 1.0 (no darkening) — used ONLY to darken fs_pbr's diffuse IBL term
// (never direct lighting, never specular IBL), per the design's Invariants
// table.
fn resolve_occlusion(uv: vec4<f32>) -> f32 {
    if u.texture_flags2.y > 0.5 {
        let uv_t = material_uv(uv, 3u);
        return mix(1.0, textureSampleBias(occlusion_map, occlusion_sampler, uv_t, u.render_mode.y).r, u.occlusion_uv_t.z);
    }
    return 1.0;
}

// sRGB emissive map, multiplied by the
// material's own (already premultiplied-with-intensity) emission factor and the
// per-object `emission.w` skin strength multiplier. Unwired = the material's
// emission factor scaled by `emission.w` (byte-identical to before this port
// existed when `emission.w` is 1.0). Used in EVERY entry point (fs_unlit
// included, per M6-D1's albedo precedent) — emission is always added AFTER
// lighting.
fn resolve_emissive(uv: vec4<f32>) -> vec3<f32> {
    let strength = u.emission.w;
    if u.texture_flags2.z > 0.5 {
        let uv_t = material_uv(uv, 4u);
        let t = textureSampleBias(emissive_map, emissive_sampler, uv_t, u.render_mode.y).rgb;
        return u.emission.rgb * strength * t;
    }
    return u.emission.rgb * strength;
}

// `KHR_materials_sheen`
// factors + optional textures. `sheenColorTexture` is sRGB (tinted by
// `sheenColorFactor`), `sheenRoughnessTexture`'s ALPHA channel carries
// roughness (tinted by `sheenRoughnessFactor`) per the spec. v1
// simplification: no `KHR_texture_transform` support on these two maps
// (unlike the base five) — sampled at the raw mesh UV; acceptable because
// no Compare/gate asset in this doc's manifest combines sheen with a UV
// transform on the sheen maps specifically.
fn resolve_sheen(uv: vec4<f32>) -> vec4<f32> {
    var color = u.sheen_params.rgb;
    if u.texture_flags.y > 0.5 {
        color = color * sample_extension_map(sheen_color_map, uv, 5u).rgb;
    }
    var roughness = u.sheen_params.w;
    if u.texture_flags.w > 0.5 {
        roughness = roughness * sample_extension_map(sheen_roughness_map, uv, 6u).a;
    }
    return vec4<f32>(color, roughness);
}

// `KHR_materials_
// iridescence` factors + optional textures. `iridescenceTexture`'s R
// channel scales `iridescenceFactor`; `iridescenceThicknessTexture`'s G
// channel lerps between `iridescenceThicknessMinimum`/`Maximum` — both per
// spec. Same "no KHR_texture_transform on these maps" v1 simplification as
// resolve_sheen. Returns (factor, ior, thickness_nm, 0).
fn resolve_iridescence(uv: vec4<f32>) -> vec3<f32> {
    var factor = u.iridescence_params.x;
    if u.texture_flags2.w > 0.5 {
        factor = factor * sample_extension_map(iridescence_map, uv, 7u).r;
    }
    var thickness = u.iridescence_params.w; // default = thicknessMaximum (spec: texture absent → max)
    if u.anisotropy_dispersion_params.w > 0.5 {
        let t = sample_extension_map(iridescence_thickness_map, uv, 8u).g;
        thickness = mix(u.iridescence_params.z, u.iridescence_params.w, t);
    }
    return vec3<f32>(factor, u.iridescence_params.y, thickness);
}

// `KHR_materials_
// anisotropy` factors + optional texture. `anisotropyTexture`'s RG encodes
// a per-texel rotation (cos/sin, remapped from [0,1] to [-1,1]) that
// ROTATES the factor's own `anisotropyRotation`, and its B channel scales
// `anisotropyStrength` — both per spec ("Direction and strength ... encoded
// in the .rg and .b channels ... rotated by anisotropyRotation"). Returns
// (strength, rotation_radians).
fn resolve_anisotropy(uv: vec4<f32>) -> vec2<f32> {
    var strength = u.anisotropy_dispersion_params.x;
    var rotation = u.anisotropy_dispersion_params.y;
    if u.transmission_volume_params.w > 0.5 {
        let t = sample_extension_map(anisotropy_map, uv, 9u).rgb;
        let tex_rotation = atan2(t.g * 2.0 - 1.0, t.r * 2.0 - 1.0);
        rotation = rotation + tex_rotation;
        strength = strength * t.b;
    }
    return vec2<f32>(strength, rotation);
}

// `KHR_materials_clearcoat`'s two factor-scaling textures.
// clearcoatTexture's R channel scales clearcoatFactor; clearcoatRoughness
// Texture's G channel scales clearcoatRoughnessFactor — both per spec.
// Returns (clearcoat_factor, clearcoat_roughness).
fn resolve_clearcoat(uv: vec4<f32>) -> vec2<f32> {
    var factor = u.alpha_params.z;
    var roughness = u.alpha_params.w;
    let flags = clearcoat_family_flags();
    if (flags & 1u) != 0u {
        factor = factor * sample_extension_map(clearcoat_map, uv, 10u).r;
    }
    if (flags & 2u) != 0u {
        roughness = roughness * sample_extension_map(clearcoat_roughness_map, uv, 11u).g;
    }
    return vec2<f32>(factor, clamp(roughness, 0.01, 1.0));
}

// `clearcoatNormalTexture` — a standard tangent-space normal map (same RGB
// convention/cotangent-frame reconstruction as the base `normalTexture`'s
// `resolve_normal`) that perturbs ONLY the clearcoat lobe's shading normal.
// Without a coat map, keep the oriented geometric normal.
fn resolve_clearcoat_normal(uv: vec4<f32>, n: vec3<f32>, world_pos: vec3<f32>, tangent: vec4<f32>) -> vec3<f32> {
    if (clearcoat_family_flags() & 4u) != 0u {
        let sampled = sample_extension_map(clearcoat_normal_map, uv, 12u).rgb;
        var tangent_normal = sampled * 2.0 - vec3<f32>(1.0);
        tangent_normal = vec3<f32>(tangent_normal.xy * u.normal_uv_t.w, tangent_normal.z);
        let frame_uv = select(material_uv(uv, 12u), material_uv(uv, 1u), u.texture_flags.x > 0.5);
        let tbn = tbn_for(n, world_pos, frame_uv, tangent);
        return normalize(tbn * tangent_normal);
    }
    return n;
}

// `KHR_materials_specular`'s two factor-scaling textures.
// specularTexture's ALPHA channel scales specularFactor; specularColor
// Texture (sRGB) tints specularColorFactor — both per spec. Returns
// (specular_factor, specular_tint.rgb).
fn resolve_specular(uv: vec4<f32>) -> vec4<f32> {
    var factor = u.pbr_metallic_roughness.w;
    var tint = u.pbr_specular_tint.rgb;
    let flags = specular_family_flags();
    if (flags & 1u) != 0u {
        factor = factor * sample_extension_map(specular_map, uv, 13u).a;
    }
    if (flags & 2u) != 0u {
        tint = tint * sample_extension_map(specular_color_map, uv, 14u).rgb;
    }
    return vec4<f32>(tint, factor);
}

// `KHR_materials_transmission`'s `transmissionTexture` — R channel
// scales `transmissionFactor` per spec.
fn resolve_transmission_factor(uv: vec4<f32>) -> f32 {
    var factor = u.transmission_volume_params.x;
    if (specular_family_flags() & 4u) != 0u {
        factor = factor * sample_extension_map(transmission_map, uv, 15u).r;
    }
    return factor;
}

// `KHR_materials_volume`'s `thicknessTexture` — G channel scales
// `thicknessFactor` per spec.
fn resolve_volume_thickness(uv: vec4<f32>) -> f32 {
    var thickness = u.transmission_volume_params.y;
    if (specular_family_flags() & 8u) != 0u {
        thickness = thickness * sample_extension_map(volume_thickness_map, uv, 16u).g;
    }
    return thickness;
}

// KHR_materials_diffuse_transmission: A scales the factor, RGB scales
// the independently authored colour. Colour textures are decoded to linear.
fn resolve_diffuse_transmission(uv: vec4<f32>) -> vec4<f32> {
    var factor = u.diffuse_transmission_params.x;
    var color = u.diffuse_transmission_params.yzw;
    let flags = specular_family_flags();
    if (flags & 16u) != 0u {
        factor *= sample_extension_map(diffuse_transmission_map, uv, 17u).a;
    }
    if (flags & 32u) != 0u {
        color *= sample_extension_map(diffuse_transmission_color_map, uv, 18u).rgb;
    }
    return vec4<f32>(color, clamp(factor, 0.0, 1.0));
}

fn transmission_diffuse(
    N: vec3<f32>,
    V: vec3<f32>,
    world_pos: vec3<f32>,
    roughness: f32,
    ior: f32,
    albedo_rgb: vec3<f32>,
    F0: vec3<f32>,
    F90: vec3<f32>,
    env_brdf: vec2<f32>,
    thickness: f32,
    instance_scale: f32,
    dispersion: f32,
) -> vec3<f32> {
    // Authored thickness is in mesh-local units. Compose the instance's
    // absolute uniform scale with the model's column lengths for both
    // refraction and absorption; geometry-derived paths are already world-space.
    var model_scale = vec3<f32>(
        length(u.model[0].xyz),
        length(u.model[1].xyz),
        length(u.model[2].xyz),
    ) * instance_scale * vec3<f32>(
        length(u.instance_inner[0].xyz),
        length(u.instance_inner[1].xyz),
        length(u.instance_inner[2].xyz),
    );
    if u.volume_optics.x > 0.5 { model_scale = vec3<f32>(1.0); }

    let mip_level = transmission_mip_level(roughness, ior);

    var transmitted: vec3<f32>;
    if dispersion > 0.0 {
        // `KHR_materials_
        // dispersion` — per-channel IOR spread around the base IOR
        // (Khronos glTF-Sample-Viewer reference constant: half-spread =
        // (ior - 1) * 0.025 * dispersion), each channel refracted and
        // sampled independently — the classic "chromatic aberration
        // through glass" look. `dispersion == 0.0` (every material without
        // this extension, the overwhelming common case) skips this branch
        // entirely, so non-dispersive glass pays zero extra cost.
        let half_spread = (ior - 1.0) * 0.025 * dispersion;
        let ior_r = ior - half_spread;
        let ior_g = ior;
        let ior_b = ior + half_spread;
        let sample_r = sample_transmission(N, V, world_pos, thickness, model_scale, ior_r, mip_level);
        let sample_g = sample_transmission(N, V, world_pos, thickness, model_scale, ior_g, mip_level);
        let sample_b = sample_transmission(N, V, world_pos, thickness, model_scale, ior_b, mip_level);
        transmitted = vec3<f32>(sample_r.r, sample_g.g, sample_b.b);
    } else {
        transmitted = sample_transmission(N, V, world_pos, thickness, model_scale, ior, mip_level);
    }

    // Beer-Lambert (KHR_materials_volume): attenuation_coefficient =
    // -ln(attenuationColor)/attenuationDistance, tint =
    // exp(-coefficient*distance_travelled). Defaults (attenuationColor =
    // [1,1,1], attenuationDistance = the gltf_load.rs 1e6 "no attenuation"
    // sentinel) collapse ln(1) = 0 -> coefficient = 0 -> tint = exactly 1
    // (no tinting) — verified numerically at authoring time (E2b execution
    // report), not just assumed algebraically. `travelled` uses the BASE
    // (non-dispersed) ray length even when dispersion > 0 — a documented v1
    // simplification (per-channel attenuation distance is a second-order
    // effect the spec doesn't require and the Khronos sample viewer itself
    // folds into the per-channel getIBLVolumeRefraction call, which this
    // codebase's single Beer-Lambert tint approximates).
    var base_refraction_dir = refract(-V, N, 1.0 / max(ior, 1.0));
    if dot(base_refraction_dir, base_refraction_dir) < 1e-8 {
        base_refraction_dir = -N;
    }
    let travelled = length(normalize(base_refraction_dir) * thickness * model_scale);
    let attenuation_coefficient = volume_attenuation_coefficient();
    let tint = exp(-attenuation_coefficient * travelled);
    var attenuated = transmitted * tint;
    if u.volume_optics.x > 0.5 {
        let clip = u.view_proj * vec4<f32>(world_pos, 1.0);
        let uv = clip.xy / clip.w * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
        let pixel = clamp(vec2<i32>(uv * vec2<f32>(textureDimensions(volume_density))), vec2<i32>(0), vec2<i32>(textureDimensions(volume_density)) - vec2<i32>(1));
        let embedded = max(textureLoad(volume_density, pixel, 0).r, 0.0);
        let optical_density = u.volume_optics.y * travelled + embedded;
        let visibility = exp(-optical_density);
        // Isotropic single-scattering approximation lit by the existing diffuse
        // environment. Embedded density is screen-space and assumes particles
        // belong to this volume; it is not a world-space multiple-scattering solve.
        let illumination = textureSampleLevel(irradiance_map, envmap_sampler, vec2<f32>(0.5, 1.0), 0.0).rgb;
        let inscatter = u.volume_scattering_color.rgb * illumination;
        attenuated = attenuated * visibility + inscatter * (1.0 - visibility);
    }

    // Same specular-reflectance fraction the split-sum IBL above already
    // computes (F0*brdf.x + brdf.y, f90 implicitly 1.0) — the glass's own
    // specular highlight/reflection stays additive on top (composed at the
    // call site, untouched); only the DIFFUSE response is replaced by the
    // refracted sample, per the glTF sample viewer's f_diffuse
    // substitution — surface base_color still tints the see-through
    // (real glass isn't perfectly colorless).
    let specular_color = transmission_specular_fraction(F0, F90, env_brdf);
    return (vec3<f32>(1.0) - specular_color) * attenuated * albedo_rgb;
}

// ===== Material kind fragment entry points =====

// Unlit — flat colour passthrough plus emission. No lighting math, no
// light loop (matches render_3d_mesh.wgsl exactly).
@fragment
fn fs_unlit(in: VsOut) -> @location(0) vec4<f32> {
    let albedo = resolve_albedo(in.uv, in.vertex_color);
    if appearance_discard(in.appearance_weight) {
        discard;
    }
    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {
        discard;
    }
    // exp2(exposure_ev). Multiplies the
    // final STRAIGHT rgb (post-fog, post-emission, pre-output); alpha is
    // untouched (alpha contract). exposure_ev = 0 (PINHOLE default) → ×1,
    // byte-identical to pre-lens builds (I2/I5).
    let rgb = apply_fog(albedo.rgb + resolve_emissive(in.uv), in.world_pos) * exp2(u.scene_params.z);
    return apply_appearance(rgb, albedo.a, in.appearance_weight);
}

// PBR — Cook-Torrance microfacet specular + Lambert diffuse, summed over
// every wired light (H/D/G/F/kd/diffuse are all light-dependent via H,
// so they're recomputed per light); IBL reflection + ambient + emission
// added exactly once outside the loop. IBL's Fresnel term uses N·V
// (view-only Schlick) rather than the light-dependent N·H term any
// single light would give — the standard split-sum substitute for IBL,
// and the only well-defined choice when light_count can be 0.
@fragment
fn fs_pbr(in: VsOut) -> @location(0) vec4<f32> {
    if u.volume_optics.x > 0.5 {
        let nearest = textureLoad(volume_nearest, vec2<i32>(in.clip_pos.xy), 0);
        if in.clip_pos.z < nearest - max(nearest * 0.00001, 0.0000001) { discard; }
    }
    var albedo = resolve_albedo(in.uv, in.vertex_color);
    if appearance_discard(in.appearance_weight) {
        discard;
    }
    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {
        discard;
    }
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    var geometric_N = normalize(in.world_normal);
    if dot(geometric_N, V) < 0.0 {
        geometric_N = -geometric_N;
    }
    var N = resolve_normal(in.uv, in.world_normal, in.world_pos, in.world_tangent);
    if dot(N, V) < 0.0 {
        N = -N;
    }
    let mr = resolve_mr(in.uv);
    // Water on the surface darkens it and smooths it, keyed on world
    // position because materials are shared. After the cutout test, which
    // stays on the material's own alpha.
    let wet = look_wet_surface(in.world_pos, albedo.rgb, mr.x);
    albedo = vec4<f32>(wet.albedo, albedo.a);
    let roughness = wet.roughness;
    let metallic = mr.y;
    // (sheenColor.rgb, sheenRoughness). The default zero colour zeroes every
    // sheen term.
    let sheen = resolve_sheen(in.uv);
    let sheen_color = sheen.rgb;
    let sheen_roughness = sheen.w;
    let translucency = resolve_diffuse_transmission(in.uv);

    // KHR_materials_specular + ior, from the Khronos KHR_materials_specular
    // README:
    //   dielectric_f0 = min(((ior-1)/(ior+1))^2 * specularColorFactor, 1.0)
    //                   * specularFactor
    // Defaults (ior=1.5, specular_factor=1.0, specular_tint=(1,1,1)) give
    // F0 = 0.04. The same specular factor controls the dielectric grazing
    // reflectance; metals keep the physical F90 = 1.
    let ior = u.pbr_metallic_roughness.z;
    let specular_resolved = resolve_specular(in.uv);
    let specular_tint = specular_resolved.rgb;
    let specular_factor = specular_resolved.a;
    let dielectric_reflectance = pow((ior - 1.0) / (ior + 1.0), 2.0);
    let dielectric_f0 = min(dielectric_reflectance * specular_tint, vec3<f32>(1.0)) * specular_factor;
    let base_f0 = mix(dielectric_f0, albedo.rgb, metallic);
    let base_f90 = mix(vec3<f32>(specular_factor), vec3<f32>(1.0), vec3<f32>(metallic));
    let iridescence = resolve_iridescence(in.uv);

    // `KHR_materials_anisotropy` tangent basis: authored when the mesh
    // carries TANGENT (BUG-wfxe), the same cotangent_frame fallback
    // `resolve_normal` uses otherwise. Anisotropy's T and the normal map's T
    // must be the SAME frame or the stretch axis drifts off the mapped
    // surface detail.
    var anisotropy_strength = 0.0;
    var aniso_t = vec3<f32>(1.0, 0.0, 0.0);
    var aniso_b = vec3<f32>(0.0, 1.0, 0.0);
    if u.anisotropy_dispersion_params.x > 0.0 {
        let tangent_uv = select(in.uv.xy, material_uv(in.uv, 1u), u.texture_flags.x > 0.5);
        let tbn = tbn_for(N, in.world_pos, tangent_uv, in.world_tangent);
        let anisotropy = resolve_anisotropy(in.uv);
        anisotropy_strength = clamp(anisotropy.x, 0.0, 1.0);
        let anisotropy_rotation = anisotropy.y;
        let cos_ar = cos(anisotropy_rotation);
        let sin_ar = sin(anisotropy_rotation);
        let tangent_axis = normalize(tbn[0]);
        let bitangent_axis = normalize(cross(N, tangent_axis))
            * select(1.0, -1.0, dot(cross(N, tangent_axis), tbn[1]) < 0.0);
        aniso_t = tangent_axis * cos_ar + bitangent_axis * sin_ar;
        aniso_b = bitangent_axis * cos_ar - tangent_axis * sin_ar;
    }

    // KHR_materials_clearcoat, a second, always-dielectric GGX lobe on top of
    // the base BRDF with a fixed F0 of 0.04. Its maps scale the factors
    // (`resolve_clearcoat`) and perturb its own normal `Nc`
    // (`resolve_clearcoat_normal`); unmapped, the oriented geometric normal.
    var clearcoat = 0.0;
    var clearcoat_roughness = 0.01;
    var Nc = geometric_N;
    if u.alpha_params.z > 0.0 {
        let clearcoat_resolved = resolve_clearcoat(in.uv);
        clearcoat = clearcoat_resolved.x;
        clearcoat_roughness = clearcoat_resolved.y;
        Nc = resolve_clearcoat_normal(in.uv, geometric_N, in.world_pos, in.world_tangent);
    }

    let surface = PbrSurface(
        in.world_pos, in.clip_pos.xy, V, N, Nc, albedo.rgb, metallic, roughness,
        base_f0, base_f90, iridescence, anisotropy_strength, aniso_t, aniso_b,
        clearcoat, clearcoat_roughness, sheen_color, sheen_roughness, translucency,
    );
    let direct = pbr_direct_lighting(surface);
    let ibl = pbr_ibl(surface);

    // Use the measured view reflectance for the diffuse energy split. A
    // scalar max-channel reduction keeps coloured Fresnel from creating
    // diffuse energy in the other channels.
    let f_view_max = max(ibl.f_view.r, max(ibl.f_view.g, ibl.f_view.b));
    let kd_ibl = (1.0 - f_view_max) * (1.0 - metallic);
    // Occlusion darkens the diffuse IBL term ONLY, never direct lighting or
    // specular IBL. Unwired = 1.0 (no darkening).
    let occlusion = resolve_occlusion(in.uv);
    let diffuse_ibl = kd_ibl * albedo.rgb * ibl.irradiance * occlusion;

    let diffuse_transmission = translucency.a;
    var translucent_ibl = vec3<f32>(0.0);
    if diffuse_transmission > 0.0 {
        let back_uv = env_lookup_uv(-N);
        let back_irradiance = textureSampleLevel(irradiance_map, envmap_sampler, back_uv, 0.0).rgb;
        translucent_ibl = kd_ibl * translucency.rgb * back_irradiance
            * diffuse_transmission * occlusion;
    }
    var diffuse_component = (direct.diffuse + diffuse_ibl) * (1.0 - diffuse_transmission)
        + direct.translucent + translucent_ibl;
    if subsurface_binding.x > 0.0 {
        let scattered = textureLoad(subsurface_radiance, vec2<i32>(in.clip_pos.xy), 0);
        if scattered.a < 0.0 {
            // An open boundary or exhausted random walk must be visible;
            // never silently show a different transport mode as successful.
            diffuse_component = vec3<f32>(1.0, 0.0, 1.0);
        } else if scattered.a == subsurface_binding.y {
            diffuse_component = mix(diffuse_component, kd_ibl * scattered.rgb, subsurface_binding.x);
        }
    }
    let transmission_factor = resolve_transmission_factor(in.uv);
    if transmission_factor > 0.0 {
        var volume_thickness = resolve_volume_thickness(in.uv);
        if u.volume_optics.x > 0.5 {
            volume_thickness = max(textureLoad(volume_path, vec2<i32>(in.clip_pos.xy), 0).r, 0.0);
        }
        let transmitted_diffuse = transmission_diffuse(
            N, V, in.world_pos, roughness, ior, albedo.rgb, ibl.ibl_f0, base_f90,
            ibl.env_brdf, volume_thickness, in.instance_scale, u.anisotropy_dispersion_params.z
        ) * (1.0 - metallic);
        diffuse_component = mix(diffuse_component, transmitted_diffuse, transmission_factor);
    }
    let ambient = rt_or_flat_ambient(albedo.rgb, in.clip_pos.xy);
    let emissive = resolve_emissive(in.uv);
    let base_rgb = (direct.total - direct.diffuse + ibl.specular + diffuse_component + ambient)
        * ibl.sheen_base_scale + direct.sheen + ibl.sheen + emissive;
    let cc_n_dot_v = max(dot(Nc, V), 0.001);
    let clearcoat_fresnel = CLEARCOAT_F0 + (1.0 - CLEARCOAT_F0)
        * pow(clamp(1.0 - cc_n_dot_v, 0.0, 1.0), 5.0);
    let fc = clearcoat * clearcoat_fresnel;
    let coat_rgb = direct.coat + ibl.coat;
    let lit = base_rgb * (1.0 - fc) + coat_rgb * clearcoat;

    // exp2(exposure_ev), see fs_unlit.
    let rgb = apply_fog(lit, in.world_pos) * exp2(u.scene_params.z);
    return apply_appearance(rgb, albedo.a, in.appearance_weight);
}

// Cel — Lambert N·L quantized into cel_bands discrete steps, summed over
// every wired light; ambient + emission added exactly once.
@fragment
fn fs_cel(in: VsOut) -> @location(0) vec4<f32> {
    let albedo = resolve_albedo(in.uv, in.vertex_color);
    if appearance_discard(in.appearance_weight) {
        discard;
    }
    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {
        discard;
    }
    var N = resolve_normal(in.uv, in.world_normal, in.world_pos, in.world_tangent);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    if dot(N, V) < 0.0 {
        N = -N;
    }
    let bands = max(u.cel_params.x, 2.0);
    let band_low = u.cel_params.y;
    let band_high = u.cel_params.z;

    var lit = vec3<f32>(0.0);
    let light_count = u32(u.scene_params.x);
    for (var i = 0u; i < light_count; i = i + 1u) {
        let l_dir = light_direction_attenuation(i, in.world_pos);
        let l_col = lights[i * LIGHT_STRIDE + 1u];
        let L = l_dir.xyz;
        let n_dot_l = max(dot(N, L), 0.0);
        let snapped = floor(n_dot_l * bands) / (bands - 1.0);
        let level = mix(band_low, band_high, clamp(snapped, 0.0, 1.0));
        let vis = shadow_factor(in.world_pos, l_col.w, in.clip_pos.xy);
        lit = lit + albedo.rgb * level * l_col.rgb * l_dir.w * vis;
    }
    let ambient = rt_or_flat_ambient(albedo.rgb, in.clip_pos.xy);
    // exp2(exposure_ev), see fs_unlit.
    let rgb = apply_fog(lit + ambient + resolve_emissive(in.uv), in.world_pos) * exp2(u.scene_params.z);
    return apply_appearance(rgb, albedo.a, in.appearance_weight);
}
