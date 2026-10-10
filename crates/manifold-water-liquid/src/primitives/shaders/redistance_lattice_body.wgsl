// Exact Euclidean distance to the piecewise-linear marching-cubes zero
// surface, capped at band. This is a geometric distance rebuild, not a blur.
// The search box contains every cell intersecting the band-radius ball;
// there is no fixed stencil/slider cap. Distances are in scene metres.
fn rd_segment(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let v = b - a;
    let vv = dot(v, v);
    if vv == 0.0 { return dot(p-a, p-a); }
    let d = p - (a + clamp(dot(p-a, v)/vv, 0.0, 1.0)*v);
    return dot(d,d);
}
fn rd_triangle(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> f32 {
    let ab = b-a;
    let ac = c-a;
    let n = cross(ab, ac);
    let nn = dot(n,n);
    var best = min(rd_segment(p,a,b), min(rd_segment(p,b,c), rd_segment(p,c,a)));
    if nn > 0.0 {
        let q = p-a;
        let u = dot(cross(q,ac), n)/nn;
        let v = dot(cross(ab,q), n)/nn;
        if u >= 0.0 && v >= 0.0 && u+v <= 1.0 {
            best = min(best, dot(q,n)*dot(q,n)/nn);
        }
    }
    return best;
}
fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32,
    size_x: f32, size_y: f32, size_z: f32, band: f32, enabled: f32) -> f32 {
    let original = buf_levelset[idx];
    if enabled == 0.0 { return original; }
    let nodes = vec3<u32>(vec3<f32>(nodes_x,nodes_y,nodes_z));
    if any(nodes < vec3<u32>(2u)) || idx >= nodes.x*nodes.y*nodes.z { return original; }
    let at = vec3<u32>(idx % nodes.x, (idx/nodes.x)%nodes.y, idx/(nodes.x*nodes.y));
    let spacing = vec3<f32>(size_x,size_y,size_z)/vec3<f32>(nodes-vec3<u32>(1u));
    let p = vec3<f32>(at)*spacing;
    let reach = vec3<i32>(ceil(vec3<f32>(band)/spacing));
    let low = max(vec3<i32>(at)-reach-vec3<i32>(1),vec3<i32>(0));
    let high = min(vec3<i32>(at)+reach,vec3<i32>(nodes)-vec3<i32>(2));
    var best = band*band;
    for (var z=low.z; z<=high.z; z=z+1) {
        for (var y=low.y; y<=high.y; y=y+1) {
            for (var x=low.x; x<=high.x; x=x+1) {
                let cell = vec3<u32>(vec3<i32>(x,y,z));
                let box_low = vec3<f32>(cell)*spacing;
                let box_high = vec3<f32>(cell+vec3<u32>(1u))*spacing;
                let delta = max(max(box_low-p,p-box_high),vec3<f32>(0.0));
                if dot(delta,delta) > best { continue; }
                let case_index = mc_case(cell,nodes);
                let triangles = MC_TRIANGLE_COUNT[case_index];
                if triangles == 0u { continue; }
                var vertices: array<vec3<f32>,12>;
                for (var e=0u; e<12u; e=e+1u) {
                    let a = cell+MC_CORNERS[MC_EDGE_A[e]];
                    let b = cell+MC_CORNERS[MC_EDGE_B[e]];
                    let va = buf_levelset[mc_node(a,nodes)];
                    let vb = buf_levelset[mc_node(b,nodes)];
                    if (va < 0.0) != (vb < 0.0) {
                        vertices[e] = (vec3<f32>(a)+va/(va-vb)*(vec3<f32>(b)-vec3<f32>(a)))*spacing;
                    }
                }
                for (var t=0u; t<triangles; t=t+1u) {
                    best = min(best,rd_triangle(p,vertices[mc_edge(case_index,3u*t)],
                        vertices[mc_edge(case_index,3u*t+1u)],vertices[mc_edge(case_index,3u*t+2u)]));
                }
            }
        }
    }
    return select(sqrt(best),-sqrt(best),original < 0.0);
}
