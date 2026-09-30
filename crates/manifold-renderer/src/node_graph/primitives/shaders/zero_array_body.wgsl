// node.zero_array — fusable BUFFER body. Every element of the signed
// accumulator becomes 0; `in` and `out` alias one buffer (cleared in place).
fn body(idx: u32, count: u32, e_in: i32) -> i32 {
    return 0;
}
