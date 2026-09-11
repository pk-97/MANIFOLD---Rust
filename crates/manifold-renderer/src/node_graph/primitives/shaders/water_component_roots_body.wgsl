// Union has completed before this immutable gather dispatch. Every valid
// non-root parent is strictly smaller, so traversal terminates without a
// relaxation-pass budget or synchronization with another invocation.
fn body(idx: u32, count: u32) -> u32 {
    var current = idx;
    while (current < count) {
        let parent = buf_parents[current];
        if (parent == current) { return current; }
        if (parent >= current) { return 0xffffffffu; }
        current = parent;
    }
    return 0xffffffffu;
}
