use crate::exec::effect_node::EffectNode;
use crate::exec::effect_node::EffectNodeContext;
use crate::exec::effect_node::EffectNodeType;
use crate::exec::effect_node::NodeInstanceId;
use crate::exec::execution_plan::ExecutionPlan;
use crate::exec::execution_plan::ResourceId;
use crate::parameters::ParamDef;
use crate::ports::PortType;
use std::sync::{Arc, Mutex};
use crate::ports::{NodeInput, NodeOutput, NodePort, PortKind};
        use crate::mesh::MeshVertex;
        use crate::scene::mesh_change::{MeshOutputRule, MeshRevisionRule};
        use crate::ports::ArrayType;

        fn mesh_ty() -> PortType {
            PortType::Array(ArrayType::of_known::<MeshVertex>())
        }

        /// `MeshVertex`-layout producer/consumer with scripted
        /// write/unchanged/pending declarations and an optional
        /// `mesh_output_rule` override (None = trait default, the
        /// conservative `Written`/`Written`). The rule lives behind a
        /// shared handle because the compiled rule is a plan-compile-time
        /// snapshot — a test that flips the source's topology mid-run
        /// recompiles the plan with the handle changed.
        pub struct MeshNode {
            type_id: EffectNodeType,
            pub(crate) inputs: Vec<NodeInput>,
            pub(crate) outputs: Vec<NodeOutput>,
            declare_unchanged: Arc<Mutex<bool>>,
            declare_pending: Arc<Mutex<bool>>,
            rule: Arc<Mutex<Option<MeshOutputRule<'static>>>>,
        }

        pub(crate) fn shared_rule(rule: Option<MeshOutputRule<'static>>) -> Arc<Mutex<Option<MeshOutputRule<'static>>>> {
            Arc::new(Mutex::new(rule))
        }

        /// Handles a producer hands back: unchanged/pending declaration
        /// flags plus the shared rule handle (see [`MeshNode`]).
        type ProducerHandles = (
            Arc<Mutex<bool>>,
            Arc<Mutex<bool>>,
            Arc<Mutex<Option<MeshOutputRule<'static>>>>,
        );

        impl MeshNode {
            pub fn producer(rule: Option<MeshOutputRule<'static>>) -> (Self, ProducerHandles) {
                let declare_unchanged = Arc::new(Mutex::new(false));
                let declare_pending = Arc::new(Mutex::new(false));
                let rule = shared_rule(rule);
                (
                    Self {
                        type_id: EffectNodeType::new("test.mesh_node"),
                        inputs: vec![],
                        outputs: vec![output("out", mesh_ty())],
                        declare_unchanged: declare_unchanged.clone(),
                        declare_pending: declare_pending.clone(),
                        rule: rule.clone(),
                    },
                    (declare_unchanged, declare_pending, rule),
                )
            }

            #[cfg(test)]
            pub(crate) fn consumer(rule: Option<MeshOutputRule<'static>>) -> (Self, Arc<Mutex<bool>>, Arc<Mutex<bool>>) {
                let declare_unchanged = Arc::new(Mutex::new(false));
                let declare_pending = Arc::new(Mutex::new(false));
                (
                    Self {
                        type_id: EffectNodeType::new("test.mesh_consumer"),
                        inputs: vec![input("in", mesh_ty(), true)],
                        outputs: vec![output("out", mesh_ty())],
                        declare_unchanged: declare_unchanged.clone(),
                        declare_pending: declare_pending.clone(),
                        rule: shared_rule(rule),
                    },
                    declare_unchanged,
                    declare_pending,
                )
            }

            /// Terminal mesh consumer: an input and no outputs. Plan
            /// compile prunes UNCONSUMED outputs from a step, so every
            /// producer under test needs its mesh output wired somewhere
            /// to keep its resource in the plan.
            pub fn sink() -> Self {
                Self {
                    type_id: EffectNodeType::new("test.mesh_sink"),
                    inputs: vec![input("in", mesh_ty(), true)],
                    outputs: vec![],
                    declare_unchanged: Arc::new(Mutex::new(false)),
                    declare_pending: Arc::new(Mutex::new(false)),
                    rule: shared_rule(None),
                }
            }
        }

        impl EffectNode for MeshNode {
            fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
                crate::scene::depth_rule::DepthRule::Terminal
            }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                &self.inputs
            }
            fn outputs(&self) -> &[NodeOutput] {
                &self.outputs
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn mesh_output_rule(&self, _port: &str) -> MeshOutputRule<'_> {
                self.rule.lock().unwrap().unwrap_or(MeshOutputRule {
                    topology: MeshRevisionRule::Written,
                    positions: MeshRevisionRule::Written,
                })
            }
            fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
                if *self.declare_unchanged.lock().unwrap() {
                    ctx.mark_outputs_unchanged();
                }
                if *self.declare_pending.lock().unwrap() {
                    ctx.mark_outputs_pending();
                }
            }
        }

        /// The single output resource of `node`, the way production
        /// callers address plan resources.
        pub fn out_res(plan: &ExecutionPlan, node: NodeInstanceId) -> ResourceId {
            plan.steps()
                .iter()
                .find(|s| s.node == node)
                .and_then(|s| s.outputs.first())
                .map(|&(_, res)| res)
                .expect("node must have one output resource")
        }

        pub fn fixed_rule() -> MeshOutputRule<'static> {
            MeshOutputRule {
                topology: MeshRevisionRule::Fixed,
                positions: MeshRevisionRule::Fixed,
            }
        }

        pub struct DeclaredPrimitiveProbe {
            inner: Box<dyn EffectNode>,
        }

        impl DeclaredPrimitiveProbe {
            pub fn new(inner: Box<dyn EffectNode>) -> Self { Self { inner } }
        }

        impl EffectNode for DeclaredPrimitiveProbe {
            fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
                self.inner.depth_rule()
            }
            fn type_id(&self) -> &EffectNodeType {
                self.inner.type_id()
            }
            fn inputs(&self) -> &[NodeInput] {
                self.inner.inputs()
            }
            fn outputs(&self) -> &[NodeOutput] {
                self.inner.outputs()
            }
            fn parameters(&self) -> &[ParamDef] {
                self.inner.parameters()
            }
            fn mesh_output_rule(&self, port: &str) -> MeshOutputRule<'_> {
                self.inner.mesh_output_rule(port)
            }
            fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {
                // No-op actual write — see the struct doc.
            }
        }

    fn input(name: &'static str, ty: PortType, required: bool) -> NodeInput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Input,
            required,
        }
    }

    fn output(name: &'static str, ty: PortType) -> NodeOutput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Output,
            required: false,
        }
    }
