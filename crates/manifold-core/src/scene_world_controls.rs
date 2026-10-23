//! The Physics World controls a simulation takes: a liquid and a swarm follow
//! the World's gravity, still air and clock.

/// One Physics World control a node takes, and the node's own input that
/// takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldControlInput {
    /// The World's param.
    pub world: &'static str,
    /// The taking node's input port.
    pub input: &'static str,
}

const fn take(world: &'static str, input: &'static str) -> WorldControlInput {
    WorldControlInput { world, input }
}

/// Every Physics World control a simulation takes (a liquid, a swarm): its
/// gravity, its still air, and its clock, so the node follows World Speed and
/// restarts on World Reset.
pub const WORLD_INPUTS: [WorldControlInput; 6] = [
    take("gravity_x", "gravity_x"),
    take("gravity_y", "gravity"),
    take("gravity_z", "gravity_z"),
    take("air_resistance", "air_resistance"),
    take("speed", "speed"),
    take("reset", "reset"),
];
