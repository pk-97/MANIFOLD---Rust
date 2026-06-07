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
        param_index: usize,
    },
    LayerOpacity {
        layer_id: LayerId,
    },
    LayerEffect {
        layer_id: LayerId,
        effect_type: PresetTypeId,
        param_index: usize,
    },
    GenParam {
        layer_id: LayerId,
        param_index: usize,
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

        // Master effects
        for fx in &project.settings.master_effects {
            let Some(def) = manifold_core::preset_definition_registry::try_get(fx.effect_type())
            else {
                continue;
            };
            for pi in 0..def.param_count {
                let Some(addr) = manifold_core::preset_definition_registry::get_osc_address(
                    fx.effect_type(),
                    pi,
                ) else {
                    continue;
                };
                let pd = &def.param_defs[pi];
                self.subscribe(
                    receiver,
                    &addr,
                    OscParamTarget::MasterEffect {
                        effect_type: fx.effect_type().clone(),
                        param_index: pi,
                    },
                    pd.min,
                    pd.max,
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
                let Some(def) =
                    manifold_core::preset_definition_registry::try_get(fx.effect_type())
                else {
                    continue;
                };
                for pi in 0..def.param_count {
                    let Some(addr) =
                        manifold_core::preset_definition_registry::get_osc_address_for_layer(
                            fx.effect_type(),
                            lid_str,
                            pi,
                        )
                    else {
                        continue;
                    };
                    let pd = &def.param_defs[pi];
                    self.subscribe(
                        receiver,
                        &addr,
                        OscParamTarget::LayerEffect {
                            layer_id: lid.clone(),
                            effect_type: fx.effect_type().clone(),
                            param_index: pi,
                        },
                        pd.min,
                        pd.max,
                    );
                }
            }
        }

        // Generator params
        if let Some(gp) = layer.gen_params() {
            let Some(def) =
                manifold_core::preset_definition_registry::try_get(gp.generator_type())
            else {
                return;
            };
            for pi in 0..def.param_count {
                let Some(addr) =
                    manifold_core::preset_definition_registry::get_osc_address_for_layer(
                        gp.generator_type(),
                        lid_str,
                        pi,
                    )
                else {
                    continue;
                };
                let pd = &def.param_defs[pi];
                self.subscribe(
                    receiver,
                    &addr,
                    OscParamTarget::GenParam {
                        layer_id: lid.clone(),
                        param_index: pi,
                    },
                    pd.min,
                    pd.max,
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
                    param_index,
                } => {
                    if let Some(fx) = project
                        .settings
                        .master_effects
                        .iter_mut()
                        .find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(*param_index, write.value);
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
                    param_index,
                } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(effects) = &mut layer.effects
                        && let Some(fx) =
                            effects.iter_mut().find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(*param_index, write.value);
                    }
                }
                OscParamTarget::GenParam {
                    layer_id,
                    param_index,
                } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(gp) = layer.gen_params_mut()
                    {
                        gp.set_base_param(*param_index, write.value);
                    }
                }
                OscParamTarget::Macro { index } => {
                    manifold_core::macro_bank::MacroBank::apply_macro(project, *index, write.value);
                }
            }
        }
    }
}
