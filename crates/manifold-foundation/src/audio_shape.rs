//! Audio-modulation shaping defaults: the one definition the engine's `AudioModShape`
//! (manifold-core) and the UI's reset targets (manifold-ui) both read.

pub const SENSITIVITY_DEFAULT: f32 = 1.0;
/// Instant: a hit (kick, transient) lands on its frame at full height; smoothing is opt-in.
pub const ATTACK_DEFAULT_MS: f32 = 0.0;
pub const RELEASE_DEFAULT_MS: f32 = 120.0;
