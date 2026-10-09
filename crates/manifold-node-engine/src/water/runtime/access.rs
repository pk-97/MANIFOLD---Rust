//! Borrow the water extension without giving it ownership of the generic runtime.

use super::WaterRuntimeState;
use crate::runtime::ModifierPreviewContext;
use crate::{
    exec::{execution::Executor, execution_plan::ExecutionPlan},
    graph::Graph,
    runtime::{
        PresetRuntime,
        extensions::{RuntimeContext, RuntimeSlots},
    },
};
use manifold_core::PresetTypeId;

pub struct WaterRuntime<'a> {
    pub(super) water: &'a mut WaterRuntimeState,
    pub(super) graph: &'a mut Graph,
    pub(super) plan: &'a ExecutionPlan,
    pub(super) executor: &'a mut Executor,
    pub(super) effect_nodes: RuntimeSlots<'a>,
    pub(super) type_id: Option<&'a PresetTypeId>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) last_forced_outputs_epoch: u64,
    pub(super) forced_outputs_stale: bool,
}

pub struct WaterRuntimeRef<'a> {
    pub(super) water: &'a WaterRuntimeState,
    pub(super) graph: &'a Graph,
    pub(super) plan: &'a ExecutionPlan,
    pub(super) effect_nodes: RuntimeSlots<'a>,
    pub(super) last_forced_outputs_epoch: u64,
    pub(super) forced_outputs_stale: bool,
}

impl<'a> WaterRuntime<'a> {
    pub(super) fn from_parts(
        water: &'a mut WaterRuntimeState,
        context: RuntimeContext<'a>,
    ) -> Self {
        let RuntimeContext {
            graph,
            plan,
            executor,
            effect_nodes,
            type_id,
            width,
            height,
            last_forced_outputs_epoch,
            forced_outputs_stale,
        } = context;
        Self {
            water,
            graph,
            plan,
            executor,
            effect_nodes,
            type_id,
            width,
            height,
            last_forced_outputs_epoch,
            forced_outputs_stale,
        }
    }

    pub(super) fn borrow(
        water: &'a mut WaterRuntimeState,
        context: &'a mut RuntimeContext<'_>,
    ) -> Self {
        Self {
            water,
            graph: context.graph,
            plan: context.plan,
            executor: context.executor,
            effect_nodes: context.effect_nodes,
            type_id: context.type_id,
            width: context.width,
            height: context.height,
            last_forced_outputs_epoch: context.last_forced_outputs_epoch,
            forced_outputs_stale: context.forced_outputs_stale,
        }
    }

    pub(super) fn as_ref(&self) -> WaterRuntimeRef<'_> {
        WaterRuntimeRef {
            water: self.water,
            graph: self.graph,
            plan: self.plan,
            effect_nodes: self.effect_nodes,
            last_forced_outputs_epoch: self.last_forced_outputs_epoch,
            forced_outputs_stale: self.forced_outputs_stale,
        }
    }
}

pub trait WaterRuntimeExt {
    fn water(&mut self) -> WaterRuntime<'_>;
    fn water_ref(&self) -> WaterRuntimeRef<'_>;
    fn write_modifier_fluid_domains(
        &self,
        context: &ModifierPreviewContext,
        output: &mut Vec<(
            manifold_core::NodeId,
            crate::water::fluid::FluidDomainSnapshot,
        )>,
    );
}

impl WaterRuntimeExt for PresetRuntime {
    fn water(&mut self) -> WaterRuntime<'_> {
        let (state, context) = self
            .extension_mut::<WaterRuntimeState>()
            .expect("water runtime extension is linked");
        WaterRuntime::from_parts(state, context)
    }

    fn water_ref(&self) -> WaterRuntimeRef<'_> {
        WaterRuntimeRef {
            water: self
                .extension::<WaterRuntimeState>()
                .expect("water runtime extension is linked"),
            graph: &self.graph,
            plan: &self.plan,
            effect_nodes: self.extension_slots(),
            last_forced_outputs_epoch: self.compiled_outputs_epoch(),
            forced_outputs_stale: self.awaiting_forced_outputs_rebuild(),
        }
    }
    /// Append fluid snapshots from the selected generated modifier copy and
    /// translate each generated node id back to its authored address in place.
    fn write_modifier_fluid_domains(
        &self,
        context: &ModifierPreviewContext,
        output: &mut Vec<(
            manifold_core::NodeId,
            crate::water::fluid::FluidDomainSnapshot,
        )>,
    ) {
        let start = output.len();
        self.water_ref().write_fluid_domains_watched(output);
        let mut write = start;
        for read in start..output.len() {
            let authored = self
                .modifier_preview_local_node(context, output[read].0.as_str())
                .cloned();
            if let Some(authored) = authored {
                let snapshot = output[read].1;
                output[write] = (authored, snapshot);
                write += 1;
            }
        }
        output.truncate(write);
    }
}
