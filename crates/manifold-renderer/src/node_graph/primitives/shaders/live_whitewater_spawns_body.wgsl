// node.live_whitewater_spawns — fusable BUFFER body, COINCIDENT. 1 for a
// spawn slot holding a particle (lifetime above 0), else 0: the counts a
// node.running_total turns into node.append_whitewater's placement.

fn body(idx: u32, count: u32, e_spawns: Element) -> u32 {
    return select(0u, 1u, e_spawns.position_lifetime.w > 0.0);
}
