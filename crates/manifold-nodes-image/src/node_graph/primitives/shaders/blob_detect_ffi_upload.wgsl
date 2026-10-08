// node.blob_detect_ffi — upload the CPU-side blob list (packed
// fixed-size buffer of MAX_BLOB_CAP Blob entries) into the
// runtime-allocated Array<Blob> output buffer.
//
// One thread per output slot. Slots beyond `count` are zeroed so
// stale data from a previous inference never leaks into downstream
// consumers.

const MAX_BLOB_CAP: u32 = 8u;

struct Blob {
    x:      f32,
    y:      f32,
    width:  f32,
    height: f32,
};

struct Uniforms {
    count:    u32,    // valid blobs in src (0..MAX_BLOB_CAP)
    capacity: u32,    // size of out buffer (Array<Blob> chain-build cap)
    _pad0:    u32,
    _pad1:    u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<uniform> src: array<Blob, MAX_BLOB_CAP>;
@group(0) @binding(2) var<storage, read_write> out_blobs: array<Blob>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.capacity {
        return;
    }
    if i < u.count && i < MAX_BLOB_CAP {
        out_blobs[i] = src[i];
    } else {
        out_blobs[i] = Blob(0.0, 0.0, 0.0, 0.0);
    }
}
