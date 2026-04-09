use crate::effect_type_id::EffectTypeId;
use crate::effects::{EffectInstance, ParamDef};
use std::collections::HashMap;
use std::sync::LazyLock;

// ─── Effect Definition ───

/// Metadata for one effect type: display name, parameter schema, OSC prefix.
/// Mechanical translation of Unity's EffectDefinitionRegistry.EffectDef.
#[derive(Debug, Clone)]
pub struct EffectDef {
    pub display_name: &'static str,
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    pub osc_prefix: Option<&'static str>,
}

// ─── Static Registry ───

static DEFINITIONS: LazyLock<HashMap<EffectTypeId, EffectDef>> = LazyLock::new(|| {
    let mut m = build_definitions();
    for meta in inventory::iter::<crate::effect_registration::EffectMetadata> {
        m.insert(meta.id.clone(), meta.to_effect_def());
    }
    m
});

// ─── Public API ───

/// Get the definition for an effect type. Panics if not found.
/// Matches Unity's `EffectDefinitionRegistry.Get(EffectType)`.
pub fn get(effect_type: &EffectTypeId) -> &'static EffectDef {
    DEFINITIONS.get(effect_type).unwrap_or_else(|| {
        panic!(
            "EffectDefinitionRegistry: unknown EffectTypeId '{}'",
            effect_type
        )
    })
}

/// Try to get the definition for an effect type.
/// Matches Unity's `EffectDefinitionRegistry.TryGet(EffectType, out EffectDef)`.
pub fn try_get(effect_type: &EffectTypeId) -> Option<&'static EffectDef> {
    DEFINITIONS.get(effect_type)
}

/// Create a new EffectInstance with default parameter values from the registry.
/// Matches Unity's `EffectDefinitionRegistry.CreateDefault(EffectType)`.
pub fn create_default(effect_type: &EffectTypeId) -> EffectInstance {
    let def = get(effect_type);
    let mut inst = EffectInstance::new(effect_type.clone());
    for (i, pd) in def.param_defs.iter().enumerate() {
        inst.set_base_param(i, pd.default_value);
    }
    inst
}

/// Format a parameter value for display.
/// Named labels take priority, then wholeNumbers round, then F2.
/// Matches Unity's `EffectDefinitionRegistry.FormatValue(EffectType, int, float)`.
pub fn format_value(effect_type: &EffectTypeId, param_index: usize, value: f32) -> String {
    let def = match try_get(effect_type) {
        Some(d) if param_index < d.param_count => d,
        _ => return format!("{:.2}", value),
    };
    let pd = &def.param_defs[param_index];
    if let Some(ref labels) = pd.value_labels {
        let idx = (value.round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
        return labels[idx].clone();
    }
    if pd.whole_numbers {
        return format!("{}", value.round() as i32);
    }
    format!("{:.2}", value)
}

/// Get the OSC address for a master effect parameter.
/// Returns None if no address is defined.
/// Matches Unity's `EffectDefinitionRegistry.GetOscAddress(EffectType, int)`.
pub fn get_osc_address(effect_type: &EffectTypeId, param_index: usize) -> Option<String> {
    let def = try_get(effect_type)?;
    let prefix = def.osc_prefix?;
    if param_index >= def.param_count {
        return None;
    }
    if param_index == 0 {
        return Some(format!("/master/{}", prefix));
    }
    let suffix = def.param_defs[param_index].osc_suffix.as_ref()?;
    Some(format!("/master/{}{}", prefix, suffix))
}

/// Get the OSC address for a layer effect parameter scoped to a specific layer.
/// Format: /layer/{layerId}/effectName or /layer/{layerId}/effectName/paramName
/// Matches Unity's `EffectDefinitionRegistry.GetOscAddressForLayer(EffectType, string, int)`.
pub fn get_osc_address_for_layer(
    effect_type: &EffectTypeId,
    layer_id: &str,
    param_index: usize,
) -> Option<String> {
    if layer_id.is_empty() {
        return None;
    }
    let def = try_get(effect_type)?;
    let prefix = def.osc_prefix?;
    if param_index >= def.param_count {
        return None;
    }
    if param_index == 0 {
        return Some(format!("/layer/{}/{}", layer_id, prefix));
    }
    let suffix = def.param_defs[param_index].osc_suffix.as_ref()?;
    Some(format!("/layer/{}/{}{}", layer_id, prefix, suffix))
}

/// Get default parameter values for an effect type.
/// Matches Unity's EffectDefinitionRegistry usage for creating new instances.
pub fn get_defaults(effect_type: &EffectTypeId) -> Vec<f32> {
    let def = get(effect_type);
    def.param_defs.iter().map(|p| p.default_value).collect()
}

/// Get all registered effect types (unordered).
/// Matches Unity's `EffectDefinitionRegistry.GetAllEffectTypes(List<EffectType>)`.
pub fn get_all_effect_types() -> Vec<EffectTypeId> {
    DEFINITIONS.keys().cloned().collect()
}

/// Get all registered effect types sorted by display name.
/// Matches Unity's `EffectDefinitionRegistry.GetAllEffectTypesSorted()`.
pub fn get_all_effect_types_sorted() -> Vec<EffectTypeId> {
    let mut list: Vec<EffectTypeId> = DEFINITIONS.keys().cloned().collect();
    list.sort_by_key(|t| t.as_str().to_string());
    list
}

// ─── Build Definitions ───

fn build_definitions() -> HashMap<EffectTypeId, EffectDef> {
    // All effects are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/effects/*.rs).
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_registration::EffectMetadata;
    use crate::generator_registration::ParamSpec;

    // Test-only inventory submissions — manifold-renderer isn't linked in
    // manifold-core unit tests, so we register minimal test fixtures here.
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::TRANSFORM,
            display_name: "Transform",
            category: "Spatial",
            available: true,
            osc_prefix: "transform",
            legacy_discriminant: Some(0),
            params: &[
                ParamSpec::continuous("X", -1.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("Y", -1.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("Zoom", 0.1, 5.0, 1.0, "F2", ""),
                ParamSpec::continuous("Rot", -180.0, 180.0, 0.0, "F2", ""),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::BLOOM,
            display_name: "Bloom",
            category: "Post-Process",
            available: true,
            osc_prefix: "bloom",
            legacy_discriminant: Some(12),
            params: &[
                ParamSpec::continuous("Amount", 0.0, 5.0, 0.187, "F2", ""),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::DITHER,
            display_name: "Dither",
            category: "Post-Process",
            available: true,
            osc_prefix: "dither",
            legacy_discriminant: Some(18),
            params: &[
                ParamSpec::continuous("Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::whole_labels("Algo", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::KALEIDOSCOPE,
            display_name: "Kaleidoscope",
            category: "Post-Process",
            available: true,
            osc_prefix: "kaleidoscope",
            legacy_discriminant: Some(14),
            params: &[
                ParamSpec::continuous("Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::whole("Segs", 2.0, 16.0, 6.0, "Segments"),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::INFINITE_ZOOM,
            display_name: "Infinite Zoom",
            category: "Post-Process",
            available: false,
            osc_prefix: "infiniteZoom",
            legacy_discriminant: Some(13),
            params: &[
                ParamSpec::continuous("Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("Sharp", 0.0, 1.0, 0.5, "F2", "Sharpness"),
            ],
        }
    }

    #[test]
    fn test_param_counts_match() {
        // Check all registered effects have consistent param counts
        for (_, def) in DEFINITIONS.iter() {
            assert_eq!(
                def.param_count,
                def.param_defs.len(),
                "param_count mismatch for {}: declared {} but has {} defs",
                def.display_name,
                def.param_count,
                def.param_defs.len()
            );
        }
    }

    #[test]
    fn test_create_default_bloom() {
        let inst = create_default(&EffectTypeId::BLOOM);
        assert_eq!(*inst.effect_type(), EffectTypeId::BLOOM);
        assert!(inst.enabled);
        assert_eq!(inst.param_values.len(), 1);
        assert!((inst.param_values[0] - 0.187).abs() < 1e-6);
    }

    #[test]
    fn test_format_value_labels() {
        let s = format_value(&EffectTypeId::DITHER, 1, 2.0);
        assert_eq!(s, "Lines");
    }

    #[test]
    fn test_format_value_whole() {
        let s = format_value(&EffectTypeId::KALEIDOSCOPE, 1, 6.7);
        assert_eq!(s, "7");
    }

    #[test]
    fn test_format_value_continuous() {
        let s = format_value(&EffectTypeId::BLOOM, 0, 0.5);
        assert_eq!(s, "0.50");
    }

    #[test]
    fn test_osc_address_master() {
        let addr = get_osc_address(&EffectTypeId::BLOOM, 0);
        assert_eq!(addr, Some("/master/bloom".to_string()));
    }

    #[test]
    fn test_osc_address_master_param() {
        let addr = get_osc_address(&EffectTypeId::INFINITE_ZOOM, 1);
        assert_eq!(addr, Some("/master/infiniteZoomSharpness".to_string()));
    }

    #[test]
    fn test_osc_address_no_suffix() {
        let addr = get_osc_address(&EffectTypeId::TRANSFORM, 0);
        assert_eq!(addr, Some("/master/transform".to_string()));
        let addr = get_osc_address(&EffectTypeId::TRANSFORM, 1);
        assert_eq!(addr, None);
    }

    #[test]
    fn test_osc_address_layer() {
        let addr = get_osc_address_for_layer(&EffectTypeId::BLOOM, "layer_1", 0);
        assert_eq!(addr, Some("/layer/layer_1/bloom".to_string()));
    }

    #[test]
    fn test_sorted_types() {
        let sorted = get_all_effect_types_sorted();
        for i in 1..sorted.len() {
            assert!(sorted[i - 1].as_str() <= sorted[i].as_str());
        }
    }
}
