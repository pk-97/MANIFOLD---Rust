//! Native take lifecycle with a controlled source capability. Actual glTF
//! animation sampling/admission is covered separately, not by this test node.
use super::*;

use crate::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::scene::source_asset::SourceAssetIdentity;
use crate::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::parameters::{ParamDef, ParamValue};
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const PENDING: u8 = 0;
const READY_A: u8 = 1;
const READY_B: u8 = 2;
const FAILED: u8 = 3;

struct AssetGravity {
    node_type: EffectNodeType,
    state: Arc<AtomicU8>,
}

impl EffectNode for AssetGravity {
    fn is_pure(&self) -> bool {
        true
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.node_type
    }

    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
    }

    fn inputs(&self) -> &[NodeInput] {
        &[]
    }

    fn outputs(&self) -> &[NodeOutput] {
        static OUTPUTS: [NodeOutput; 1] = [NodePort {
            name: Cow::Borrowed("gravity"),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Output,
            required: false,
        }];
        &OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        ctx.outputs.set_scalar("gravity", ParamValue::Float(-9.81));
    }

    fn source_asset_identity(
        &self,
        _params: &crate::exec::effect_node::ParamValues,
    ) -> SourceAssetIdentity<'_> {
        match self.state.load(Ordering::Acquire) {
            PENDING => SourceAssetIdentity::Pending,
            READY_A => SourceAssetIdentity::Ready([0xAA; 32]),
            READY_B => SourceAssetIdentity::Ready([0xBB; 32]),
            FAILED => SourceAssetIdentity::Failed("asset decode failed"),
            _ => SourceAssetIdentity::Failed("invalid test asset state"),
        }
    }
}

fn asset_definition(directory: &std::path::Path, cache_mode: u32) -> EffectGraphDef {
    let mut def = serde_json::to_value(runtime_definition()).unwrap();
    def["nodes"][0]["params"]["cache_mode"] = serde_json::json!({"type":"Enum","value":cache_mode});
    def["nodes"][0]["params"]["cache_path"] =
        serde_json::json!({"type":"String","value":directory.to_str().unwrap()});
    def["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": 5,
            "nodeId": "animation",
            "typeId": "node.gltf_animation_source"
        }));
    def["wires"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "fromNode": 5,
            "fromPort": "gravity",
            "toNode": 0,
            "toPort": "gravity"
        }));
    serde_json::from_value(def).unwrap()
}

fn build(state: &Arc<AtomicU8>, directory: &std::path::Path, cache_mode: u32) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
    registry.register("test.fluid_time", || {
        fluid_time_observer()
    });
    let state = Arc::clone(state);
    registry.register("node.gltf_animation_source", move || {
        Box::new(AssetGravity {
            node_type: EffectNodeType::new("node.gltf_animation_source"),
            state: Arc::clone(&state),
        })
    });
    PresetRuntime::from_def(asset_definition(directory, cache_mode), &registry, None).unwrap()
}

fn time(seconds: f64) -> FrameTime {
    FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds::ZERO,
        frame_count: 0,
    }
}

fn state(runtime: &PresetRuntime) -> crate::water::fluid::FluidDomainState {
    let fluid = runtime
        .graph
        .instance_by_node_id(&NodeId::new("fluid"))
        .unwrap();
    runtime
        .graph
        .get_node(fluid)
        .unwrap()
        .node
        .fluid_domain_snapshot()
        .unwrap()
        .state
}

fn directory(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "manifold-fluid-asset-take-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn native_take_requires_loaded_asset_content_and_refreshes_after_rebuild() {
    use crate::water::fluid::FluidDomainState;

    for (label, status) in [("pending", PENDING), ("failed", FAILED)] {
        let directory = directory(label);
        let source_state = Arc::new(AtomicU8::new(status));
        let mut runtime = build(&source_state, &directory, 1);
        runtime.execute_frame(time(0.0));
        runtime.execute_frame(time(0.1));
        assert_eq!(state(&runtime), FluidDomainState::Failed);
        assert!(
            crate::water::fluid::FluidTakeReplay::open(&directory).is_err(),
            "{label} asset state must not create a usable take"
        );
        source_state.store(READY_A, Ordering::Release);
        runtime.execute_frame(time(0.1));
        runtime.execute_frame(time(0.2));
        assert_eq!(state(&runtime), FluidDomainState::Ready);
        assert_eq!(
            crate::water::fluid::FluidTakeReplay::open(&directory)
                .unwrap()
                .recorded_tick(),
            12,
            "recovery must preserve the clock anchored before the load completed"
        );
        drop(runtime);
        std::fs::remove_dir_all(directory).unwrap();
    }

    let directory = directory("ready");
    let source_state = Arc::new(AtomicU8::new(READY_A));
    let mut recorded = build(&source_state, &directory, 1);
    recorded.execute_frame(time(0.0));
    recorded.execute_frame(time(0.1));
    assert_eq!(state(&recorded), FluidDomainState::Ready);
    drop(recorded);

    let mut playback = build(&source_state, &directory, 2);
    playback.execute_frame(time(0.1));
    assert_eq!(state(&playback), FluidDomainState::Ready);

    source_state.store(READY_B, Ordering::Release);
    playback.execute_frame(time(0.1));
    assert_eq!(
        state(&playback),
        FluidDomainState::Failed,
        "a different loaded asset snapshot must reject playback at the held time"
    );
    source_state.store(READY_A, Ordering::Release);
    playback.execute_frame(time(0.1));
    assert_eq!(
        state(&playback),
        FluidDomainState::Ready,
        "restoring the recorded content must recover playback at the held time"
    );

    source_state.store(READY_B, Ordering::Release);
    let mut rebuilt = build(&source_state, &directory, 2);
    rebuilt.carry_generator_state_from(&mut playback);
    rebuilt.execute_frame(time(0.1));
    assert_eq!(
        state(&rebuilt),
        FluidDomainState::Failed,
        "a rebuilt graph must observe the current asset state even with native state carried"
    );
    source_state.store(READY_A, Ordering::Release);
    rebuilt.execute_frame(time(0.1));
    assert_eq!(state(&rebuilt), FluidDomainState::Ready);

    drop(rebuilt);
    drop(playback);
    std::fs::remove_dir_all(directory).unwrap();
}
