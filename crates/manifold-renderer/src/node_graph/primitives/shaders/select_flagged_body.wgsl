// node.select_flagged — fusable BUFFER body, GATHER. Entry idx is the first
// index c with total[c] > idx: a binary search of the inclusive running
// total, gathered through buf_total. Past the grand total: 0xffffffff.

fn body(idx: u32, count: u32, capacity: i32) -> u32 {
    let n = arrayLength(&buf_total);
    if n == 0u || idx >= buf_total[n - 1u] {
        return 0xffffffffu;
    }
    var lo = 0u;
    var hi = n - 1u;
    while lo < hi {
        let mid = lo + (hi - lo) / 2u;
        if buf_total[mid] > idx {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    return lo;
}
