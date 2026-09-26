// Deterministic clear -> count -> serial scan -> emit kernel for cut maps.
// xyz are barycentrics and w is the source triangle index.
struct Params {
    mode: u32,
    bands: u32,
    triangle_count: u32,
    candidate_count: u32,
    output_capacity: u32,
    _pad0: u32,
    cell_size: f32,
    scale: f32,
    source_offset: vec3<f32>,
    _pad1: f32,
    direction: vec3<f32>,
    _pad2: f32,
};
struct MeshVertex { position: vec3<f32>, _pad0: f32, normal: vec3<f32>, _pad1: f32, uv: vec2<f32>, _pad2: vec2<f32>, tangent: vec4<f32>, color: vec4<f32> };
struct Vec4Vertex { position: vec4<f32> };
struct CutVertex { position: vec3<f32>, barycentric: vec3<f32> };
struct Polygon { vertices: array<CutVertex, 9>, count: u32, valid: bool };
struct CellResult { records: u32, valid: bool, reason: u32 };
struct CellBounds { minimum: vec3<i32>, maximum: vec3<i32>, valid: bool };
struct AxisBounds { minimum: i32, maximum: i32, valid: bool, empty: bool };

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> reference: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> counts: array<u32>;
@group(0) @binding(3) var<storage, read_write> prefix: array<u32>;
@group(0) @binding(4) var<storage, read_write> status: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> map: array<Vec4Vertex>;

fn finite(v: f32) -> bool { return v == v && abs(v) <= 3.402823466e+38; }
fn finite3(v: vec3<f32>) -> bool { return finite(v.x) && finite(v.y) && finite(v.z); }
fn safe_scale() -> f32 { return max(abs(params.scale), 1e-6); }
fn safe_cell() -> f32 { return max(abs(params.cell_size), 1e-6); }
fn direction() -> vec3<f32> {
    let d = params.direction;
    let len = length(d);
    if finite(len) && len > 1e-8 { return d / len; }
    return vec3<f32>(0.0, 1.0, 0.0);
}
fn source_position(position: vec3<f32>) -> vec3<f32> { return (position + params.source_offset) / safe_scale(); }
fn same_vertex(a: CutVertex, b: CutVertex) -> bool { return all(a.barycentric == b.barycentric); }
fn empty_polygon() -> Polygon {
    var p: Polygon; p.count = 0u; p.valid = true; return p;
}
fn append_unique(polygon: Polygon, vertex: CutVertex) -> Polygon {
    var p = polygon;
    if !p.valid { return p; }
    if p.count > 0u && same_vertex(p.vertices[p.count - 1u], vertex) { return p; }
    if p.count >= 9u { p.valid = false; return p; }
    p.vertices[p.count] = vertex; p.count = p.count + 1u; return p;
}

fn plane_distance(vertex: CutVertex, normal: vec3<f32>, offset: f32, kind: u32) -> f32 {
    if kind == 0u { return dot(vertex.position, normal) + offset; }
    let projected = 0.5 + 0.5 * dot(source_position(vertex.position), direction());
    let distance = projected - offset;
    if kind == 1u { return distance; }
    return -distance;
}
fn clip_plane(polygon: Polygon, normal: vec3<f32>, offset: f32, kind: u32) -> Polygon {
    var result = empty_polygon(); result.valid = polygon.valid;
    if !result.valid || polygon.count == 0u { return result; }
    for (var i = 0u; i < polygon.count; i = i + 1u) {
        let previous = polygon.vertices[(i + polygon.count - 1u) % polygon.count];
        let current = polygon.vertices[i];
        let previous_distance = plane_distance(previous, normal, offset, kind);
        let current_distance = plane_distance(current, normal, offset, kind);
        if !finite(previous_distance) || !finite(current_distance) { result.valid = false; return result; }
        let previous_inside = previous_distance >= 0.0;
        let current_inside = current_distance >= 0.0;
        if previous_inside != current_inside {
            let denominator = previous_distance - current_distance;
            if !finite(denominator) || denominator == 0.0 { result.valid = false; return result; }
            let t = previous_distance / denominator;
            if !finite(t) { result.valid = false; return result; }
            var intersection = previous;
            if t >= 1.0 { intersection = current; }
            else if t > 0.0 {
                intersection.position = mix(previous.position, current.position, t);
                intersection.barycentric = mix(previous.barycentric, current.barycentric, t);
            }
            if !finite3(intersection.position) || !finite3(intersection.barycentric) { result.valid = false; return result; }
            result = append_unique(result, intersection);
        }
        if current_inside { result = append_unique(result, current); }
        if !result.valid { return result; }
    }
    if result.count > 1u && same_vertex(result.vertices[0], result.vertices[result.count - 1u]) { result.count = result.count - 1u; }
    return result;
}

fn triangle_polygon(triangle: u32) -> Polygon {
    var result = empty_polygon();
    let base = triangle * 3u;
    let p0 = reference[base].position; let p1 = reference[base + 1u].position; let p2 = reference[base + 2u].position;
    result.vertices[0] = CutVertex(p0, vec3<f32>(1.0, 0.0, 0.0));
    result.vertices[1] = CutVertex(p1, vec3<f32>(0.0, 1.0, 0.0));
    result.vertices[2] = CutVertex(p2, vec3<f32>(0.0, 0.0, 1.0));
    result.count = 3u;
    result.valid = finite3(p0) && finite3(p1) && finite3(p2) && finite3(params.source_offset);
    if result.valid && length(cross(p1 - p0, p2 - p0)) == 0.0 { result.count = 0u; }
    return result;
}

fn band_polygon(triangle: u32, band: u32) -> Polygon {
    var result = triangle_polygon(triangle);
    if !result.valid || result.count == 0u { return result; }
    let band_count = max(params.bands, 1u);
    var projected = vec3<f32>(0.0);
    for (var i = 0u; i < 3u; i = i + 1u) {
        projected[i] = 0.5 + 0.5 * dot(source_position(result.vertices[i].position), direction());
        if !finite(projected[i]) { result.valid = false; return result; }
    }
    let lo = min(projected.x, min(projected.y, projected.z));
    let hi = max(projected.x, max(projected.y, projected.z));
    if hi == lo {
        let centroid = clamp((projected.x + projected.y + projected.z) / 3.0, 0.0, 0.99999994);
        let centroid_band = min(u32(floor(centroid * f32(band_count))), band_count - 1u);
        if band != centroid_band { result.count = 0u; return result; }
    }
    if band > 0u { result = clip_plane(result, vec3<f32>(0.0), f32(band) / f32(band_count), 1u); }
    if band + 1u < band_count { result = clip_plane(result, vec3<f32>(0.0), f32(band + 1u) / f32(band_count), 2u); }
    return result;
}
fn normalized_triangle_polygon(triangle: u32) -> Polygon {
    var result = triangle_polygon(triangle);
    for (var i = 0u; i < result.count; i = i + 1u) {
        result.vertices[i].position = source_position(result.vertices[i].position);
        if !finite3(result.vertices[i].position) { result.valid = false; return result; }
    }
    return result;
}
fn clip_cell_axis(polygon: Polygon, axis: u32, cell: i32) -> Polygon {
    var normal = vec3<f32>(0.0); normal[axis] = 1.0;
    let center = f32(cell) * safe_cell();
    var result = clip_plane(polygon, normal, -(center - safe_cell() * 0.5), 0u);
    result = clip_plane(result, -normal, center + safe_cell() * 0.5, 0u);
    return result;
}
fn polygon_records(polygon: Polygon) -> CellResult {
    var result = CellResult(0u, polygon.valid, 0u);
    if !polygon.valid || polygon.count < 3u { return result; }
    for (var i = 1u; i + 1u < polygon.count; i = i + 1u) {
        let a = polygon.vertices[0].position; let b = polygon.vertices[i].position; let c = polygon.vertices[i + 1u].position;
        let twice_area = length(cross(b - a, c - a));
        if !finite(twice_area) { result.valid = false; result.reason = 2u; return result; }
        if twice_area > 0.0 {
            if result.records > 0xffffffffu - 3u { result.valid = false; result.reason = 1u; return result; }
            result.records = result.records + 3u;
        }
    }
    if result.records > params.output_capacity { result.valid = false; result.reason = 1u; }
    return result;
}

fn cell_index(value: f32) -> i32 {
    return i32(floor(value / safe_cell() + 0.5));
}
fn cell_bounds(triangle: u32) -> CellBounds {
    let base = triangle * 3u;
    let p0 = source_position(reference[base].position); let p1 = source_position(reference[base + 1u].position); let p2 = source_position(reference[base + 2u].position);
    var result = CellBounds(vec3<i32>(0), vec3<i32>(0), finite3(p0) && finite3(p1) && finite3(p2));
    if !result.valid { return result; }
    let lo = min(p0, min(p1, p2)); let hi = max(p0, max(p1, p2));
    let ilo = vec3<f32>(floor(lo / safe_cell() + 0.5)); let ihi = vec3<f32>(floor(hi / safe_cell() + 0.5));
    if any(!finite3(ilo)) || any(ilo < vec3<f32>(-2147483000.0)) || any(ilo > vec3<f32>(2147483000.0)) || any(!finite3(ihi)) || any(ihi < vec3<f32>(-2147483000.0)) || any(ihi > vec3<f32>(2147483000.0)) { result.valid = false; return result; }
    result.minimum = vec3<i32>(ilo); result.maximum = vec3<i32>(ihi); return result;
}
fn axis_bounds(polygon: Polygon, axis: u32) -> AxisBounds {
    var result = AxisBounds(0, -1, polygon.valid, false);
    if !polygon.valid { return result; }
    if polygon.count == 0u { result.empty = true; return result; }
    var low = 2147483647; var high = -2147483647;
    for (var i = 0u; i < polygon.count; i = i + 1u) {
        let coordinate = polygon.vertices[i].position[axis];
        let index = floor(coordinate / safe_cell() + 0.5);
        if !finite(coordinate) || !finite(index) || index < -2147483000.0 || index > 2147483000.0 { result.valid = false; return result; }
        let cell_index = i32(index); low = min(low, cell_index); high = max(high, cell_index);
    }
    result.minimum = low; result.maximum = high; return result;
}
fn degenerate_cell_owned(triangle: u32, cell: vec3<i32>) -> bool {
    let base = triangle * 3u;
    let p0 = source_position(reference[base].position); let p1 = source_position(reference[base + 1u].position); let p2 = source_position(reference[base + 2u].position);
    let centroid = (p0 + p1 + p2) / 3.0; let eps = 1e-7;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let lo = min(p0[axis], min(p1[axis], p2[axis])); let hi = max(p0[axis], max(p1[axis], p2[axis]));
        if hi == lo {
            let owner = cell_index(centroid[axis]);
            if abs((centroid[axis] / safe_cell() + 0.5) - floor(centroid[axis] / safe_cell() + 0.5)) <= eps && owner != cell[axis] { return false; }
        }
    }
    return true;
}
fn cell_records(triangle: u32) -> CellResult {
    var result = CellResult(0u, true, 0u);
    let bounds = cell_bounds(triangle);
    if !bounds.valid { result.valid = false; result.reason = 2u; return result; }
    let normalized = normalized_triangle_polygon(triangle);
    if !normalized.valid { result.valid = false; result.reason = 2u; return result; }
    if normalized.count == 0u { return result; }
    var x = bounds.minimum.x;
    loop {
        let slab = clip_cell_axis(normalized, 0u, x); if !slab.valid { result.valid = false; result.reason = 2u; return result; }
        let y_bounds = axis_bounds(slab, 1u); if !y_bounds.valid { result.valid = false; result.reason = 2u; return result; }
        if !y_bounds.empty {
            var y = y_bounds.minimum;
            loop {
                let column = clip_cell_axis(slab, 1u, y); if !column.valid { result.valid = false; result.reason = 2u; return result; }
                let z_bounds = axis_bounds(column, 2u); if !z_bounds.valid { result.valid = false; result.reason = 2u; return result; }
                if !z_bounds.empty {
                    var z = z_bounds.minimum;
                    loop {
                        let cell = vec3<i32>(x, y, z);
                        if degenerate_cell_owned(triangle, cell) {
                            let cell_result = polygon_records(clip_cell_axis(column, 2u, z));
                            if !cell_result.valid { return cell_result; }
                            if cell_result.records > params.output_capacity || result.records > params.output_capacity - cell_result.records { result.valid = false; result.reason = 1u; return result; }
                            result.records = result.records + cell_result.records;
                        }
                        if z == z_bounds.maximum { break; } z = z + 1;
                    }
                }
                if y == y_bounds.maximum { break; } y = y + 1;
            }
        }
        if x == bounds.maximum.x { break; } x = x + 1;
    }
    return result;
}

fn candidate_records(candidate: u32) -> CellResult {
    if f32(candidate) > 16777215.0 { return CellResult(0u, false, 3u); }
    if params.mode != 0u { return cell_records(candidate); }
    var result = CellResult(0u, true, 0u);
    for (var band = 0u; band < max(params.bands, 1u); band = band + 1u) {
        let part = polygon_records(band_polygon(candidate, band));
        if !part.valid { return part; }
        if part.records > params.output_capacity || result.records > params.output_capacity - part.records { return CellResult(0u, false, 1u); }
        result.records = result.records + part.records;
    }
    return result;
}
fn emit_polygon(polygon: Polygon, triangle: u32, start: u32) {
    if !polygon.valid || polygon.count < 3u { return; }
    var written = 0u;
    for (var i = 1u; i + 1u < polygon.count; i = i + 1u) {
        let a = polygon.vertices[0].barycentric; let b = polygon.vertices[i].barycentric; let c = polygon.vertices[i + 1u].barycentric;
        let twice_area = length(cross(polygon.vertices[i].position - polygon.vertices[0].position, polygon.vertices[i + 1u].position - polygon.vertices[0].position));
        if twice_area > 0.0 && finite(twice_area) {
            let index = start + written;
            if params.output_capacity >= 3u && index <= params.output_capacity - 3u {
                map[index] = Vec4Vertex(vec4<f32>(a, f32(triangle))); map[index + 1u] = Vec4Vertex(vec4<f32>(b, f32(triangle))); map[index + 2u] = Vec4Vertex(vec4<f32>(c, f32(triangle)));
            }
            written = written + 3u;
        }
    }
}
@compute @workgroup_size(256, 1, 1)
fn clear_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < params.output_capacity { map[id.x] = Vec4Vertex(vec4<f32>(0.0, 0.0, 0.0, -1.0)); }
}
@compute @workgroup_size(1, 1, 1)
fn clear_status_main() { atomicStore(&status[3], 0u); }

var<workgroup> local_prefix: array<u32, 256>;
var<workgroup> local_error: atomic<u32>;

fn sat_add(a: u32, b: u32, limit: u32) -> u32 {
    if b > limit || a > limit - b { return limit; }
    return a + b;
}

@compute @workgroup_size(256, 1, 1)
fn count_main(
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(workgroup_id) group: vec3<u32>,
) {
    let lane = local.x;
    let candidate = group.x * 256u + lane;
    var value = 0u;
    var reason = 0u;
    if candidate < params.candidate_count {
        let result = candidate_records(candidate);
        if result.valid { value = result.records; }
        else { reason = max(result.reason, 1u); }
        counts[candidate] = select(value, 0xffffffffu, reason != 0u);
    }
    if lane == 0u { atomicStore(&local_error, 0u); }
    workgroupBarrier();
    if reason != 0u { atomicMax(&local_error, reason); }
    local_prefix[lane] = min(select(value, 0u, reason != 0u), params.output_capacity);
    workgroupBarrier();
    var offset = 1u;
    loop {
        if offset >= 256u { break; }
        var addend = 0u;
        if lane >= offset { addend = local_prefix[lane - offset]; }
        workgroupBarrier();
        if addend > params.output_capacity || local_prefix[lane] > params.output_capacity - min(addend, params.output_capacity) {
            atomicMax(&local_error, 1u);
        }
        local_prefix[lane] = sat_add(local_prefix[lane], addend, params.output_capacity);
        workgroupBarrier();
        offset = offset * 2u;
    }
    if candidate < params.candidate_count {
        let own = select(value, 0u, reason != 0u);
        if local_prefix[lane] < own { prefix[candidate] = 0u; }
        else { prefix[candidate] = local_prefix[lane] - own; }
    }
    if lane == 255u {
        let block = group.x;
        let block_count = (params.candidate_count + 255u) / 256u;
        counts[params.candidate_count + block] = local_prefix[255u];
        prefix[params.candidate_count + block] = atomicLoad(&local_error);
        if block >= block_count { counts[params.candidate_count + block] = 0u; prefix[params.candidate_count + block] = 0u; }
    }
}

@compute @workgroup_size(1, 1, 1)
fn scan_main() {
    atomicStore(&status[0], 0u);
    atomicStore(&status[1], 0u);
    atomicStore(&status[2], 0u);
    var total = 0u;
    let block_count = (params.candidate_count + 255u) / 256u;
    for (var block = 0u; block < block_count; block = block + 1u) {
        let block_sum = counts[params.candidate_count + block];
        let block_error = prefix[params.candidate_count + block];
        if block_error != 0u { atomicStore(&status[1], 1u); atomicMax(&status[2], block_error); atomicMax(&status[3], block_error); }
        prefix[params.candidate_count + block] = total;
        if block_sum > params.output_capacity || total > params.output_capacity - block_sum {
            atomicStore(&status[1], 1u);
            atomicMax(&status[2], 1u);
            atomicMax(&status[3], 1u);
            total = params.output_capacity;
        } else { total = total + block_sum; }
    }
    atomicStore(&status[0], total);
}

fn emit_cells(triangle: u32, start: u32) {
    let bounds = cell_bounds(triangle); if !bounds.valid { return; }
    let normalized = normalized_triangle_polygon(triangle); if !normalized.valid { return; }
    var x = bounds.minimum.x; var written = 0u;
    loop {
        let slab = clip_cell_axis(normalized, 0u, x); if !slab.valid { return; }
        let y_bounds = axis_bounds(slab, 1u); if !y_bounds.valid { return; }
        if !y_bounds.empty {
            var y = y_bounds.minimum;
            loop {
                let column = clip_cell_axis(slab, 1u, y); if !column.valid { return; }
                let z_bounds = axis_bounds(column, 2u); if !z_bounds.valid { return; }
                if !z_bounds.empty {
                    var z = z_bounds.minimum;
                    loop {
                        let cell = vec3<i32>(x, y, z);
                        if degenerate_cell_owned(triangle, cell) {
                            let polygon = clip_cell_axis(column, 2u, z); let part = polygon_records(polygon);
                            if !part.valid { return; }
                            if part.records > 0u { emit_polygon(polygon, triangle, start + written); written = written + part.records; }
                        }
                        if z == z_bounds.maximum { break; } z = z + 1;
                    }
                }
                if y == y_bounds.maximum { break; } y = y + 1;
            }
        }
        if x == bounds.maximum.x { break; } x = x + 1;
    }
}

@compute @workgroup_size(256, 1, 1)
fn emit_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if atomicLoad(&status[1]) != 0u || id.x >= params.candidate_count { return; }
    let block = id.x / 256u;
    let start = prefix[id.x] + prefix[params.candidate_count + block];
    if params.mode == 0u {
        var written = 0u;
        for (var band = 0u; band < max(params.bands, 1u); band = band + 1u) {
            let polygon = band_polygon(id.x, band); let part = polygon_records(polygon);
            if part.valid && part.records > 0u { emit_polygon(polygon, id.x, start + written); written = written + part.records; }
        }
    } else { emit_cells(id.x, start); }
}
