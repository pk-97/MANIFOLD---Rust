// Foam is a visible-surface coverage layer, not a volume integral.
struct WaterParticle {
    position_mass: vec4<f32>, velocity_density: vec4<f32>,
    affine_x: vec4<f32>, affine_y: vec4<f32>, affine_z: vec4<f32>,
    previous_position: vec4<f32>,
}
@group(0) @binding(0) var<uniform> splat: SplatView;
@group(0) @binding(1) var<storage, read> particles: array<WaterParticle>;
@group(0) @binding(2) var<storage, read> foam: array<f32>;
@group(0) @binding(3) var depth: texture_2d<f32>;
@group(0) @binding(4) var<storage, read_write> out_bits: array<atomic<u32>>;
fn linear_depth(raw: f32) -> f32 {
    return splat.near * splat.far / (splat.far - raw * (splat.far - splat.near));
}
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= splat.count) { return; }
    let p = particles[i];
    let fraction = foam[i];
    if (p.position_mass.w == 0.0 || !splat_finite1(fraction) || fraction <= 0.001) { return; }
    let c = splat_view_center(splat, p.position_mass.xyz);
    if (!splat_accept(splat, c)) { return; }
    var lo: vec2<i32>;
    var hi: vec2<i32>;
    splat_bbox(splat, c, &lo, &hi);
    for (var y = lo.y; y <= hi.y; y++) {
        for (var x = lo.x; x <= hi.x; x++) {
            let px = vec2<i32>(x, y);
            let dir = splat_ray_dir(splat, px);
            let t = splat_ray_hit(c, splat.radius, dir);
            if (t < 0.0) { continue; }
            let water_raw = textureLoad(depth, px, 0).r;
            if (!splat_finite1(water_raw) || water_raw >= 1.0) { continue; }
            // Both depths must be measured along the view axis, including off-axis rays.
            if (abs(t * dir.z - linear_depth(water_raw)) > 2.0 * splat.radius) { continue; }
            let radial = clamp((dot(dir, c) - t) / splat.radius, 0.0, 1.0);
            let coverage = clamp(fraction, 0.0, 1.0) * radial;
            let index = u32(y) * splat.width + u32(x);
            // Nonnegative float bits sort numerically; max cannot overflow or lose retries.
            atomicMax(&out_bits[index], bitcast<u32>(coverage));
        }
    }
}

@group(0) @binding(5) var<storage, read> shapes: array<SurfaceShape>;
@compute @workgroup_size(256)
fn cs_anisotropic(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x; if (i >= splat.count) { return; }
    let fraction = foam[i]; if (fraction <= 0.001 || !splat_finite1(fraction)) { return; }
    let s = shape_view(splat, shapes[i]); if (!shape_valid(s)) { return; }
    var lo: vec2<i32>; var hi: vec2<i32>; var sphere = splat; sphere.radius = s.bound;
    if (particles[i].position_mass.w == 0.0 || !splat_accept(sphere, s.center)) { return; }
    shape_bbox(splat, s, &lo, &hi);
    for (var y=lo.y; y<=hi.y; y++) { for (var x=lo.x; x<=hi.x; x++) {
        let dir=splat_ray_dir(splat,vec2<i32>(x,y)); let hit=shape_ray_hit(s,dir); if(hit.x<=0.0){continue;}
        let raw=textureLoad(depth,vec2<i32>(x,y),0).r; if(!splat_finite1(raw)||raw>=1.0){continue;}
        if(abs(hit.x*dir.z-linear_depth(raw))>2.0*min(length(s.axis_x),min(length(s.axis_y),length(s.axis_z)))){continue;}
        let q=vec3<f32>(dot(dir,s.axis_x)/dot(s.axis_x,s.axis_x),dot(dir,s.axis_y)/dot(s.axis_y,s.axis_y),dot(dir,s.axis_z)/dot(s.axis_z,s.axis_z));
        let radial=clamp(0.5*(hit.y-hit.x)*length(q),0.0,1.0);
        atomicMax(&out_bits[u32(y)*splat.width+u32(x)],bitcast<u32>(clamp(fraction,0.0,1.0)*radial));
    }}
}
