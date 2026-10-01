// A liquid domain's body-row upload: CPU words carried inline in the
// uniform, written into the provided bodies buffer in encoder order, so a row
// a previous frame's substeps may still read is never overwritten early. An
// IO endpoint of a CPU bridge (ADDING_PRIMITIVES.md exclusion 3).

struct UploadParams {
    start: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    words: array<vec4<u32>, 254>,
}

@group(0) @binding(0) var<uniform> params: UploadParams;
@group(0) @binding(1) var<storage, read_write> dst: array<vec4<u32>>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    dst[params.start + i] = params.words[i];
}
