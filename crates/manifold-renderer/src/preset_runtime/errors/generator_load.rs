use super::*;

/// Errors produced when loading a generator preset (the generator
/// construction path of [`PresetRuntime`]).
#[derive(Debug)]
pub enum JsonGeneratorLoadError {
    SceneModifier(crate::node_graph::scene_modifier_expand::SceneModifierExpandError),
    /// JSON parsing failed.
    Json(serde_json::Error),
    /// The schema document failed to construct a Graph.
    Load(LoadError),
    /// The compiled graph had a static error (cycle, type mismatch, …).
    Compile(GraphError),
    /// The preset's JSON contains no `system.generator_input` node.
    MissingGeneratorInput,
    /// The preset's JSON contains no `system.final_output` node, or it
    /// isn't wired.
    MissingFinalOutput,
    /// BUG-125: the preset's JSON contains MORE THAN ONE `system.final_output`
    /// node. The tracked-output resolution (`graph.nodes().find(...)`) is a
    /// single, unordered lookup — a second `final_output` would be picked
    /// nondeterministically per process, and the per-frame canvas-target
    /// rebind (`replace_texture_2d`) would silently overwrite whichever one
    /// lost with the host canvas's format, up to a real GPU command-buffer
    /// fault. Rejected at load rather than silently picked.
    MultipleFinalOutputs {
        count: usize,
    },
    /// A primitive declared an `Array<T>` output but
    /// `EffectNode::array_output_capacity` returned `None` for that port.
    UnsizedArrayOutput {
        node_type: String,
        port: String,
    },
    /// Sibling of `UnsizedArrayOutput` for Texture3D.
    UnsizedTexture3DOutput {
        node_type: String,
        port: String,
    },
    /// Post-allocation catch-all: an `Array<T>` resource in the compiled plan
    /// has no bound slot or no underlying buffer.
    UnboundArrayResource {
        producer_handle: Option<String>,
        producer_node_type: String,
        producer_port: String,
        cause: &'static str,
    },
    /// A staged runtime resize could not be admitted or allocated.
    Resize(String),
    /// A stateful or GPU graph producer cannot be replayed at physics ticks.
    PhysicsSamplingUnsupported(String),
}

impl std::fmt::Display for JsonGeneratorLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SceneModifier(error) => error.fmt(f),
            Self::Json(e) => write!(f, "JSON parse error: {e}"),
            Self::Load(e) => write!(f, "graph load error: {e}"),
            Self::Compile(e) => write!(f, "graph compile error: {e:?}"),
            Self::MissingGeneratorInput => write!(
                f,
                "preset has no `{GENERATOR_INPUT_TYPE_ID}` node — required for generator graphs"
            ),
            Self::MissingFinalOutput => write!(
                f,
                "preset has no `{FINAL_OUTPUT_TYPE_ID}` node, or it is not wired"
            ),
            Self::MultipleFinalOutputs { count } => write!(
                f,
                "preset has {count} `{FINAL_OUTPUT_TYPE_ID}` nodes — exactly one is \
                 required; the tracked-output resolution can't disambiguate more than \
                 one (see BUG-125). Wire extra outputs to a non-FinalOutput dead-end \
                 sink and inspect via `dump_textures_all` instead."
            ),
            Self::UnsizedArrayOutput { node_type, port } => write!(
                f,
                "primitive `{node_type}` Array<T> output port `{port}` has no \
                 concrete size — `array_output_capacity` returned None. \
                 Add a `max_capacity` param, or override the method to derive \
                 size from a forward-dep input (not a state-capture port)."
            ),
            Self::UnsizedTexture3DOutput { node_type, port } => write!(
                f,
                "primitive `{node_type}` Texture3D output port `{port}` has no \
                 concrete dims — `texture_3d_output_dims` returned None. \
                 Add `vol_res` / `vol_depth` params, or override the method to \
                 derive dims from a forward-dep input."
            ),
            Self::UnboundArrayResource {
                producer_handle,
                producer_node_type,
                producer_port,
                cause,
            } => {
                let handle_part = match producer_handle {
                    Some(h) => format!(" (handle `{h}`)"),
                    None => String::new(),
                };
                write!(
                    f,
                    "Array<T> output of `{producer_node_type}.{producer_port}`{handle_part} \
                     has no bound buffer after chain build: {cause}. \
                     This is the post-allocation audit catching a wire \
                     whose source resource was never pre-bound."
                )
            }
            Self::Resize(error) => write!(f, "runtime resize preparation failed: {error}"),
            Self::PhysicsSamplingUnsupported(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for JsonGeneratorLoadError {}

impl From<serde_json::Error> for JsonGeneratorLoadError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<LoadError> for JsonGeneratorLoadError {
    fn from(e: LoadError) -> Self {
        Self::Load(e)
    }
}

impl From<GraphError> for JsonGeneratorLoadError {
    fn from(e: GraphError) -> Self {
        Self::Compile(e)
    }
}

impl From<crate::node_graph::scene_modifier_expand::SceneModifierExpandError>
    for JsonGeneratorLoadError
{
    fn from(error: crate::node_graph::scene_modifier_expand::SceneModifierExpandError) -> Self {
        Self::SceneModifier(error)
    }
}
