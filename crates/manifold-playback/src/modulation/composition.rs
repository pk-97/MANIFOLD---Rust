//! Pure composition of already-prepared parameter control sources.
//!
//! This module deliberately does not advance audio/envelope event state. The
//! caller supplies the current or historically captured audio state, making a
//! composition repeatable for a sampled beat.

use manifold_core::audio_mod::ParameterAudioMod;
use manifold_core::effects::{ParamEnvelope, ParameterDriver};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::params::constrain_to_range;
use manifold_core::{Beats, Bpm, Seconds};

/// Sources for one parameter-manifest composition pass.
pub struct ControlSources<'a> {
    /// Whether audio and drivers are enabled for this instance. Envelope
    /// shadows still apply; continuous envelopes use `active_elapsed`.
    pub enabled: bool,
    pub drivers: &'a [ParameterDriver],
    pub envelopes: &'a [ParamEnvelope],
    pub audio_mods: &'a [ParameterAudioMod],
}

/// Timing inputs for pure driver and envelope evaluation.
#[derive(Clone, Copy, Debug)]
pub struct ControlSample {
    pub beat: Beats,
    pub time: Seconds,
    pub bpm: Bpm,
    pub fps: f32,
    /// `None` disables continuous-envelope evaluation. A negative value is an
    /// inactive clip sentinel; callers may still supply a non-negative value
    /// for an otherwise disabled effect to preserve the existing semantics.
    pub active_elapsed: Option<Beats>,
}

/// Audio runtime state needed by composition, captured without advancing it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioControlState {
    pub step_value: Option<f32>,
    pub held_output: Option<f32>,
    pub fire_count: u32,
}

impl AudioControlState {
    /// Read only the three audio runtime fields used by composition.
    pub fn current(m: &ParameterAudioMod) -> Self {
        Self {
            step_value: m.step_value,
            held_output: m.audio_held_output,
            fire_count: m.fire_count,
        }
    }
}

/// Compose prepared controls onto `params` in retained-hop precedence order.
///
/// The caller has already applied automation and reset/prepared `value` and
/// `base`; this function writes `value` only. Historical callers must retain
/// the configuration, bases and envelope shadows valid at the sampled time.
/// This function neither captures them nor invents a source clock.
/// `audio_state` is intentionally a side-effect-free
/// callback: composition can call it more than once for one audio mod (for
/// example, once for its step shadow and again for its held output), and a
/// historical caller can return captured state instead of the mod's current
/// runtime state.
pub fn compose_controls(
    params: &mut ParamManifest,
    sources: ControlSources<'_>,
    sample: ControlSample,
    audio_state: impl Fn(usize, &ParameterAudioMod) -> AudioControlState,
) -> bool {
    if sources.drivers.is_empty() && sources.envelopes.is_empty() && sources.audio_mods.is_empty() {
        return false;
    }
    let mut dirty = false;
    for param in params.iter_mut() {
        if let Some(value) = compose_param(param, param.value, &sources, sample, &audio_state) {
            param.value = value;
            dirty = true;
        }
    }
    dirty
}

fn targets(source: &str, param: &str) -> bool {
    source == param
}

/// [`compose_controls`] for one parameter, from `prepared`, without writing.
/// `None` when no source wrote it. A per-hop caller passes one hop's audio
/// state and gets the value the frame composition would give for that hop.
pub fn compose_param(
    param: &Param,
    prepared: f32,
    sources: &ControlSources<'_>,
    sample: ControlSample,
    audio_state: &impl Fn(usize, &ParameterAudioMod) -> AudioControlState,
) -> Option<f32> {
    let id = param.spec.id.as_str();
    let mut value = prepared;
    let mut written = false;

    // A stepped audio shadow replaces the prepared base before all downstream
    // sources, matching retained-hop modulation precedence.
    if sources.enabled {
        for (index, audio) in sources.audio_mods.iter().enumerate() {
            if audio.enabled
                && targets(audio.param_id.as_ref(), id)
                && let Some(step) = audio_state(index, audio).step_value
            {
                value = step;
                written = true;
            }
        }
    }

    // Envelope step/random shadows apply regardless of the instance-enabled
    // flag, preserving the retained pipeline's established precedence.
    for envelope in sources.envelopes.iter().filter(|envelope| envelope.enabled) {
        if matches!(envelope.action, manifold_core::audio_mod::TriggerAction::Continuous) {
            continue;
        }
        if targets(envelope.param_id.as_ref(), id)
            && let Some(step) = envelope.step_value
        {
            value = step;
            written = true;
        }
    }

    if sources.enabled {
        for driver in sources
            .drivers
            .iter()
            .filter(|driver| driver.enabled && !driver.is_paused_by_user)
        {
            if targets(driver.param_id.as_ref(), id) {
                let raw = driver_target_value(
                    driver,
                    sample.beat,
                    sample.time,
                    sample.bpm,
                    sample.fps,
                    param.spec.min,
                    param.spec.max,
                );
                value = constrain_to_range(raw, param.spec.min, param.spec.max, param.spec.wraps);
                written = true;
            }
        }

        // Continuous audio and trigger counters compose after drivers. Gates
        // only produce trigger side effects during the event phase and never
        // write a parameter value here.
        for (index, audio) in sources.audio_mods.iter().enumerate() {
            if !audio.enabled || !targets(audio.param_id.as_ref(), id) {
                continue;
            }
            let state = audio_state(index, audio);
            if param.spec.is_trigger && !param.spec.is_trigger_gate {
                value = param.base + state.fire_count as f32;
                written = true;
            } else if !param.spec.is_trigger_gate
                && matches!(audio.action, manifold_core::audio_mod::TriggerAction::Continuous)
                && let Some(output) = state.held_output
            {
                value = output;
                written = true;
            }
        }
    }

    // Continuous envelopes are the final additive stage and only evaluate for
    // an active, non-negative elapsed beat.
    if let Some(active_elapsed) = sample.active_elapsed
        && active_elapsed >= Beats::ZERO
    {
        for envelope in sources.envelopes.iter().filter(|envelope| {
            envelope.enabled
                && matches!(envelope.action, manifold_core::audio_mod::TriggerAction::Continuous)
        }) {
            if targets(envelope.param_id.as_ref(), id)
                && apply_envelope_offset(
                    &mut value,
                    param.spec.min,
                    param.spec.max,
                    envelope.target_normalized,
                    ParamEnvelope::decay_level(active_elapsed, envelope.decay_beats),
                )
            {
                written = true;
            }
        }
    }

    written.then_some(value)
}

/// Map a driver's normalized output onto a target parameter's value range.
pub(super) fn driver_target_value(
    driver: &ParameterDriver,
    current_beat: Beats,
    time: Seconds,
    bpm: Bpm,
    fps: f32,
    min: f32,
    max: f32,
) -> f32 {
    // `period_beats()` is the free period when the driver is in free mode, else
    // the sync division's period (dotted/triplet baked into the variant).
    let mut normalized = if driver.frame_aligned {
        driver.evaluate_frame_aligned(time, bpm, fps)
    } else {
        ParameterDriver::evaluate_with_period(
            current_beat,
            driver.period_beats(),
            driver.waveform,
            driver.phase,
        )
    };
    if driver.reversed {
        normalized = 1.0 - normalized;
    }
    // Apply trim: map [0,1] to [lo, hi] within param range.
    let lo = min + (max - min) * driver.trim_min;
    let hi = min + (max - min) * driver.trim_max;
    lo + (hi - lo) * normalized
}

/// Apply an envelope's additive decay offset to one parameter value.
pub(super) fn apply_envelope_offset(
    value: &mut f32,
    min: f32,
    max: f32,
    target_norm: f32,
    level: f32,
) -> bool {
    let current = *value;
    let target = min + (max - min) * target_norm.clamp(0.0, 1.0);
    let offset = (target - current) * level;
    let final_value = (current + offset).clamp(min, max);
    if (final_value - current).abs() > f32::EPSILON {
        *value = final_value;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_mod::{AudioFeature, TriggerAction};
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::ParamId;
    use manifold_core::id::AudioSendId;
    use manifold_core::params::Param;
    use manifold_core::types::{BeatDivision, DriverWaveform};

    fn param(id: &str, min: f32, max: f32, base: f32) -> Param {
        let spec = ParamSpecDef {
            id: id.to_owned(),
            name: id.to_owned(),
            min,
            max,
            default_value: base,
            ..ParamSpecDef::default()
        };
        let mut param = Param::bundled(spec);
        param.base = base;
        param.value = base;
        param
    }

    fn sample(active_elapsed: Option<Beats>) -> ControlSample {
        ControlSample {
            beat: Beats(0.0),
            time: Seconds(0.0),
            bpm: Bpm(120.0),
            fps: 60.0,
            active_elapsed,
        }
    }

    fn audio(id: &str) -> ParameterAudioMod {
        ParameterAudioMod::new(
            ParamId::from(id.to_owned()),
            AudioSendId::new("send"),
            AudioFeature::default(),
        )
    }

    #[test]
    fn precedence_is_step_envelope_driver_audio_then_continuous_envelope() {
        let mut params = ParamManifest::from_params(vec![param("value", 0.0, 1.0, 0.1)]);

        let mut stepped_audio = audio("value");
        stepped_audio.action = TriggerAction::Step {
            amount: 0.1,
            wrap: manifold_core::audio_mod::WrapMode::Clamp,
        };
        stepped_audio.step_value = Some(0.2);
        let mut continuous_audio = audio("value");
        continuous_audio.audio_held_output = Some(0.4);

        let mut stepped_envelope = ParamEnvelope::new("value");
        stepped_envelope.action = TriggerAction::Step {
            amount: 0.1,
            wrap: manifold_core::audio_mod::WrapMode::Clamp,
        };
        stepped_envelope.step_value = Some(0.3);
        let mut continuous_envelope = ParamEnvelope::new("value");
        continuous_envelope.target_normalized = 1.0;
        continuous_envelope.decay_beats = 1.0;

        let driver = ParameterDriver::new("value", BeatDivision::Whole, DriverWaveform::Sawtooth);
        let audio_mods = [stepped_audio, continuous_audio];
        let envelopes = [stepped_envelope, continuous_envelope];
        let drivers = [driver];
        // Check each layer of precedence, including shadows that would be
        // hidden by later writers if only the final result were asserted.
        for (audio_count, envelope_count, driver_count, expected) in [
            (1, 0, 0, 0.2),
            (1, 1, 0, 0.3),
            (1, 1, 1, 0.0),
            (2, 1, 1, 0.4),
            (2, 2, 1, 0.7),
        ] {
            params.get_mut("value").unwrap().value = 0.1;
            assert!(compose_controls(
                &mut params,
                ControlSources {
                    enabled: true,
                    drivers: &drivers[..driver_count],
                    envelopes: &envelopes[..envelope_count],
                    audio_mods: &audio_mods[..audio_count],
                },
                sample(Some(Beats(0.5))),
                |_, m| AudioControlState::current(m),
            ));
            assert!((params.get("value").unwrap().value - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn audio_order_is_last_writer_wins_and_trigger_uses_base_plus_count() {
        let mut params = ParamManifest::from_params(vec![param("value", 0.0, 1.0, 0.25)]);
        let mut first = audio("value");
        first.audio_held_output = Some(0.2);
        let mut second = audio("value");
        second.audio_held_output = Some(0.8);
        let audio_mods = [first, second];

        compose_controls(
            &mut params,
            ControlSources {
                enabled: true,
                drivers: &[],
                envelopes: &[],
                audio_mods: &audio_mods,
            },
            sample(None),
            |_, m| AudioControlState::current(m),
        );
        assert_eq!(params.get("value").unwrap().value, 0.8);

        let mut trigger_params = ParamManifest::from_params(vec![param("value", 0.0, 1.0, 0.25)]);
        trigger_params.get_mut("value").unwrap().spec.is_trigger = true;
        let trigger = audio("value");
        let driver = ParameterDriver::new("value", BeatDivision::Whole, DriverWaveform::Sawtooth);
        let fire_count = 4;
        compose_controls(
            &mut trigger_params,
            ControlSources {
                enabled: true,
                drivers: std::slice::from_ref(&driver),
                envelopes: &[],
                audio_mods: std::slice::from_ref(&trigger),
            },
            sample(None),
            |_, _| AudioControlState {
                step_value: None,
                held_output: None,
                fire_count,
            },
        );
        assert_eq!(trigger_params.get("value").unwrap().value, 4.25);
    }

    #[test]
    fn captured_audio_state_is_repeatable_and_does_not_mutate_runtime() {
        let mut mod_state = audio("value");
        mod_state.step_value = Some(0.1);
        mod_state.audio_held_output = Some(0.2);
        mod_state.fire_count = 9;
        let captured = AudioControlState {
            step_value: Some(0.6),
            held_output: Some(0.7),
            fire_count: 3,
        };
        let before = AudioControlState::current(&mod_state);
        let mods = [mod_state];
        let compose = |params: &mut ParamManifest| {
            compose_controls(
                params,
                ControlSources {
                    enabled: true,
                    drivers: &[],
                    envelopes: &[],
                    audio_mods: &mods,
                },
                sample(None),
                |_, _| captured,
            )
        };
        let mut first = ParamManifest::from_params(vec![param("value", 0.0, 1.0, 0.0)]);
        let mut second = ParamManifest::from_params(vec![param("value", 0.0, 1.0, 0.0)]);
        assert!(compose(&mut first));
        assert!(compose(&mut second));
        assert_eq!(first.get("value").unwrap().value, 0.7);
        assert_eq!(second.get("value").unwrap().value, 0.7);
        assert_eq!(AudioControlState::current(&mods[0]), before);
    }

    #[test]
    fn disabled_audio_and_missing_targets_are_skipped_but_gate_is_not_written() {
        let mut params = ParamManifest::from_params(vec![param("value", 0.0, 1.0, 0.4)]);
        let mut disabled = audio("value");
        disabled.enabled = false;
        let missing = audio("missing");
        let mut gate = audio("value");
        gate.audio_held_output = Some(0.9);
        gate.fire_count = 7;
        params.get_mut("value").unwrap().spec.is_trigger_gate = true;
        let mods = [disabled, missing, gate];
        assert!(!compose_controls(
            &mut params,
            ControlSources {
                enabled: true,
                drivers: &[],
                envelopes: &[],
                audio_mods: &mods,
            },
            sample(None),
            |_, m| AudioControlState::current(m),
        ));
        assert_eq!(params.get("value").unwrap().value, 0.4);
    }

    #[test]
    fn composition_does_not_touch_base_or_touched() {
        let mut params = ParamManifest::from_params(vec![param("value", 0.0, 1.0, 0.3)]);
        params.get_mut("value").unwrap().touched = true;
        let mut driver =
            ParameterDriver::new("value", BeatDivision::Whole, DriverWaveform::Sawtooth);
        driver.trim_min = 1.0;
        driver.trim_max = 1.0;
        let drivers = [driver];
        assert!(compose_controls(
            &mut params,
            ControlSources {
                enabled: true,
                drivers: &drivers,
                envelopes: &[],
                audio_mods: &[],
            },
            sample(None),
            |_, m| AudioControlState::current(m),
        ));
        let p = params.get("value").unwrap();
        assert_eq!(p.value, 1.0);
        assert_eq!(p.base, 0.3);
        assert!(p.touched);
    }
}
