//! Shared test fixtures for project module tests (used by >=2 module test mods).

use super::*;

    /// Build a bundled test [`crate::params::Param`] (mirrors
    /// `effects::tests::slot`, kept local since that helper is private to
    /// `effects.rs`'s own test module).
    pub(super) fn slot(id: &str, value: f32, exposed: bool) -> crate::params::Param {
        let spec = crate::effect_graph_def::ParamSpecDef {
            id: id.to_string(),
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        };
        let mut p = crate::params::Param::bundled(spec);
        p.value = value;
        p.base = value;
        p.exposed = exposed;
        p
    }

    pub(super) fn graph_def_with_id(id: &str, name: &str) -> crate::effect_graph_def::EffectGraphDef {
        crate::effect_graph_def::EffectGraphDef {
            version: crate::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: Some(name.to_string()),
            description: None,
            preset_metadata: Some(crate::effect_graph_def::PresetMetadata {
                id: PresetTypeId::from_string(id.to_string()),
                display_name: name.to_string(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: Vec::new(),
                bindings: Vec::new(),
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    pub(super) fn send_with_id(label: &str, id: &str) -> crate::audio_setup::AudioSend {
        let mut s = crate::audio_setup::AudioSend::new(label);
        s.id = crate::AudioSendId::new(id);
        s
    }

    /// A layer with one clip-trigger config sourcing `send_id`, `enabled` as
    /// given. Test helper for the P2 layer-owned trigger tests below.
    pub(super) fn layer_with_clip_trigger(
        send_id: crate::AudioSendId,
        band: crate::audio_mod::AudioBand,
        enabled: bool,
    ) -> crate::layer::Layer {
        let mut layer = crate::layer::Layer::new("L".to_string(), crate::types::LayerType::Video, 0);
        let mut cfg = crate::audio_trigger::LayerClipTrigger::new(crate::audio_mod::AudioModSource {
            send_id,
            feature: crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Transients,
                band,
            ),
        });
        cfg.enabled = enabled;
        layer.clip_triggers.push(cfg);
        layer
    }
