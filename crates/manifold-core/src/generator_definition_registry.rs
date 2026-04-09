use std::collections::HashMap;
use std::sync::LazyLock;

use crate::effects::ParamDef;
use crate::generator_type_id::GeneratorTypeId;

// ─── Generator Definition ───

/// A string parameter definition for generators that accept text input.
#[derive(Debug, Clone)]
pub struct StringParamDef {
    /// Display name shown in inspector.
    pub name: &'static str,
    /// Key used in `TimelineClip.string_params` map.
    pub key: &'static str,
    /// Default value for new clips.
    pub default_value: &'static str,
}

#[derive(Debug, Clone)]
pub struct GeneratorDef {
    pub display_name: &'static str,
    pub is_line_based: bool,
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    pub string_param_defs: Vec<StringParamDef>,
    pub osc_prefix: Option<&'static str>,
}

// ─── Static Registry ───

static DEFINITIONS: LazyLock<HashMap<GeneratorTypeId, GeneratorDef>> = LazyLock::new(|| {
    let mut m = build_definitions();
    for meta in inventory::iter::<crate::generator_registration::GeneratorMetadata> {
        m.insert(meta.id.clone(), meta.to_generator_def());
    }
    m
});

static MAX_PARAM_COUNT: LazyLock<usize> = LazyLock::new(|| {
    DEFINITIONS
        .values()
        .map(|d| d.param_count)
        .max()
        .unwrap_or(0)
});

// ─── Public API ───

pub fn get(gen_type: &GeneratorTypeId) -> &'static GeneratorDef {
    DEFINITIONS.get(gen_type).unwrap_or_else(|| {
        panic!(
            "GeneratorDefinitionRegistry: unknown GeneratorTypeId '{}'",
            gen_type
        )
    })
}

pub fn try_get(gen_type: &GeneratorTypeId) -> Option<&'static GeneratorDef> {
    DEFINITIONS.get(gen_type)
}

pub fn is_line_based(gen_type: &GeneratorTypeId) -> bool {
    DEFINITIONS.get(gen_type).is_some_and(|d| d.is_line_based)
}

pub fn get_param_def(gen_type: &GeneratorTypeId, index: usize) -> ParamDef {
    let Some(def) = DEFINITIONS.get(gen_type) else {
        return ParamDef::default();
    };
    if index >= def.param_count {
        return ParamDef::default();
    }
    def.param_defs[index].clone()
}

pub fn get_defaults(gen_type: &GeneratorTypeId) -> Vec<f32> {
    let Some(def) = DEFINITIONS.get(gen_type) else {
        return Vec::new();
    };
    def.param_defs.iter().map(|p| p.default_value).collect()
}

pub fn format_gen_value(gen_type: &GeneratorTypeId, index: usize, value: f32) -> String {
    let pd = get_param_def(gen_type, index);

    // Labels take priority
    if let Some(ref labels) = pd.value_labels {
        let idx = (value.round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
        return labels[idx].clone();
    }

    // Whole numbers next
    if pd.whole_numbers {
        return format!("{}", value.round() as i32);
    }

    // Format string next
    if let Some(ref fmt) = pd.format_string {
        return format_float_with_format_string(value, fmt);
    }

    // Default: F2
    format!("{:.2}", value)
}

pub fn get_osc_address(gen_type: &GeneratorTypeId, index: usize) -> Option<String> {
    let def = DEFINITIONS.get(gen_type)?;
    let prefix = def.osc_prefix.as_ref()?;
    if index >= def.param_count {
        return None;
    }

    let suffix = def.param_defs[index].osc_suffix.as_ref()?;
    Some(format!("/{}/{}", prefix, suffix))
}

pub fn get_osc_address_for_layer(
    gen_type: &GeneratorTypeId,
    layer_id: &str,
    index: usize,
) -> Option<String> {
    if layer_id.is_empty() {
        return None;
    }
    let def = DEFINITIONS.get(gen_type)?;
    let prefix = def.osc_prefix.as_ref()?;
    if index >= def.param_count {
        return None;
    }

    let suffix = def.param_defs[index].osc_suffix.as_ref()?;
    Some(format!("/layer/{}/gen/{}/{}", layer_id, prefix, suffix))
}

pub fn try_get_gen_param_range(gen_type: &GeneratorTypeId, index: usize) -> Option<(f32, f32)> {
    let def = DEFINITIONS.get(gen_type)?;
    if index >= def.param_count {
        return None;
    }
    let pd = &def.param_defs[index];
    Some((pd.min, pd.max))
}

pub fn clamp_param(gen_type: &GeneratorTypeId, index: usize, value: f32) -> f32 {
    let Some(def) = DEFINITIONS.get(gen_type) else {
        return value;
    };
    if index >= def.param_count {
        return value;
    }
    let pd = &def.param_defs[index];
    value.clamp(pd.min, pd.max)
}

pub fn max_param_count() -> usize {
    *MAX_PARAM_COUNT
}

// ─── Format Helper ───

fn format_float_with_format_string(value: f32, fmt: &str) -> String {
    match fmt {
        "F0" => format!("{:.0}", value),
        "F1" => format!("{:.1}", value),
        "F2" => format!("{:.2}", value),
        "F3" => format!("{:.3}", value),
        "F4" => format!("{:.4}", value),
        _ => format!("{:.2}", value),
    }
}

// ─── Registry Builder ───

fn build_definitions() -> HashMap<GeneratorTypeId, GeneratorDef> {
    let mut m = HashMap::new();

    // ── None ──
    m.insert(
        GeneratorTypeId::NONE,
        GeneratorDef {
            display_name: "None",
            is_line_based: false,
            param_count: 0,
            param_defs: Vec::new(),
            string_param_defs: Vec::new(),
            osc_prefix: None,
        },
    );

    // All other generators are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/generators/*.rs).

    m
}
