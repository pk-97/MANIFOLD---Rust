use std::collections::HashMap;
use std::sync::LazyLock;
use crate::effect_type_id::EffectTypeId;
use crate::effects::{ParamDef, EffectInstance};

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

static DEFINITIONS: LazyLock<HashMap<EffectTypeId, EffectDef>> = LazyLock::new(build_definitions);

// ─── ParamDef Helpers ───

/// Basic continuous parameter (no labels, no osc suffix).
fn pd(name: &str, min: f32, max: f32, default: f32) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: false,
        value_labels: None,
        format_string: None,
        osc_suffix: None,
    }
}

/// Continuous parameter with osc suffix.
fn pd_osc(name: &str, min: f32, max: f32, default: f32, osc_suffix: &str) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: false,
        value_labels: None,
        format_string: None,
        osc_suffix: Some(osc_suffix.to_string()),
    }
}

/// Whole-number parameter with osc suffix (no labels).
fn pd_whole(name: &str, min: f32, max: f32, default: f32, osc_suffix: &str) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: true,
        is_toggle: false,
        value_labels: None,
        format_string: None,
        osc_suffix: Some(osc_suffix.to_string()),
    }
}

/// Whole-number parameter with value labels and osc suffix.
fn pd_whole_labels(name: &str, min: f32, max: f32, default: f32, labels: &[&str], osc_suffix: &str) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: true,
        is_toggle: false,
        value_labels: Some(labels.iter().map(|s| s.to_string()).collect()),
        format_string: None,
        osc_suffix: Some(osc_suffix.to_string()),
    }
}

/// Toggle parameter with osc suffix (isToggle = true).
fn pd_toggle(name: &str, min: f32, max: f32, default: f32, osc_suffix: &str) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: true,
        value_labels: None,
        format_string: None,
        osc_suffix: Some(osc_suffix.to_string()),
    }
}

// ─── Public API ───

/// Get the definition for an effect type. Panics if not found.
/// Matches Unity's `EffectDefinitionRegistry.Get(EffectType)`.
pub fn get(effect_type: &EffectTypeId) -> &'static EffectDef {
    DEFINITIONS.get(effect_type)
        .unwrap_or_else(|| panic!("EffectDefinitionRegistry: unknown EffectTypeId '{}'", effect_type))
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
pub fn get_osc_address_for_layer(effect_type: &EffectTypeId, layer_id: &str, param_index: usize) -> Option<String> {
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
    let mut m = HashMap::new();

    // Transform
    m.insert(EffectTypeId::TRANSFORM, EffectDef {
        display_name: "Transform",
        param_count: 4,
        param_defs: vec![
            pd("X", -1.0, 1.0, 0.0),
            pd("Y", -1.0, 1.0, 0.0),
            pd("Zoom", 0.1, 5.0, 1.0),
            pd("Rot", -180.0, 180.0, 0.0),
        ],
        osc_prefix: Some("transform"),
    });

    // InvertColors
    m.insert(EffectTypeId::INVERT_COLORS, EffectDef {
        display_name: "Invert Colors",
        param_count: 1,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 1.0),
        ],
        osc_prefix: Some("invert"),
    });

    // Feedback
    m.insert(EffectTypeId::FEEDBACK, EffectDef {
        display_name: "Feedback",
        param_count: 1,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
        ],
        osc_prefix: Some("feedback"),
    });

    // PixelSort
    m.insert(EffectTypeId::PIXEL_SORT, EffectDef {
        display_name: "Pixel Sort",
        param_count: 1,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
        ],
        osc_prefix: Some("pixelSort"),
    });

    // Bloom
    m.insert(EffectTypeId::BLOOM, EffectDef {
        display_name: "Bloom",
        param_count: 1,
        param_defs: vec![
            pd("Amount", 0.0, 5.0, 0.187),
        ],
        osc_prefix: Some("bloom"),
    });

    // InfiniteZoom
    m.insert(EffectTypeId::INFINITE_ZOOM, EffectDef {
        display_name: "Infinite Zoom",
        param_count: 2,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Sharp", 0.0, 1.0, 0.5, "Sharpness"),
        ],
        osc_prefix: Some("infiniteZoom"),
    });

    // Kaleidoscope
    m.insert(EffectTypeId::KALEIDOSCOPE, EffectDef {
        display_name: "Kaleidoscope",
        param_count: 2,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole("Segs", 2.0, 16.0, 6.0, "Segments"),
        ],
        osc_prefix: Some("kaleidoscope"),
    });

    // EdgeStretch
    m.insert(EffectTypeId::EDGE_STRETCH, EffectDef {
        display_name: "Edge Stretch",
        param_count: 3,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 1.0),
            pd_osc("Width", 0.1, 0.9, 0.433, "SourceWidth"),
            pd_whole_labels("Dir", 0.0, 2.0, 0.0, &["Horiz", "Vert", "Both"], "Direction"),
        ],
        osc_prefix: Some("edgeStretch"),
    });

    // VoronoiPrism
    m.insert(EffectTypeId::VORONOI_PRISM, EffectDef {
        display_name: "Voronoi Prism",
        param_count: 2,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole("Cells", 4.0, 64.0, 16.0, "CellCount"),
        ],
        osc_prefix: Some("voronoiPrism"),
    });

    // QuadMirror
    m.insert(EffectTypeId::QUAD_MIRROR, EffectDef {
        display_name: "Quad Mirror",
        param_count: 1,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 1.0),
        ],
        osc_prefix: Some("quadMirror"),
    });

    // Dither
    m.insert(EffectTypeId::DITHER, EffectDef {
        display_name: "Dither",
        param_count: 2,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole_labels("Algo", 0.0, 5.0, 0.0,
                &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"],
                "Algorithm"),
        ],
        osc_prefix: Some("dither"),
    });

    // Strobe
    m.insert(EffectTypeId::STROBE, EffectDef {
        display_name: "Strobe",
        param_count: 3,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole_labels("Rate", 0.0, 8.0, 6.0,
                &["1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32"],
                "Rate"),
            pd_whole_labels("Mode", 0.0, 2.0, 0.0,
                &["Opacity", "White", "Gain"],
                "Mode"),
        ],
        osc_prefix: Some("strobe"),
    });

    // StylizedFeedback
    m.insert(EffectTypeId::STYLIZED_FEEDBACK, EffectDef {
        display_name: "Stylized Feedback",
        param_count: 4,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.5),
            pd_osc("Zoom", 0.9, 1.1, 0.95, "Zoom"),
            pd_osc("Rotate", -10.0, 10.0, 0.0, "Rotate"),
            pd_whole_labels("Mode", 0.0, 2.0, 0.0,
                &["Screen", "Add", "Max"],
                "Mode"),
        ],
        osc_prefix: Some("stylizedFeedback"),
    });

    // Mirror
    m.insert(EffectTypeId::MIRROR, EffectDef {
        display_name: "Mirror",
        param_count: 2,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 1.0),
            pd_whole_labels("Mode", 0.0, 2.0, 0.0,
                &["Horiz", "Vert", "Both"],
                "Mode"),
        ],
        osc_prefix: Some("mirror"),
    });

    // BlobTracking
    m.insert(EffectTypeId::BLOB_TRACKING, EffectDef {
        display_name: "Blob Tracking",
        param_count: 5,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Thresh", 0.05, 0.9, 0.65, "Threshold"),
            pd_osc("Sens", 0.2, 1.0, 0.85, "Sensitivity"),
            pd_osc("Smooth", 0.0, 1.0, 0.7, "Smoothing"),
            pd_osc("Connect", 0.0, 1.0, 0.35, "Connect"),
        ],
        osc_prefix: Some("blobTracking"),
    });

    // CRT
    m.insert(EffectTypeId::CRT, EffectDef {
        display_name: "CRT",
        param_count: 5,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 1.0),
            pd_osc("Scanlines", 0.0, 1.0, 0.397, "Scanlines"),
            pd_osc("Glow", 0.0, 1.0, 0.3, "Glow"),
            pd_osc("Curvature", 0.0, 1.0, 0.0, "Curvature"),
            pd_osc("Style", 0.0, 1.0, 0.5, "Style"),
        ],
        osc_prefix: Some("crt"),
    });

    // FluidDistortion
    m.insert(EffectTypeId::FLUID_DISTORTION, EffectDef {
        display_name: "Fluid Distortion",
        param_count: 4,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Viscosity", 0.0, 1.0, 0.5, "Viscosity"),
            pd_osc("Vorticity", 0.0, 1.0, 0.5, "Vorticity"),
            pd_osc("Inject", 0.0, 1.0, 0.5, "Inject"),
        ],
        osc_prefix: Some("fluidDistortion"),
    });

    // EdgeGlow
    m.insert(EffectTypeId::EDGE_GLOW, EffectDef {
        display_name: "Edge Glow",
        param_count: 4,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Thresh", 0.0, 1.0, 0.3, "Threshold"),
            pd_osc("Glow", 0.0, 1.0, 0.5, "Glow"),
            pd_whole_labels("Mode", 0.0, 2.0, 0.0,
                &["Sobel", "Laplacian", "Frei-Chen"],
                "Mode"),
        ],
        osc_prefix: Some("edgeGlow"),
    });

    // Datamosh
    m.insert(EffectTypeId::DATAMOSH, EffectDef {
        display_name: "Datamosh",
        param_count: 3,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Hold", 0.0, 1.0, 0.5, "Hold"),
            pd_osc("Blend", 0.0, 1.0, 0.5, "Blend"),
        ],
        osc_prefix: Some("datamosh"),
    });

    // SlitScan
    m.insert(EffectTypeId::SLIT_SCAN, EffectDef {
        display_name: "Slit Scan",
        param_count: 3,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Width", 0.01, 0.5, 0.1, "Width"),
            pd_whole_labels("Dir", 0.0, 1.0, 0.0,
                &["Horiz", "Vert"],
                "Direction"),
        ],
        osc_prefix: Some("slitScan"),
    });

    // ColorGrade
    m.insert(EffectTypeId::COLOR_GRADE, EffectDef {
        display_name: "Color Grade",
        param_count: 9,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Gain", 0.0, 2.0, 1.0, "Gain"),
            pd_osc("Sat", 0.0, 2.0, 1.0, "Saturation"),
            pd_osc("Hue", -180.0, 180.0, 0.0, "Hue"),
            pd_osc("Contrast", 0.0, 2.0, 1.0, "Contrast"),
            pd_osc("Colorize", 0.0, 1.0, 0.0, "Colorize"),
            pd_whole("TintHue", 0.0, 360.0, 0.0, "TintHue"),
            pd_osc("TintSat", 0.0, 2.0, 1.0, "TintSaturation"),
            pd_osc("Focus", 0.0, 1.0, 0.75, "ColorizeFocus"),
        ],
        osc_prefix: Some("colorGrade"),
    });

    // WireframeDepth — intentional divergence from Unity (D-22):
    // 12 params, removed Persist/Depth/Face, added EdgeFollow, renamed CVFlow→Flow.
    m.insert(EffectTypeId::WIREFRAME_DEPTH, EffectDef {
        display_name: "Wireframe Depth",
        param_count: 12,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 1.0),                                      // 0
            pd_whole("Density", 16.0, 280.0, 260.0, "Density"),                // 1
            pd_osc("Width", 0.4, 3.0, 1.335, "Width"),                         // 2
            pd_osc("ZScale", 0.0, 2.5, 1.35, "ZScale"),                        // 3
            pd_osc("Smooth", 0.0, 0.98, 0.90, "Smooth"),                       // 4
            pd_osc("Subject", 0.0, 1.0, 0.52, "SubjectIsolation"),             // 5
            pd_whole_labels("Blend", 0.0, 6.0, 6.0,
                &["Normal", "Add", "Multiply", "Screen", "Overlay", "Stencil", "Opaque"],
                "BlendMode"),                                                   // 6
            pd_osc("WireRes", 0.5, 1.0, 1.0, "WireRes"),                       // 7
            pd_whole_labels("MeshRate", 1.0, 4.0, 1.0,
                &["Every", "Half", "Third", "Quarter"],
                "MeshRate"),                                                    // 8
            pd_whole_labels("Flow", 0.0, 1.0, 1.0,
                &["Off", "On"],
                "NativeFlow"),                                                  // 9
            pd_whole_labels("Lock", 0.0, 1.0, 1.0,
                &["Off", "On"],
                "FlowLock"),                                                    // 10
            pd_osc("EdgeFollow", 0.0, 1.0, 0.5, "EdgeFollow"),                 // 11
        ],
        osc_prefix: Some("wireframeDepth"),
    });

    // ChromaticAberration
    m.insert(EffectTypeId::CHROMATIC_ABERRATION, EffectDef {
        display_name: "Chromatic Aberration",
        param_count: 5,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Offset", 0.0, 0.05, 0.01, "Offset"),
            pd_whole_labels("Mode", 0.0, 1.0, 0.0,
                &["Radial", "Linear"],
                "Mode"),
            pd_whole("Angle", 0.0, 360.0, 0.0, "Angle"),
            pd_osc("Falloff", 0.0, 1.0, 0.5, "Falloff"),
        ],
        osc_prefix: Some("chromAb"),
    });

    // GradientMap
    m.insert(EffectTypeId::GRADIENT_MAP, EffectDef {
        display_name: "Gradient Map",
        param_count: 7,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole("ShadowH", 0.0, 360.0, 240.0, "ShadowHue"),
            pd_osc("ShadowS", 0.0, 1.0, 0.8, "ShadowSat"),
            pd_whole("HighH", 0.0, 360.0, 30.0, "HighlightHue"),
            pd_osc("HighS", 0.0, 1.0, 0.8, "HighlightSat"),
            pd_whole("MidH", 0.0, 360.0, 160.0, "MidHue"),
            pd_osc("Contrast", 0.0, 2.0, 1.0, "Contrast"),
        ],
        osc_prefix: Some("gradientMap"),
    });

    // Glitch
    m.insert(EffectTypeId::GLITCH, EffectDef {
        display_name: "Glitch",
        param_count: 5,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole("Block", 4.0, 64.0, 16.0, "BlockSize"),
            pd_osc("RGB Shift", 0.0, 0.05, 0.01, "RGBShift"),
            pd_osc("Scanline", 0.0, 1.0, 0.3, "Scanline"),
            pd_osc("Speed", 0.1, 10.0, 2.0, "Speed"),
        ],
        osc_prefix: Some("glitch"),
    });

    // FilmGrain
    m.insert(EffectTypeId::FILM_GRAIN, EffectDef {
        display_name: "Film Grain",
        param_count: 4,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Size", 0.5, 4.0, 1.5, "Size"),
            pd_osc("LumaWt", 0.0, 1.0, 0.5, "LumaWeight"),
            pd_osc("Color", 0.0, 1.0, 0.0, "ColorGrain"),
        ],
        osc_prefix: Some("filmGrain"),
    });

    // Halation
    m.insert(EffectTypeId::HALATION, EffectDef {
        display_name: "Halation",
        param_count: 5,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Thresh", 0.0, 1.0, 0.5, "Threshold"),
            pd_osc("Spread", 0.0, 1.0, 0.5, "Spread"),
            pd_whole("Hue", 0.0, 360.0, 20.0, "Hue"),
            pd_osc("Sat", 0.0, 1.0, 0.6, "Saturation"),
        ],
        osc_prefix: Some("halation"),
    });


    // Corruption
    m.insert(EffectTypeId::CORRUPTION, EffectDef {
        display_name: "Corruption",
        param_count: 6,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole("Block", 4.0, 64.0, 16.0, "BlockSize"),
            pd_osc("Hold", 0.0, 1.0, 0.5, "HoldChance"),
            pd_osc("Color", 0.0, 1.0, 0.3, "ColorCorrupt"),
            pd_osc("Rate", 0.1, 10.0, 2.0, "RefreshRate"),
            pd_osc("Drift", 0.0, 1.0, 0.2, "Drift"),
        ],
        osc_prefix: Some("corruption"),
    });

    // Infrared
    m.insert(EffectTypeId::INFRARED, EffectDef {
        display_name: "Infrared",
        param_count: 6,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole_labels("Palette", 0.0, 9.0, 0.0,
                &["White Hot", "Black Hot", "Green NV", "Iron Bow", "Rainbow", "Lava", "Arctic", "Magenta", "Electric", "Toxic"],
                "Palette"),
            pd_osc("Contrast", 0.5, 3.0, 1.0, "Contrast"),
            pd_osc("Noise", 0.0, 1.0, 0.15, "Noise"),
            pd_osc("Scanline", 0.0, 1.0, 0.0, "Scanline"),
            pd_osc("HotSpot", 0.0, 1.0, 0.0, "HotSpot"),
        ],
        osc_prefix: Some("infrared"),
    });

    // Surveillance
    m.insert(EffectTypeId::SURVEILLANCE, EffectDef {
        display_name: "Surveillance",
        param_count: 7,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_osc("Edge", 0.0, 1.0, 0.3, "EdgeThreshold"),
            pd_whole("Boxes", 1.0, 8.0, 3.0, "Boxes"),
            pd_osc("Cross", 0.0, 1.0, 0.5, "CrosshairSize"),
            pd_osc("Scan", 0.0, 5.0, 1.0, "ScanSpeed"),
            pd_whole_labels("Overlay", 0.0, 2.0, 0.0,
                &["Brackets", "Crosshair", "Grid"],
                "Overlay"),
            pd_toggle("IDs", 0.0, 1.0, 1.0, "IDLabels"),
        ],
        osc_prefix: Some("surveillance"),
    });

    // Redaction
    m.insert(EffectTypeId::REDACTION, EffectDef {
        display_name: "Redaction",
        param_count: 7,
        param_defs: vec![
            pd("Amount", 0.0, 1.0, 0.0),
            pd_whole("Boxes", 1.0, 8.0, 3.0, "BoxCount"),
            pd_osc("Size", 0.05, 0.5, 0.15, "BoxSize"),
            pd_whole_labels("Style", 0.0, 2.0, 0.0,
                &["Solid", "Pixelate", "Bars"],
                "Style"),
            pd_whole("Pixel", 4.0, 32.0, 8.0, "PixelSize"),
            pd_osc("Speed", 0.0, 3.0, 0.5, "Speed"),
            pd_osc("Snap", 0.0, 1.0, 0.0, "Snap"),
        ],
        osc_prefix: Some("redaction"),
    });

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_effects_registered() {
        // Every variant in EffectType::ALL should have a registry entry
        for et in get_all_effect_types() {
            assert!(try_get(&et).is_some(), "Missing registry entry for {:?}", et);
        }
        // Transform is also in ALL implicitly (it's the default)
        assert!(try_get(&EffectTypeId::TRANSFORM).is_some());
    }

    #[test]
    fn test_param_counts_match() {
        for et in get_all_effect_types() {
            let def = get(&et);
            assert_eq!(
                def.param_count,
                def.param_defs.len(),
                "param_count mismatch for {:?}: declared {} but has {} defs",
                et, def.param_count, def.param_defs.len()
            );
        }
        let def = get(&EffectTypeId::TRANSFORM);
        assert_eq!(def.param_count, def.param_defs.len());
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
        // Dither Algo at 2.0 should be "Lines"
        let s = format_value(&EffectTypeId::DITHER, 1, 2.0);
        assert_eq!(s, "Lines");
    }

    #[test]
    fn test_format_value_whole() {
        // Kaleidoscope Segs at 6.7 should round to "7"
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
        // Transform param 0 has no osc_suffix on the param (index 0 uses prefix only)
        let addr = get_osc_address(&EffectTypeId::TRANSFORM, 0);
        assert_eq!(addr, Some("/master/transform".to_string()));
        // Transform params 1,2,3 have no osc_suffix → None
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
