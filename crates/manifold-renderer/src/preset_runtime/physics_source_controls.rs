//! Stable authored identity for per-instance physics source controls.
//!
//! The digest is deliberately limited to the parameter and control state that
//! can change the prepared physics source. Runtime modulation accumulators and
//! display-only metadata are excluded by the source model's serde contracts.

use std::io::{self, Write};

use manifold_core::ableton_mapping::AbletonParamMapping;
use manifold_core::audio_mod::ParameterAudioMod;
use manifold_core::effects::{AutomationLane, ParamEnvelope, ParameterDriver, PresetInstance};
use serde::Serialize;
use sha2::{Digest as ShaDigest, Sha256};

const DIGEST_VERSION: &[u8] = b"manifold.physics-source-controls\0v1";

#[cfg(test)]
#[path = "physics_source_controls_tests.rs"]
mod tests;

/// Hash the authored control identity for the requested, already canonical IDs.
///
/// The implementation writes every field directly into SHA-256. JSON rows use
/// the existing serde implementations and an unambiguous delimiter, so no
/// temporary JSON buffer is needed on the successful path.
pub(super) fn digest(ids: &[String], instance: &PresetInstance) -> Result<[u8; 32], String> {
    let mut writer = DigestWriter::default();
    writer.bytes(DIGEST_VERSION)?;
    writer.u32(ids.len())?;

    reject_unresolved_legacy(instance)?;

    for id in ids {
        writer.bytes(id.as_bytes())?;
        let param = instance.params.get(id);
        writer.bool(param.is_some());
        if let Some(param) = param {
            validate_param(param)?;
            writer.bytes(b"param")?;
            writer.bytes(param.spec.id.as_bytes())?;
            writer.f32(param.spec.min);
            writer.f32(param.spec.max);
            writer.f32(param.spec.default_value);
            hash_json(&mut writer, b"curve", &param.spec.curve)?;
            writer.bool(param.spec.invert);
            writer.bool(param.whole_numbers());
            writer.bool(param.spec.is_toggle);
            writer.bool(param.spec.is_trigger);
            writer.bool(param.spec.is_trigger_gate);
            writer.bool(param.wraps());

            let base = instance.get_base_param(id);
            if !base.is_finite() {
                return Err(format!(
                    "physics control '{id}' has a non-finite base value"
                ));
            }

            let has_enabled_automation = instance.automation_lanes.as_ref().is_some_and(|lanes| {
                lanes
                    .iter()
                    .any(|lane| lane.param_id.as_ref() == id && lane.enabled)
            });
            let has_ableton_mapping = instance.ableton_mappings.as_ref().is_some_and(|mappings| {
                mappings
                    .iter()
                    .any(|mapping| mapping.param_id.as_ref() == id)
            });
            let hash_base =
                !param.spec.is_trigger && !has_enabled_automation && !has_ableton_mapping;
            writer.bool(hash_base);
            if hash_base {
                writer.f32(base);
            }
        }

        hash_drivers(&mut writer, id, instance.drivers.as_deref())?;
        hash_envelopes(&mut writer, id, instance.envelopes.as_deref())?;
        hash_audio_mods(&mut writer, id, instance.audio_mods.as_deref())?;
        hash_automation(&mut writer, id, instance.automation_lanes.as_deref())?;
        hash_ableton(&mut writer, id, instance.ableton_mappings.as_deref())?;
    }

    Ok(writer.finish())
}

fn validate_param(param: &manifold_core::params::Param) -> Result<(), String> {
    for (name, value) in [
        ("min", param.spec.min),
        ("max", param.spec.max),
        ("default", param.spec.default_value),
    ] {
        if !value.is_finite() {
            return Err(format!(
                "physics control '{}' has a non-finite {name}",
                param.id()
            ));
        }
    }
    Ok(())
}

fn reject_unresolved_legacy(instance: &PresetInstance) -> Result<(), String> {
    if instance
        .drivers
        .as_deref()
        .is_some_and(|rows| rows.iter().any(|row| row.legacy_param_index.is_some()))
    {
        return Err("physics control has an unresolved legacy driver identity".into());
    }
    if instance
        .envelopes
        .as_deref()
        .is_some_and(|rows| rows.iter().any(|row| row.legacy_param_index.is_some()))
    {
        return Err("physics control has an unresolved legacy envelope identity".into());
    }
    if instance
        .ableton_mappings
        .as_deref()
        .is_some_and(|rows| rows.iter().any(|row| row.legacy_param_index.is_some()))
    {
        return Err("physics control has an unresolved legacy Ableton identity".into());
    }
    Ok(())
}

fn hash_drivers(
    writer: &mut DigestWriter,
    id: &str,
    rows: Option<&[ParameterDriver]>,
) -> Result<(), String> {
    let Some(rows) = rows else {
        writer.u32(0)?;
        return Ok(());
    };
    let count = rows
        .iter()
        .filter(|row| row.param_id.as_ref() == id)
        .count();
    writer.u32(count)?;
    for row in rows.iter().filter(|row| row.param_id.as_ref() == id) {
        hash_json(writer, b"driver", row)?;
    }
    Ok(())
}

fn hash_envelopes(
    writer: &mut DigestWriter,
    id: &str,
    rows: Option<&[ParamEnvelope]>,
) -> Result<(), String> {
    let Some(rows) = rows else {
        writer.u32(0)?;
        return Ok(());
    };
    let count = rows
        .iter()
        .filter(|row| row.param_id.as_ref() == id)
        .count();
    writer.u32(count)?;
    for row in rows.iter().filter(|row| row.param_id.as_ref() == id) {
        hash_json(writer, b"envelope", row)?;
    }
    Ok(())
}

fn hash_audio_mods(
    writer: &mut DigestWriter,
    id: &str,
    rows: Option<&[ParameterAudioMod]>,
) -> Result<(), String> {
    let Some(rows) = rows else {
        writer.u32(0)?;
        return Ok(());
    };
    let count = rows
        .iter()
        .filter(|row| row.param_id.as_ref() == id)
        .count();
    writer.u32(count)?;
    for row in rows.iter().filter(|row| row.param_id.as_ref() == id) {
        hash_json(writer, b"audio", row)?;
    }
    Ok(())
}

fn hash_automation(
    writer: &mut DigestWriter,
    id: &str,
    rows: Option<&[AutomationLane]>,
) -> Result<(), String> {
    let Some(rows) = rows else {
        writer.u32(0)?;
        return Ok(());
    };
    let count = rows
        .iter()
        .filter(|row| row.param_id.as_ref() == id)
        .count();
    writer.u32(count)?;
    for row in rows.iter().filter(|row| row.param_id.as_ref() == id) {
        hash_json(writer, b"automation", row)?;
    }
    Ok(())
}

fn hash_ableton(
    writer: &mut DigestWriter,
    id: &str,
    rows: Option<&[AbletonParamMapping]>,
) -> Result<(), String> {
    let Some(rows) = rows else {
        writer.u32(0)?;
        return Ok(());
    };
    let count = rows
        .iter()
        .filter(|row| row.param_id.as_ref() == id)
        .count();
    writer.u32(count)?;
    for row in rows.iter().filter(|row| row.param_id.as_ref() == id) {
        writer.bytes(b"ableton")?;
        writer.bytes(&row.address.track_id.to_be_bytes())?;
        writer.bytes(&row.address.device_id.to_be_bytes())?;
        writer.bytes(&row.address.param_id.to_be_bytes())?;
        writer.bytes(row.address.device_identity.device_class_name.as_bytes())?;
        validate_finite("Ableton rangeMin", row.range_min)?;
        validate_finite("Ableton rangeMax", row.range_max)?;
        writer.f32(row.range_min);
        writer.f32(row.range_max);
        writer.bool(row.inverted);
    }
    Ok(())
}

fn validate_finite(name: &str, value: f32) -> Result<(), String> {
    value
        .is_finite()
        .then_some(())
        .ok_or_else(|| format!("physics control has a non-finite {name}"))
}

fn hash_json<T: Serialize>(writer: &mut DigestWriter, tag: &[u8], value: &T) -> Result<(), String> {
    writer.bytes(tag)?;
    let mut serializer = serde_json::Serializer::new(&mut *writer);
    value
        .serialize(&mut serializer)
        .map_err(|error| format!("physics control encoding: {error}"))?;
    writer.delimiter();
    Ok(())
}

#[derive(Default)]
struct DigestWriter {
    hasher: Sha256,
}

impl DigestWriter {
    fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.u64(bytes.len() as u64)?;
        self.hasher.update(bytes);
        Ok(())
    }

    fn bool(&mut self, value: bool) {
        self.hasher.update([value as u8]);
    }

    fn f32(&mut self, value: f32) {
        self.hasher.update(value.to_bits().to_be_bytes());
    }

    fn u32(&mut self, value: usize) -> Result<(), String> {
        let value =
            u32::try_from(value).map_err(|_| "physics control count overflow".to_string())?;
        self.hasher.update(value.to_be_bytes());
        Ok(())
    }

    fn u64(&mut self, value: u64) -> Result<(), String> {
        self.hasher.update(value.to_be_bytes());
        Ok(())
    }

    fn delimiter(&mut self) {
        self.hasher.update([0xff]);
    }
}

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.hasher.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
