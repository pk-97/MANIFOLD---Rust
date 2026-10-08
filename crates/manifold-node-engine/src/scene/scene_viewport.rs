//! Viewport camera configuration, host errors and the engine pass contract.

use std::fmt;

use manifold_gpu::GpuTexture;

use crate::runtime::frame_status::FrameRenderStatus;
use crate::exec::effect_node::EffectNodeContext;
use crate::scene::viewport_camera::ViewportCamera;

/// The camera and canvas used by one render-only viewport pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneViewportConfig {
    pub camera: ViewportCamera,
    pub width: u32,
    pub height: u32,
}

impl SceneViewportConfig {
    /// Reject dimensions and camera values that could make the native pass
    /// issue an invalid allocation or projection.
    pub fn is_valid(self) -> bool {
        let camera = self.camera;
        self.width > 0
            && self.width <= 4096
            && self.height > 0
            && self.height <= 4096
            && camera.target.iter().all(|value| value.is_finite())
            && camera.yaw.is_finite()
            && camera.pitch.is_finite()
            && camera.distance.is_finite()
            && camera.distance > 0.0
            && camera.fov_y.is_finite()
            && camera.fov_y > 0.0
            && camera.fov_y < std::f32::consts::PI
            && camera.near.is_finite()
            && camera.near > 0.0
            && camera.far.is_finite()
            && camera.near < camera.far
    }
}

/// Errors reported by the viewport host before or during target selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneViewportError {
    TargetNotFound,
    NotSceneRenderer,
    InvalidConfiguration,
}

impl fmt::Display for SceneViewportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetNotFound => f.write_str("viewport target not found"),
            Self::NotSceneRenderer => f.write_str("viewport target is not a scene renderer"),
            Self::InvalidConfiguration => f.write_str("viewport configuration is invalid"),
        }
    }
}

impl std::error::Error for SceneViewportError {}

/// Host-level target availability, separate from a rendered frame's validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneViewportHostError {
    MissingRuntime,
    InvalidTarget(SceneViewportError),
    Modifier(crate::runtime::ModifierPreviewError),
}

impl SceneViewportHostError {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingRuntime => "This scene is not active at the current time.",
            Self::InvalidTarget(SceneViewportError::TargetNotFound) => "This scene is not available in the current render.",
            Self::InvalidTarget(SceneViewportError::NotSceneRenderer) => "Select a scene renderer to use the 3D viewport.",
            Self::InvalidTarget(SceneViewportError::InvalidConfiguration) => "The viewport camera or dimensions are invalid.",
            Self::Modifier(error) => error.message(),
        }
    }
}

/// A persistent secondary view constructed by its source node.
pub trait ViewportPass: Send {
    fn render(&mut self, ctx: &mut EffectNodeContext<'_, '_>, config: SceneViewportConfig);
    fn texture(&self) -> Option<&GpuTexture>;
    fn status(&self) -> FrameRenderStatus;
    fn errors(&self) -> &[String];
    fn clear_state(&mut self);
}
