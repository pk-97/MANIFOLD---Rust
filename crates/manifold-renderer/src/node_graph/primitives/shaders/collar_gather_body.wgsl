// node.collar_gather — fusable BUFFER body, GATHER. One thread per element
// of the output collar vector: entry e reads the grid at its cell minus the
// constant c = vector[K] (0 when the vector has no element K); element K is
// sum[0] / cells. `entries`, `grid`, `vector` and `sum` are gathered through
// buf_entries, buf_grid, buf_vector and buf_sum.

fn body(idx: u32, count: u32) -> f32 {
    let k = arrayLength(&buf_entries);
    let cells = arrayLength(&buf_grid);
    if idx == k {
        if arrayLength(&buf_sum) == 0u || cells == 0u {
            return 0.0;
        }
        return buf_sum[0] / f32(cells);
    }
    if idx > k {
        return 0.0;
    }
    let cell = buf_entries[idx];
    if cell >= cells {
        return 0.0;
    }
    var c = 0.0;
    if arrayLength(&buf_vector) > k {
        c = buf_vector[k];
    }
    return buf_grid[cell] - c;
}
