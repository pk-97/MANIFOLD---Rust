// A static collider's exact mesh for the matter atoms (a shape with tree == 1:
// no lattice, a BVH in the body's local frame at scale 1). Declared through
// `wgsl_includes` after liquid_pose.wgsl, liquid_collider.wgsl and
// collider_bvh.wgsl; the including body defines cb_node and cb_triangle over its
// gathered collider_nodes and collider_triangles. The band is four of the
// node's grid cells and the shell one, so an open sheet is a cell thick.

fn mt_local(x: vec3<f32>, position: vec3<f32>, rotation: vec4<f32>) -> vec3<f32> {
    return liquid_rotate(vec4<f32>(-rotation.xyz, rotation.w), x - position);
}

fn mt_phi(node: u32, triangle: u32, local: vec3<f32>, cell: f32) -> f32 {
    return cb_phi(node, triangle, local, 4.0 * cell, cell);
}

// The world-space gradient of the local distance (central differences half a
// cell either side), turned by the body's rotation.
fn mt_gradient(node: u32, triangle: u32, local: vec3<f32>, cell: f32, rotation: vec4<f32>) -> vec3<f32> {
    let h = 0.5 * cell;
    let g = vec3<f32>(
        mt_phi(node, triangle, local + vec3<f32>(h, 0.0, 0.0), cell) - mt_phi(node, triangle, local - vec3<f32>(h, 0.0, 0.0), cell),
        mt_phi(node, triangle, local + vec3<f32>(0.0, h, 0.0), cell) - mt_phi(node, triangle, local - vec3<f32>(0.0, h, 0.0), cell),
        mt_phi(node, triangle, local + vec3<f32>(0.0, 0.0, h), cell) - mt_phi(node, triangle, local - vec3<f32>(0.0, 0.0, h), cell),
    ) / (2.0 * h);
    return liquid_rotate(rotation, g);
}

// liquid_collider_project for a tree shape: the same rule on the mesh's
// distance and gradient.
fn mt_collider_project(
    v: vec3<f32>,
    x: vec3<f32>,
    position: vec3<f32>,
    rotation: vec4<f32>,
    linear: vec3<f32>,
    angular: vec3<f32>,
    friction: f32,
    node: u32,
    triangle: u32,
    cell: f32,
) -> vec3<f32> {
    let local = mt_local(x, position, rotation);
    if mt_phi(node, triangle, local, cell) >= 0.0 {
        return v;
    }
    let grad = mt_gradient(node, triangle, local, cell, rotation);
    let length_sq = dot(grad, grad);
    if !(length_sq > 0.0) {
        return v;
    }
    let normal = grad * inverseSqrt(length_sq);
    let v_body = liquid_body_velocity(linear, angular, position, x);
    let v_rel = v - v_body;
    let v_n = dot(v_rel, normal);
    if v_n >= 0.0 {
        return v;
    }
    let t = v_rel - v_n * normal;
    let t_len = length(t);
    let keep = max(0.0, t_len + v_n * friction);
    return v_body + select(vec3<f32>(0.0), t * (keep / t_len), t_len > 0.0);
}
