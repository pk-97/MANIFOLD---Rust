//! Capture graph fields and physical recipients without advancing simulation.
use std::sync::Arc;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_core::{NodeId, Seconds};
use manifold_physics::input::{AppliedEvent, EventStamp};
use manifold_physics::{FieldValue, TickStamp};

use super::{FrameTime, PresetRuntime};
use crate::node_graph::physics_events::{ImpulseTarget, ResolvedNodeImpulse};
use crate::node_graph::{
    NodeInstanceId, ParamValue, ParamValues, PortType, PrimitiveRegistry, ResourceId,
};

struct Recipient {
    id: NodeId,
    instance: NodeInstanceId,
    target: ImpulseTarget,
}

/// Reusable capture plan. Build when authoring a binding or changing its
/// selection, after installing the matching graph. Only stateless CPU field
/// ancestry is admitted; capture never runs a solver, trigger latch or GPU node.
pub struct PreparedSceneImpulse {
    identity: Arc<()>,
    plan_epoch: u64,
    recipients: Arc<[Recipient]>,
    field: ResourceId,
    steps: Vec<bool>,
    params: Vec<Option<ParamValues>>,
}

/// Owned event payload with reusable recipient storage. Allocate with
/// `PreparedSceneImpulse::new_capture` before the producer starts. Capture
/// owns the field, native epochs and mapped timestamps; delivery reads no
/// current field controls or target selection.
pub struct CapturedSceneImpulse {
    identity: Arc<()>,
    recipients: Arc<[Recipient]>,
    stamps: Vec<EventStamp>,
    planned: Vec<Option<TickStamp>>,
    field: Option<FieldValue>,
    source: Option<FrameTime>,
}

impl PreparedSceneImpulse {
    pub(super) fn rearm(&mut self, identity: &Arc<()>, captured: &mut CapturedSceneImpulse) {
        self.identity = identity.clone();
        captured.identity = identity.clone();
        captured.clear();
    }

    pub fn new_capture(&self) -> CapturedSceneImpulse {
        CapturedSceneImpulse {
            identity: self.identity.clone(),
            recipients: self.recipients.clone(),
            stamps: Vec::with_capacity(self.recipients.len()),
            planned: vec![None; self.recipients.len()],
            field: None,
            source: None,
        }
    }
}

impl CapturedSceneImpulse {
    pub(super) fn has_stale_epoch(&self, graph: &crate::node_graph::Graph) -> bool {
        self.source.is_some() && self.recipients.iter().zip(&self.stamps).any(|(recipient, stamp)| {
            graph.get_node(recipient.instance).and_then(|node| node.node.physics_impulse_epoch())
                .is_some_and(|epoch| epoch != stamp.epoch)
        })
    }

    /// Explicit acknowledgement/cancellation by the producer. Never implicit
    /// in a failed capture or delivery. Retains all allocated storage.
    pub fn clear(&mut self) {
        self.stamps.clear();
        self.planned.fill(None);
        self.field = None;
        self.source = None;
    }

    pub fn source_time(&self) -> Option<FrameTime> {
        self.source
    }

    pub fn scheduled_ticks(&self) -> impl Iterator<Item = (&NodeId, Option<TickStamp>)> {
        self.recipients
            .iter()
            .zip(&self.planned)
            .map(|(recipient, &tick)| (&recipient.id, tick))
    }

    pub fn is_scheduled(&self) -> bool {
        self.field.is_some() && self.planned.iter().all(Option::is_some)
    }
}

impl PresetRuntime {
    /// Capture a live source observation using each recipient's accepted
    /// simulation clock. Apply the source's resolved controls before calling.
    /// Scene setup and GPU-derived geometry must already have been evaluated
    /// by a full frame; this entry point samples live CPU controls only.
    /// Historical observations must be supplied in order; later render inputs
    /// cannot reconstruct an earlier audio event.
    pub fn capture_scene_impulse_at_source(
        &mut self,
        binding: &mut PreparedSceneImpulse,
        captured: &mut CapturedSceneImpulse,
        source: FrameTime,
        sequence: u64,
    ) -> Result<(), String> {
        self.validate_impulse_capture(binding, captured)?;
        self.observe_physics_at_source(source)?;
        self.capture_scene_impulse_with_stamp(
            binding,
            captured,
            source,
            sequence,
            |_, node, transport, sequence| node.physics_impulse_stamp(transport, sequence),
        )
    }

    fn validate_impulse_capture(
        &self,
        binding: &PreparedSceneImpulse,
        captured: &CapturedSceneImpulse,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&binding.identity, &self.impulse_identity)
            || !Arc::ptr_eq(&binding.recipients, &captured.recipients)
        {
            return Err(
                "Impulse: capture binding belongs to a different graph or selection".into(),
            );
        }
        if captured.field.is_some() {
            return Err(
                "Impulse: acknowledge the previous capture before reusing its storage".into(),
            );
        }
        if self.forced_outputs_stale
            || binding.plan_epoch != self.last_forced_outputs_epoch
            || binding.plan_epoch != self.graph.forced_outputs_epoch()
        {
            return Err("Impulse: execution outputs changed; rebuild and prepare the binding again".into());
        }
        Ok(())
    }

    /// `owner` is the canonical graph used to install this runtime. Scoped
    /// scene refs are validated there; physical leaves retain globally unique
    /// document IDs through flattening. The field source is a compiled leaf
    /// output (including a generated modifier leaf), in scene-space m/s.
    pub fn prepare_scene_impulse(
        &self,
        owner: &EffectGraphDef,
        scene: &SceneNodeRef,
        selection: &SceneTargetSelection,
        field_node: &NodeId,
        field_port: &str,
    ) -> Result<PreparedSceneImpulse, String> {
        if self.forced_outputs_stale
            || self.graph.forced_outputs_epoch() != self.last_forced_outputs_epoch
        {
            return Err("Impulse: rebuild the changed graph before preparing a binding".into());
        }
        let targets = crate::node_graph::scene_modifier_expand::impulse_recipients(
            owner,
            scene,
            selection,
            &PrimitiveRegistry::with_builtin(),
        )
        .map_err(|error| error.to_string())?;
        if targets.is_empty() {
            return Err("Impulse: selection has no physical recipients".into());
        }
        let mut recipients: Vec<Recipient> = Vec::with_capacity(targets.len());
        for (mut id, target) in targets {
            let mut instance = self.graph.instance_by_node_id(&id).ok_or_else(|| {
                format!("Impulse: recipient `{id}` is absent from the installed graph")
            })?;
            let node = self.graph.get_node(instance).expect("resolved recipient");
            let type_id = node.node.type_id().as_str();
            let expected = match target {
                ImpulseTarget::Fluid => is_liquid_domain(type_id),
                ImpulseTarget::Rigid(_) => type_id == "node.physics_world",
                ImpulseTarget::FluidAndRigid(_) => unreachable!("authoring resolves individual owners"),
            };
            if !expected {
                return Err(format!("Impulse: recipient `{id}` changed type"));
            }
            if let Some(pair) = self.graph.coupled_scenes().iter()
                .find(|pair| pair.rigid == instance)
            {
                instance = pair.fluid;
                id = self.graph.get_node(instance).expect("coupled owner exists").node_id.clone();
            }
            if let Some(existing) = recipients.iter_mut().find(|entry| entry.instance == instance) {
                existing.target = existing.target.union(target);
            } else {
                recipients.push(Recipient { id, instance, target });
            }
        }
        let source = self
            .graph
            .instance_by_node_id(field_node)
            .ok_or_else(|| format!("Impulse: field node `{field_node}` is missing"))?;
        let field = self
            .plan
            .steps()
            .iter()
            .find(|step| step.node == source)
            .and_then(|step| step.outputs.iter().find(|(port, _)| *port == field_port))
            .map(|(_, resource)| *resource)
            .filter(|&resource| self.plan.resource_type(resource) == Some(PortType::VectorField))
            .ok_or_else(|| "Impulse: source must be a compiled vector-field output".to_string())?;
        let mut ancestry = ahash::AHashSet::default();
        let mut pending = vec![source];
        while let Some(id) = pending.pop() {
            if !ancestry.insert(id) {
                continue;
            }
            let node = self.graph.get_node(id).expect("compiled ancestry node");
            let kind = node.node.type_id().as_str();
            let pure = node.node.is_pure()
                || matches!(
                    kind,
                    "system.generator_input"
                        | "node.value"
                        | "node.math"
                        | "node.affine_scalar"
                        | "node.lfo"
                        | "node.beat_ramp"
                );
            let requires = node.node.requires();
            if !pure || requires.gpu_encoder || requires.state_store {
                return Err(format!(
                    "Impulse: cannot capture field through stateful or GPU node `{kind}`"
                ));
            }
            pending.extend(self.graph.wires_into(id).map(|wire| wire.from.0));
        }
        let steps: Vec<_> = self
            .plan
            .steps()
            .iter()
            .map(|step| ancestry.contains(&step.node))
            .collect();
        let params = self
            .plan
            .steps()
            .iter()
            .zip(&steps)
            .map(|(step, &selected)| {
                selected.then(|| {
                    self.graph
                        .get_node(step.node)
                        .expect("compiled node")
                        .params
                        .clone()
                })
            })
            .collect();
        Ok(PreparedSceneImpulse {
            identity: self.impulse_identity.clone(),
            plan_epoch: self.last_forced_outputs_epoch,
            recipients: recipients.into(),
            field,
            steps,
            params,
        })
    }

    /// Call at the input producer boundary, after applying that observation's
    /// resolved controls. This samples current external values, not historical
    /// audio. `map_time` must map the source time into each recipient's native
    /// simulation clock; render arrival time is not a substitute. Both that
    /// mapping and the native epoch are frozen here, before later edits.
    pub fn capture_scene_impulse(
        &mut self,
        binding: &mut PreparedSceneImpulse,
        captured: &mut CapturedSceneImpulse,
        source: FrameTime,
        sequence: u64,
        mut map_time: impl FnMut(&NodeId, Seconds) -> Result<Seconds, String>,
    ) -> Result<(), String> {
        self.capture_scene_impulse_with_stamp(
            binding, captured, source, sequence,
            |id, node, transport, sequence| {
                let epoch = node.physics_impulse_epoch()
                    .ok_or_else(|| format!("Impulse: `{id}` is not initialized"))?;
                Ok(EventStamp { epoch, time: map_time(id, transport)?, sequence })
            },
        )
    }

    fn capture_scene_impulse_with_stamp(
        &mut self,
        binding: &mut PreparedSceneImpulse,
        captured: &mut CapturedSceneImpulse,
        source: FrameTime,
        sequence: u64,
        mut stamp: impl FnMut(&NodeId, &dyn crate::node_graph::EffectNode, Seconds, u64)
            -> Result<EventStamp, String>,
    ) -> Result<(), String> {
        self.validate_impulse_capture(binding, captured)?;
        if !source.seconds.0.is_finite() || !source.beats.0.is_finite() {
            return Err("Impulse: source clock must be finite".into());
        }
        captured.stamps.clear();
        for recipient in binding.recipients.iter() {
            let node = self.graph.get_node(recipient.instance)
                .expect("prepared recipient belongs to this graph");
            let stamp = stamp(&recipient.id, node.node.as_ref(), source.seconds, sequence)?;
            if !stamp.time.0.is_finite() || stamp.time.0 < 0.0 {
                return Err(
                    "Impulse: mapped simulation time must be finite and nonnegative".into(),
                );
            }
            captured.stamps.push(stamp);
        }
        // Prepared key/storage shapes are retained. Only values are copied.
        for (step, params) in self.plan.steps().iter().zip(&mut binding.params) {
            let Some(params) = params else { continue };
            let node = self
                .graph
                .get_node(step.node)
                .expect("compiled capture node");
            for (name, value) in params.iter_mut() {
                value.clone_from(node.params.get(name.as_ref()).expect("prepared parameter"));
            }
            if node.node.type_id().as_str() == "system.generator_input" {
                *params.get_mut("time").expect("generator time") =
                    ParamValue::Float(source.seconds.0 as f32);
                *params.get_mut("beat").expect("generator beat") =
                    ParamValue::Float(source.beats.0 as f32);
            }
        }
        self.executor.execute_physics_sample_frame(
            &mut self.graph,
            &self.plan,
            source,
            &binding.steps,
            &binding.params,
        );
        if self.executor.mesh_pending_of(binding.field) {
            return Err("Impulse: field inputs are pending or invalid".into());
        }
        let backend = self.executor.backend();
        let field = backend
            .slot_for(binding.field)
            .and_then(|slot| backend.vector_field(slot))
            .ok_or_else(|| "Impulse: field has no captured value".to_string())?;
        captured.field = Some(field);
        captured.source = Some(source);
        Ok(())
    }

    /// Admit a capture to its native queues. Successful recipients are marked
    /// immediately: retrying after a later recipient fails never duplicates a
    /// prefix. Inspect `scheduled_ticks` on error; admission is not a claim
    /// that every native step completed, or an atomic cross-world operation.
    pub fn deliver_scene_impulse(
        &mut self,
        captured: &mut CapturedSceneImpulse,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&captured.identity, &self.impulse_identity) {
            return Err("Impulse: captured graph was reset or rebuilt".into());
        }
        let field = captured
            .field
            .as_ref()
            .ok_or_else(|| "Impulse: no captured field".to_string())?;
        // Reject stale epochs before admitting any remaining recipient.
        for (recipient, stamp) in captured.recipients.iter().zip(&captured.stamps) {
            if self
                .graph
                .get_node(recipient.instance)
                .and_then(|node| node.node.physics_impulse_epoch())
                != Some(stamp.epoch)
            {
                return Err(format!(
                    "Impulse: `{}` was reset after capture",
                    recipient.id
                ));
            }
        }
        for ((recipient, &stamp), planned) in captured
            .recipients
            .iter()
            .zip(&captured.stamps)
            .zip(&mut captured.planned)
        {
            if planned.is_some() {
                continue;
            }
            let node = self
                .graph
                .get_node_mut(recipient.instance)
                .expect("validated recipient");
            *planned = Some(node.node.enqueue_physics_impulse(
                stamp,
                ResolvedNodeImpulse {
                    field: field.clone(),
                    target: recipient.target,
                },
            )?);
        }
        Ok(())
    }

    /// Drain native tick-start receipts for recording/diagnostics, preserving
    /// their payload, source stamp, applied tick and lateness.
    pub fn drain_scene_impulses(
        &mut self,
        mut consume: impl FnMut(&NodeId, AppliedEvent<ResolvedNodeImpulse>),
    ) {
        for node in self.graph.nodes_mut() {
            let id = &node.node_id;
            node.node
                .drain_physics_impulses(&mut |event| consume(id, event));
        }
    }

    /// Drain the stamps of impulses a held simulation discarded (pause,
    /// Speed 0); resume never replays them.
    pub fn drain_discarded_scene_impulses(&mut self, mut consume: impl FnMut(&NodeId, EventStamp)) {
        for node in self.graph.nodes_mut() {
            let id = &node.node_id;
            node.node.drain_discarded_impulses(&mut |stamp| consume(id, stamp));
        }
    }
}

#[cfg(test)]
#[path = "../testkit/impulses.rs"]
mod testkit;
