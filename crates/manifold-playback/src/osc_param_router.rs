//! Data-driven OSC parameter router.
//!
//! Maps incoming OSC float messages to project parameter writes.
//! Intentional divergence from Unity's callback-based bridge classes
//! (MasterEffectOscBridge, LayerEffectOscBridge, GeneratorOscBridge,
//! LayerOscBridge) — Rust's ownership model doesn't support the same
//! closure-captures-mutable-ref pattern. This data-driven approach
//! achieves identical behavior: OSC float in → project param written.
//!
//! Usage (content thread):
//!   router.rebuild(&project, &mut osc_receiver);  // on project load / structural change
//!   osc_receiver.update();                         // drain UDP messages → fire callbacks
//!   router.apply(&mut project);                    // write pending values to project

use crate::osc_receiver::OscReceiver;
use manifold_core::PresetTypeId;
use manifold_core::LayerId;
use manifold_core::project::Project;
use parking_lot::Mutex;
use std::sync::Arc;

// ── Target descriptor ───────────────────────────────────────────

/// What an OSC address maps to in the project.
#[derive(Clone)]
enum OscParamTarget {
    MasterOpacity,
    MasterEffect {
        effect_type: PresetTypeId,
        param_id: String,
    },
    LayerOpacity {
        layer_id: LayerId,
    },
    LayerEffect {
        layer_id: LayerId,
        effect_type: PresetTypeId,
        param_id: String,
    },
    GenParam {
        layer_id: LayerId,
        param_id: String,
    },
    Macro {
        index: usize,
    },
}

/// A pending parameter write from an OSC message.
#[derive(Clone)]
struct PendingWrite {
    target: OscParamTarget,
    value: f32,
}

// ── Router ──────────────────────────────────────────────────────

/// Data-driven OSC → project parameter router.
///
/// Port of Unity's MasterEffectOscBridge + LayerOscBridge +
/// LayerEffectOscBridge + GeneratorOscBridge as a single unit.
pub struct OscParamRouter {
    /// Pending writes from OSC callbacks (filled during osc_receiver.update()).
    pending: Arc<Mutex<Vec<PendingWrite>>>,
    /// Addresses we're currently subscribed to (for cleanup on rebuild).
    registered_addresses: Vec<String>,
}

impl Default for OscParamRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl OscParamRouter {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(Vec::with_capacity(64))),
            registered_addresses: Vec::new(),
        }
    }

    /// Number of registered OSC parameter addresses.
    pub fn route_count(&self) -> usize {
        self.registered_addresses.len()
    }

    /// Rebuild all OSC parameter routes for the current project state.
    /// Unsubscribes old addresses, subscribes new ones on the receiver.
    /// Call on project load and after structural changes (layer/effect add/remove).
    pub fn rebuild(&mut self, project: &Project, receiver: &mut OscReceiver) {
        // Unsubscribe old
        for addr in &self.registered_addresses {
            receiver.unsubscribe_all(addr);
        }
        self.registered_addresses.clear();
        self.pending.lock().clear();

        // Macro sliders: /macro/1 through /macro/8
        for i in 0..manifold_core::macro_bank::MACRO_COUNT {
            let addr = format!("/macro/{}", i + 1);
            self.subscribe(
                receiver,
                &addr,
                OscParamTarget::Macro { index: i },
                0.0,
                1.0,
            );
        }

        // Master opacity
        self.subscribe(
            receiver,
            "/master/opacity",
            OscParamTarget::MasterOpacity,
            0.0,
            1.0,
        );

        // Master effects — iterate the LIVE manifest (P4): every param on the
        // instance is addressable, including user-added / registry-absent ones.
        for fx in &project.settings.master_effects {
            for p in fx.params.iter() {
                let Some(addr) = manifold_core::preset_definition_registry::get_osc_address_by_id(
                    fx.effect_type(),
                    p.id(),
                ) else {
                    continue;
                };
                self.subscribe(
                    receiver,
                    &addr,
                    OscParamTarget::MasterEffect {
                        effect_type: fx.effect_type().clone(),
                        param_id: p.id().to_string(),
                    },
                    p.spec.min,
                    p.spec.max,
                );
            }
        }

        // Per-layer
        for layer in &project.timeline.layers {
            self.register_layer(layer, receiver);
        }
    }

    /// Register all OSC addresses for a single layer (opacity, effects, generator).
    fn register_layer(&mut self, layer: &manifold_core::layer::Layer, receiver: &mut OscReceiver) {
        let lid = &layer.layer_id;
        let lid_str = lid.as_str();

        // Layer opacity: /layer/{layerId}/opacity
        let opacity_addr = format!("/layer/{}/opacity", lid_str);
        self.subscribe(
            receiver,
            &opacity_addr,
            OscParamTarget::LayerOpacity {
                layer_id: lid.clone(),
            },
            0.0,
            1.0,
        );

        // Layer effects
        if let Some(effects) = &layer.effects {
            for fx in effects {
                for p in fx.params.iter() {
                    let Some(addr) =
                        manifold_core::preset_definition_registry::get_osc_address_for_layer_by_id(
                            fx.effect_type(),
                            lid_str,
                            p.id(),
                        )
                    else {
                        continue;
                    };
                    self.subscribe(
                        receiver,
                        &addr,
                        OscParamTarget::LayerEffect {
                            layer_id: lid.clone(),
                            effect_type: fx.effect_type().clone(),
                            param_id: p.id().to_string(),
                        },
                        p.spec.min,
                        p.spec.max,
                    );
                }
            }
        }

        // Generator params — live manifest (P4).
        if let Some(gp) = layer.gen_params() {
            for p in gp.params.iter() {
                let Some(addr) =
                    manifold_core::preset_definition_registry::get_osc_address_for_layer_by_id(
                        gp.generator_type(),
                        lid_str,
                        p.id(),
                    )
                else {
                    continue;
                };
                self.subscribe(
                    receiver,
                    &addr,
                    OscParamTarget::GenParam {
                        layer_id: lid.clone(),
                        param_id: p.id().to_string(),
                    },
                    p.spec.min,
                    p.spec.max,
                );
            }
        }
    }

    /// Subscribe a single address on the receiver. The callback maps the
    /// incoming 0–1 float to the param's native range and pushes a
    /// PendingWrite for apply() to drain.
    fn subscribe(
        &mut self,
        receiver: &mut OscReceiver,
        address: &str,
        target: OscParamTarget,
        min: f32,
        max: f32,
    ) {
        let pending = self.pending.clone();
        let target_clone = target.clone();
        let is_direct = (min - 0.0).abs() < f32::EPSILON && (max - 1.0).abs() < f32::EPSILON;

        receiver.subscribe(
            address,
            Box::new(move |_addr, values| {
                if let Some(&v) = values.first() {
                    let mapped = if is_direct {
                        v
                    } else {
                        // Unity: Mathf.Lerp(min, max, v) — clamps t to 0-1
                        min + (max - min) * v.clamp(0.0, 1.0)
                    };
                    pending.lock().push(PendingWrite {
                        target: target_clone.clone(),
                        value: mapped,
                    });
                }
            }),
        );

        self.registered_addresses.push(address.to_string());
    }

    /// Apply all pending OSC parameter writes to the project.
    /// Call immediately after `osc_receiver.update()` in the content thread tick.
    pub fn apply(&self, project: &mut Project) {
        let mut pending = self.pending.lock();
        if pending.is_empty() {
            return;
        }

        for write in pending.drain(..) {
            match &write.target {
                OscParamTarget::MasterOpacity => {
                    project.settings.set_master_opacity(write.value);
                }
                OscParamTarget::MasterEffect {
                    effect_type,
                    param_id,
                } => {
                    if let Some(fx) = project
                        .settings
                        .master_effects
                        .iter_mut()
                        .find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(param_id, write.value);
                    }
                }
                OscParamTarget::LayerOpacity { layer_id } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                    {
                        layer.opacity = write.value.clamp(0.0, 1.0);
                    }
                }
                OscParamTarget::LayerEffect {
                    layer_id,
                    effect_type,
                    param_id,
                } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(effects) = &mut layer.effects
                        && let Some(fx) =
                            effects.iter_mut().find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(param_id, write.value);
                    }
                }
                OscParamTarget::GenParam {
                    layer_id,
                    param_id,
                } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(gp) = layer.gen_params_mut()
                    {
                        gp.set_base_param(param_id, write.value);
                    }
                }
                OscParamTarget::Macro { index } => {
                    manifold_core::macro_bank::MacroBank::apply_macro(project, *index, write.value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc_receiver::OscReceiver;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::PresetInstance;
    use manifold_core::params::{Param, ParamManifest};

    fn user_spec(id: &str) -> ParamSpecDef {
        ParamSpecDef {
            id: id.to_string(),
            name: id.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        }
    }

    /// An effect type that carries an OSC prefix in the live registry (so its
    /// params are OSC-addressable at all), plus that prefix. Discovered rather
    /// than hard-coded — the test shouldn't depend on which specific effect
    /// happens to declare a prefix.
    fn effect_with_osc_prefix() -> (PresetTypeId, String) {
        use manifold_core::preset_def::PresetKind;
        manifold_core::preset_definition_registry::all_of_kind(PresetKind::Effect)
            .into_iter()
            .find_map(|ty| {
                manifold_core::preset_definition_registry::try_get(&ty)
                    .and_then(|d| d.osc_prefix.clone())
                    .map(|p| (ty, p))
            })
            .expect("at least one registered effect must declare an osc_prefix")
    }

    fn project_with_master(fx: PresetInstance) -> Project {
        let mut project = Project::default();
        project.settings.master_effects.push(fx);
        project
    }

    /// REPRO — design acceptance (b): a user-added param on a master effect
    /// must get an OSC address. Before P4 the router enumerated the frozen
    /// registry (`0..def.param_count`), so a param absent from the registry got
    /// no address — unmappable over OSC. (Runnable-red against pre-P4 code.)
    #[test]
    fn osc_registers_address_for_user_added_master_param() {
        let (ty, prefix) = effect_with_osc_prefix();
        let mut fx = PresetInstance::new(ty);
        fx.params = ParamManifest::from_params(vec![Param::user_added(user_spec("user_glow"))]);
        let project = project_with_master(fx);

        let mut receiver = OscReceiver::new();
        let mut router = OscParamRouter::new();
        router.rebuild(&project, &mut receiver);

        let expected = format!("/master/{prefix}/user_glow");
        assert!(
            router.registered_addresses.contains(&expected),
            "user-added param must be OSC-addressable; registered = {:?}",
            router.registered_addresses
        );
    }

    /// GUARD — the live-rig address contract: bundled params must get a
    /// well-formed `/master/{prefix}/{id}` address and the router must
    /// register it. (Pre-P5 this proved byte-identity against the frozen
    /// positional `get_osc_address`; that oracle is gone under registry
    /// containment, so the well-formed address IS the expected value now.)
    #[test]
    fn osc_bundled_addresses_are_byte_identical_to_positional() {
        let (ty, prefix) = effect_with_osc_prefix();
        let defaults = manifold_core::preset_definition_registry::get_defaults(&ty);
        assert!(!defaults.is_empty(), "chosen effect must have bundled params");

        let mut fx = PresetInstance::new(ty.clone());
        fx.params = ParamManifest::from_params(defaults.clone());
        let project = project_with_master(fx);

        let mut receiver = OscReceiver::new();
        let mut router = OscParamRouter::new();
        router.rebuild(&project, &mut receiver);

        for p in defaults.iter() {
            let expected = format!("/master/{}/{}", prefix, p.id());
            let addr =
                manifold_core::preset_definition_registry::get_osc_address_by_id(&ty, p.id())
                    .expect("id-keyed address must exist");
            assert_eq!(
                addr, expected,
                "address for bundled param (id {}) must be well-formed",
                p.id()
            );
            assert!(
                router.registered_addresses.contains(&addr),
                "router must register the bundled address {addr}"
            );
        }
    }

    /// Dispatch resolves and writes a user-added param by id (design acceptance
    /// (b), the "dispatches" half). Same-module, so it feeds the router's
    /// pending queue directly instead of standing up a UDP socket.
    #[test]
    fn osc_dispatch_writes_user_param_by_id() {
        let (ty, _) = effect_with_osc_prefix();
        let mut fx = PresetInstance::new(ty.clone());
        fx.params = ParamManifest::from_params(vec![Param::user_added(user_spec("user_glow"))]);
        let mut project = project_with_master(fx);

        let router = OscParamRouter::new();
        router.pending.lock().push(PendingWrite {
            target: OscParamTarget::MasterEffect {
                effect_type: ty,
                param_id: "user_glow".to_string(),
            },
            value: 0.42,
        });
        router.apply(&mut project);

        let v = project.settings.master_effects[0]
            .params
            .get("user_glow")
            .unwrap()
            .value;
        assert!((v - 0.42).abs() < 1e-6, "dispatch must write user param by id; got {v}");
    }
}
