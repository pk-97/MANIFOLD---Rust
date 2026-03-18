// Mechanical port of BitonicSort.compute — reusable bitonic merge sort.
// Operates on array<vec2<u32>> where x=sort key, y=original index.
// Each row is sorted independently. Requires O(log²N) dispatches with
// varying level and step uniforms.
//
// Unity ref: Assets/Resources/Compute/BitonicSort.compute — BitonicSortStep kernel.
//
// Dispatch pattern (from ComputeSortEffect.cs lines 188-196):
//   for level = 0 to log2(paddedWidth)-1:
//       for step = level down to 0:
//           dispatch(ceil(paddedWidth/2 / 256), height, 1)

struct BitonicParams {
    level:        u32,  // _Level — current level in bitonic merge network
    step:         u32,  // _Step  — current step within the level
    padded_width: u32,  // _PaddedWidth — row width padded to power of 2
    height:       u32,  // _Height — number of rows
}

@group(0) @binding(0) var<uniform> params: BitonicParams;
// sort_buffer: vec2<u32> per element — x=key, y=original index
@group(0) @binding(1) var<storage, read_write> sort_buffer: array<vec2<u32>>;

// BitonicSort.compute line 22 — [numthreads(256, 1, 1)]
@compute @workgroup_size(256, 1, 1)
fn bitonic_sort_step(@builtin(global_invocation_id) id: vec3<u32>) {
    let thread_idx = id.x;
    let row        = id.y;

    // BitonicSort.compute line 28
    if row >= params.height { return; }

    // BitonicSort.compute lines 31-32 — each thread handles one compare-and-swap pair
    let half_block = 1u << params.step;
    let block      = half_block << 1u;

    // BitonicSort.compute lines 35-38 — find pair indices within the row
    let block_idx = thread_idx / half_block;
    let offset    = thread_idx % half_block;

    let idx_a = block_idx * block + offset;
    let idx_b = idx_a + half_block;

    // BitonicSort.compute line 41
    if idx_b >= params.padded_width { return; }

    // BitonicSort.compute lines 44-45 — global buffer indices
    let global_a = row * params.padded_width + idx_a;
    let global_b = row * params.padded_width + idx_b;

    let a = sort_buffer[global_a];
    let b = sort_buffer[global_b];

    // BitonicSort.compute lines 51-53 — determine sort direction
    // ascending or descending based on level
    let level_block = 1u << (params.level + 1u);
    let ascending   = ((idx_a / level_block) % 2u) == 0u;

    // BitonicSort.compute lines 56-61 — compare and swap
    let should_swap = (ascending && (a.x > b.x)) || (!ascending && (a.x < b.x));
    if should_swap {
        sort_buffer[global_a] = b;
        sort_buffer[global_b] = a;
    }
}
