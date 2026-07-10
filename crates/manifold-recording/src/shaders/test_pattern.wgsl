// Synthetic block-pattern test frame for the recording proof harness
// (cargo test -p manifold-recording --features recording-proofs).
//
// Bakes a frame index into pixels as a one-row, 26-block luma code so the
// harness's oracle (crate::proofs::probe) can decode frame identity back out
// of a file that has gone through the REAL sRGB conversion shader and the
// REAL AVAssetWriter/ProRes encode — proving drops, duplicates, and
// reordering exactly, which a "N frames encoded" counter cannot (D4 in
// docs/LIVE_RECORDING_PROOFS_DESIGN.md).
//
// Pattern (frame width / NUM_BLOCKS per block, full-height solid stripe):
//   block 0        = white  (polarity)
//   block 1        = black  (locator)
//   blocks 2..26    = frame_index, MSB-first, white = bit 1
//
// Solid full-height luma stripes survive ProRes 4:2:2 quantization with
// enormous margin, so the reader's threshold-at-128 decode is unambiguous.

struct Params {
    frame_index: u32,
    width: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

// Must match crate::proofs::{NUM_BLOCKS, INDEX_BITS} and the Rust-side
// decoder in probe()'s decode_frame_indices — these three copies of the
// constants are the pattern's actual contract, not this comment.
const NUM_BLOCKS: u32 = 26u;
const INDEX_BITS: u32 = 24u;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let block_width = max(params.width / NUM_BLOCKS, 1u);
    var block = gid.x / block_width;
    if block >= NUM_BLOCKS {
        block = NUM_BLOCKS - 1u;
    }

    var white: bool;
    if block == 0u {
        white = true;
    } else if block == 1u {
        white = false;
    } else {
        let bit_index = block - 2u; // 0 = MSB of the frame index
        let shift = INDEX_BITS - 1u - bit_index;
        white = ((params.frame_index >> shift) & 1u) != 0u;
    }

    let luma = select(0.0, 1.0, white);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(luma, luma, luma, 1.0));
}
