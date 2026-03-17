use std::collections::HashMap;
use std::sync::LazyLock;

use crate::types::GeneratorType;
use crate::effects::ParamDef;

// ─── Generator Definition ───

#[derive(Debug, Clone)]
pub struct GeneratorDef {
    pub display_name: &'static str,
    pub is_line_based: bool,
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    pub osc_prefix: Option<&'static str>,
}

// ─── Static Registry ───

static DEFINITIONS: LazyLock<HashMap<GeneratorType, GeneratorDef>> = LazyLock::new(build_definitions);

static MAX_PARAM_COUNT: LazyLock<usize> = LazyLock::new(|| {
    DEFINITIONS.values().map(|d| d.param_count).max().unwrap_or(0)
});

// ─── Public API ───

pub fn get(gen_type: GeneratorType) -> &'static GeneratorDef {
    DEFINITIONS.get(&gen_type).expect("Unknown GeneratorType")
}

pub fn try_get(gen_type: GeneratorType) -> Option<&'static GeneratorDef> {
    DEFINITIONS.get(&gen_type)
}

pub fn is_line_based(gen_type: GeneratorType) -> bool {
    DEFINITIONS.get(&gen_type).map_or(false, |d| d.is_line_based)
}

pub fn get_param_def(gen_type: GeneratorType, index: usize) -> ParamDef {
    let Some(def) = DEFINITIONS.get(&gen_type) else { return ParamDef::default() };
    if index >= def.param_count { return ParamDef::default() }
    def.param_defs[index].clone()
}

pub fn get_defaults(gen_type: GeneratorType) -> Vec<f32> {
    let Some(def) = DEFINITIONS.get(&gen_type) else { return Vec::new() };
    def.param_defs.iter().map(|p| p.default_value).collect()
}

pub fn format_gen_value(gen_type: GeneratorType, index: usize, value: f32) -> String {
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

pub fn get_osc_address(gen_type: GeneratorType, index: usize) -> Option<String> {
    let def = DEFINITIONS.get(&gen_type)?;
    if def.osc_prefix.is_none() { return None }
    if index >= def.param_count { return None }

    let suffix = def.param_defs[index].osc_suffix.as_ref()?;
    Some(format!("/{}/{}", def.osc_prefix.unwrap(), suffix))
}

pub fn get_osc_address_for_layer(
    gen_type: GeneratorType,
    layer_id: &str,
    index: usize,
) -> Option<String> {
    if layer_id.is_empty() { return None }
    let def = DEFINITIONS.get(&gen_type)?;
    if def.osc_prefix.is_none() { return None }
    if index >= def.param_count { return None }

    let suffix = def.param_defs[index].osc_suffix.as_ref()?;
    Some(format!("/layer/{}/gen/{}/{}", layer_id, def.osc_prefix.unwrap(), suffix))
}

pub fn try_get_gen_param_range(gen_type: GeneratorType, index: usize) -> Option<(f32, f32)> {
    let def = DEFINITIONS.get(&gen_type)?;
    if index >= def.param_count { return None }
    let pd = &def.param_defs[index];
    Some((pd.min, pd.max))
}

pub fn clamp_param(gen_type: GeneratorType, index: usize, value: f32) -> f32 {
    let Some(def) = DEFINITIONS.get(&gen_type) else { return value };
    if index >= def.param_count { return value }
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

// ─── ParamDef Helpers ───

fn pd(
    name: &str,
    min: f32,
    max: f32,
    default: f32,
    fmt: Option<&str>,
    osc: &str,
) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: false,
        value_labels: None,
        format_string: fmt.map(|s| s.to_string()),
        osc_suffix: Some(osc.to_string()),
    }
}

fn pd_toggle(
    name: &str,
    min: f32,
    max: f32,
    default: f32,
    osc: &str,
) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: true,
        value_labels: None,
        format_string: None,
        osc_suffix: Some(osc.to_string()),
    }
}

fn pd_whole(
    name: &str,
    min: f32,
    max: f32,
    default: f32,
    osc: &str,
) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: true,
        is_toggle: false,
        value_labels: None,
        format_string: None,
        osc_suffix: Some(osc.to_string()),
    }
}

fn pd_whole_labels(
    name: &str,
    min: f32,
    max: f32,
    default: f32,
    labels: &[&str],
    osc: &str,
) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: true,
        is_toggle: false,
        value_labels: Some(labels.iter().map(|s| s.to_string()).collect()),
        format_string: None,
        osc_suffix: Some(osc.to_string()),
    }
}

// ─── Registry Builder ───

fn build_definitions() -> HashMap<GeneratorType, GeneratorDef> {
    let mut m = HashMap::new();

    // ── None ──
    m.insert(GeneratorType::None, GeneratorDef {
        display_name: "None",
        is_line_based: false,
        param_count: 0,
        param_defs: Vec::new(),
        osc_prefix: None,
    });

    // ── Tesseract ──
    let params = vec![
        pd("XY", 0.0, 2.0, 0.6, Some("F2"), "rotXY"),
        pd("ZW", 0.0, 2.0, 0.4, Some("F2"), "rotZW"),
        pd("XW", 0.0, 2.0, 0.25, Some("F2"), "rotXW"),
        pd("Line", 0.0005, 0.03, 0.002, Some("F4"), "line"),
        pd("Dist", 1.0, 6.0, 3.0, Some("F1"), "dist"),
        pd_toggle("Verts", 0.0, 1.0, 1.0, "verts"),
        pd("VSize", 0.1, 4.0, 1.0, Some("F1"), "vsize"),
        pd_toggle("Anim", 0.0, 1.0, 0.0, "anim"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Window", 0.01, 1.0, 0.1, Some("F2"), "window"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(GeneratorType::Tesseract, create_def("Tesseract", true, "generator/tesseract", params));

    // ── Duocylinder ──
    let params = vec![
        pd("XY", 0.0, 2.0, 0.4, Some("F2"), "rotXY"),
        pd("ZW", 0.0, 2.0, 0.25, Some("F2"), "rotZW"),
        pd("XW", 0.0, 2.0, 0.15, Some("F2"), "rotXW"),
        pd("Line", 0.0005, 0.03, 0.0015, Some("F4"), "line"),
        pd("Dist", 1.0, 6.0, 3.0, Some("F1"), "dist"),
        pd_toggle("Verts", 0.0, 1.0, 1.0, "verts"),
        pd("VSize", 0.1, 4.0, 1.0, Some("F1"), "vsize"),
        pd_toggle("Anim", 0.0, 1.0, 0.0, "anim"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Window", 0.01, 1.0, 0.1, Some("F2"), "window"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(GeneratorType::Duocylinder, create_def("Duocylinder", true, "generator/duocylinder", params));

    // ── Lissajous ──
    let params = vec![
        pd("Freq X", 0.0, 2.0, 0.13, Some("F2"), "freqX"),
        pd("Freq Y", 0.0, 2.0, 0.09, Some("F2"), "freqY"),
        pd("Phase", 0.0, 2.0, 0.07, Some("F2"), "phase"),
        pd("Line", 0.0005, 0.03, 0.002, Some("F4"), "line"),
        pd_toggle("Verts", 0.0, 1.0, 0.0, "verts"),
        pd("VSize", 0.1, 4.0, 0.5, Some("F1"), "vsize"),
        pd_toggle("Anim", 0.0, 1.0, 1.0, "anim"),
        pd("Speed", 0.1, 5.0, 2.67, Some("F1"), "speed"),
        pd("Window", 0.01, 1.0, 0.74, Some("F2"), "window"),
        pd("Scale", 0.25, 3.0, 1.55, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 1.0, "snap"),
    ];
    m.insert(GeneratorType::Lissajous, create_def("Lissajous", true, "generator/lissajous", params));

    // ── WireframeZoo ──
    let params = vec![
        pd("XY", 0.0, 2.0, 0.5, Some("F2"), "rotXY"),
        pd("ZW", 0.0, 2.0, 0.3, Some("F2"), "rotZW"),
        pd("XW", 0.0, 2.0, 0.2, Some("F2"), "rotXW"),
        pd("Line", 0.0005, 0.03, 0.003, Some("F4"), "line"),
        pd_whole_labels("Shape", 0.0, 4.0, 0.0, &["Tetra", "Cube", "Octa", "Icosa", "Dodeca"], "shape"),
        pd_toggle("Verts", 0.0, 1.0, 1.0, "verts"),
        pd("VSize", 0.1, 4.0, 1.0, Some("F1"), "vsize"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(GeneratorType::WireframeZoo, create_def("Wireframe Zoo", true, "generator/wireframeZoo", params));

    // ── OscilloscopeXY ──
    let params = vec![
        pd("Line", 0.0005, 0.03, 0.002, Some("F4"), "line"),
        pd_toggle("Verts", 0.0, 1.0, 0.0, "verts"),
        pd("VSize", 0.1, 4.0, 0.5, Some("F1"), "vsize"),
        pd_toggle("Anim", 0.0, 1.0, 1.0, "anim"),
        pd("Speed", 0.1, 5.0, 1.63, Some("F1"), "speed"),
        pd("Window", 0.01, 1.0, 0.59, Some("F2"), "window"),
        pd("Wave", 0.1, 3.0, 0.3, Some("F1"), "wave"),
        pd("Scale", 0.25, 3.0, 1.75, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 1.0, "snap"),
    ];
    m.insert(GeneratorType::OscilloscopeXY, create_def("Oscilloscope XY", true, "generator/oscilloscopeXY", params));

    // ── BasicShapesSnap ──
    let params = vec![
        pd("Line", 0.0005, 0.03, 0.015, Some("F4"), "line"),
        pd_whole_labels("Shape", 0.0, 5.0, 0.0, &["Square", "Diamond", "Octagon", "Sq Wire", "Dia Wire", "Oct Wire"], "shape"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(GeneratorType::BasicShapesSnap, create_def("Basic Shapes Snap", false, "generator/basicShapesSnap", params));

    // ── ConcentricTunnel ──
    let params = vec![
        pd_whole_labels("Shape", 0.0, 5.0, 0.0, &["Circle", "Triangle", "Square", "Pentagon", "Hexagon", "Star"], "shape"),
        pd("Line", 0.0005, 0.03, 0.008, Some("F4"), "line"),
        pd_whole_labels("Rate", 0.0, 4.0, 2.0, &["1/4", "1/2", "1", "2", "4"], "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd_whole_labels("Snap Mode", 0.0, 2.0, 0.0, &["Shape", "Spawn", "Both"], "snapmode"),
    ];
    m.insert(GeneratorType::ConcentricTunnel, create_def("Concentric Tunnel", false, "generator/concentricTunnel", params));

    // ── Plasma ──
    let params = vec![
        pd_whole_labels("Pattern", 0.0, 4.0, 0.0, &["Classic", "Rings", "Diamond", "Warp", "Cells"], "pattern"),
        pd("Complexity", 0.0, 1.0, 0.5, Some("F2"), "complexity"),
        pd("Contrast", 0.0, 1.0, 0.63, Some("F2"), "contrast"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 1.0, "snap"),
    ];
    m.insert(GeneratorType::Plasma, create_def("Plasma", false, "generator/plasma", params));

    // ── FractalZoom ──
    let params = vec![
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(GeneratorType::FractalZoom, create_def("Fractal Zoom", false, "generator/fractalZoom", params));

    // ── ReactionDiffusion ──
    let params = vec![
        pd("Feed", 0.01, 0.08, 0.055, Some("F3"), "feed"),
        pd("Kill", 0.03, 0.07, 0.062, Some("F3"), "kill"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(GeneratorType::ReactionDiffusion, create_def("Reaction-Diffusion", false, "generator/reactionDiffusion", params));

    // ── Flowfield ──
    let params = vec![
        pd("Noise", 0.5, 10.0, 1.5, Some("F1"), "noise"),
        pd("Curl", 0.0, 2.0, 0.3, Some("F2"), "curl"),
        pd("Decay", 0.90, 0.999, 0.97, Some("F3"), "decay"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 1.0, "snap"),
    ];
    m.insert(GeneratorType::Flowfield, create_def("Flowfield", false, "generator/flowfield", params));

    // ── ParametricSurface ──
    let params = vec![
        pd_whole_labels("Shape", 0.0, 4.0, 0.0, &["Gyroid", "Schwarz P", "Schwarz D", "Torus Knot", "Klein"], "shape"),
        pd("Morph", 0.0, 1.0, 0.0, Some("F2"), "morph"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 1.0, "snap"),
    ];
    m.insert(GeneratorType::ParametricSurface, create_def("Parametric Surface", false, "generator/parametricSurface", params));

    // ── StrangeAttractor ──
    let params = vec![
        pd_whole_labels("Type", 0.0, 4.0, 0.0, &["Lorenz", "Rossler", "Aizawa", "Thomas", "Halvorsen"], "type"),
        pd("Trail", 0.90, 0.999, 0.98, Some("F3"), "trail"),
        pd("Bright", 0.5, 5.0, 2.0, Some("F1"), "bright"),
        pd("Chaos", 0.0, 1.0, 0.0, Some("F2"), "chaos"),
        pd("Size", 0.2, 5.0, 1.5, Some("F1"), "size"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
    ];
    m.insert(GeneratorType::StrangeAttractor, create_def("Strange Attractor", false, "generator/strangeAttractor", params));

    // ── FluidSimulation ──
    let params = vec![
        pd("Flow", -0.1, -0.001, -0.01, Some("F3"), "flow"),
        pd_whole("Feather", 4.0, 60.0, 20.0, "feather"),
        pd("Curl", 30.0, 90.0, 85.0, Some("F0"), "curl"),
        pd("Turbulence", 0.0, 0.01, 0.001, Some("F4"), "turbulence"),
        pd("Speed", 0.1, 3.0, 1.0, Some("F1"), "speed"),
        pd("Contrast", 1.0, 8.0, 3.5, Some("F1"), "contrast"),
        pd_toggle("Invert", 0.0, 1.0, 0.0, "invert"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd("Particles (M)", 0.1, 8.0, 2.0, Some("F1"), "particles"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd_whole_labels("Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),
        pd("Particle Size", 1.0, 8.0, 3.0, Some("F1"), "particleSize"),
        pd("Field Res", 0.125, 1.0, 0.5, Some("F2"), "fieldRes"),
        pd("Anti-Clump", 0.0, 60.0, 20.0, Some("F0"), "antiClump"),
        pd("Wander", 0.0, 0.05, 0.01, Some("F3"), "wander"),
        pd("Respawn", 0.0, 0.01, 0.001, Some("F4"), "respawn"),
        pd("Dense Respawn", 0.0, 0.2, 0.05, Some("F3"), "denseRespawn"),
        pd_whole_labels("Color", 0.0, 5.0, 0.0, &["Mono", "Blush", "Sunset", "Ocean", "Vivid", "White"], "color"),
        pd("Color Bright", 0.5, 5.0, 2.0, Some("F1"), "colorBright"),
        pd("Zone Force", 0.0, 0.02, 0.005, Some("F3"), "zoneForce"),
    ];
    m.insert(GeneratorType::FluidSimulation, create_def("Fluid Simulation", false, "generator/fluidSimulation", params));

    // ── NumberStation ──
    let params = vec![
        pd_whole_labels("Mode", 0.0, 3.0, 0.0, &["Hex", "Binary", "Coords", "Mixed"], "mode"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Density", 0.2, 1.0, 0.6, Some("F2"), "density"),
        pd("Font", 0.5, 3.0, 1.0, Some("F1"), "fontSize"),
        pd("Glow", 0.0, 1.0, 0.3, Some("F2"), "glow"),
        pd("Flicker", 0.0, 1.0, 0.2, Some("F2"), "flicker"),
        pd_whole_labels("Color", 0.0, 3.0, 0.0, &["Green", "Amber", "White", "Cyan"], "color"),
        pd_whole("Columns", 4.0, 32.0, 16.0, "columns"),
    ];
    m.insert(GeneratorType::NumberStation, create_def("Number Station", false, "generator/numberStation", params));

    // ── Mycelium ──
    let params = vec![
        pd("SensDist", 0.005, 0.1, 0.02, Some("F3"), "sensdist"),
        pd("SensAngle", 0.1, 1.5, 0.8, Some("F2"), "sensangle"),
        pd("Turn", 0.05, 1.5, 0.4, Some("F2"), "turn"),
        pd("Step", 0.0002, 0.005, 0.001, Some("F4"), "step"),
        pd("Deposit", 0.1, 5.0, 1.5, Some("F1"), "deposit"),
        pd("Decay", 0.85, 1.0, 0.98, Some("F3"), "decay"),
        pd("Color", 0.0, 1.0, 0.08, Some("F2"), "color"),
        pd("Glow", 0.0, 3.0, 1.0, Some("F1"), "glow"),
        pd("Reactivity", 0.0, 1.0, 0.5, Some("F2"), "reactivity"),
        pd_whole("Agents", 10.0, 500.0, 200.0, "agents"),
        pd("Scale", 0.1, 2.0, 1.0, Some("F2"), "scale"),
        pd_whole("Seeds", 1.0, 5.0, 1.0, "seeds"),
    ];
    m.insert(GeneratorType::Mycelium, create_def("Mycelium", false, "generator/mycelium", params));

    // ── ComputeStrangeAttractor ──
    let params = vec![
        pd_whole_labels("Type", 0.0, 4.0, 0.0, &["Lorenz", "Rossler", "Aizawa", "Thomas", "Halvorsen"], "type"),
        pd("Contrast", 1.0, 8.0, 3.5, Some("F1"), "contrast"),
        pd("Chaos", 0.0, 1.0, 0.0, Some("F2"), "chaos"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd("Particles (M)", 0.1, 2.0, 0.5, Some("F1"), "particles"),
        pd("Diffusion", 0.0, 0.05, 0.0, Some("F3"), "diffusion"),
        pd("Tilt", -1.0, 1.0, 0.3, Some("F2"), "tilt"),
        pd("Splat Size", 1.0, 8.0, 3.0, Some("F1"), "splatSize"),
        pd_toggle("Invert", 0.0, 1.0, 0.0, "invert"),
    ];
    m.insert(GeneratorType::ComputeStrangeAttractor, create_def("Strange Attractor (GPU)", false, "generator/computeStrangeAttractor", params));

    // ── FluidSimulation3D ──
    let params = vec![
        // Shared with 2D FluidSimulation (indices 0-19)
        pd("Flow", -0.1, -0.001, -0.01, Some("F3"), "flow"),
        pd_whole("Feather", 4.0, 60.0, 20.0, "feather"),
        pd("Curl", 30.0, 90.0, 85.0, Some("F0"), "curl"),
        pd("Turbulence", 0.0, 0.01, 0.001, Some("F4"), "turbulence"),
        pd("Speed", 0.1, 3.0, 1.0, Some("F1"), "speed"),
        pd("Contrast", 1.0, 8.0, 3.5, Some("F1"), "contrast"),
        pd_toggle("Invert", 0.0, 1.0, 0.0, "invert"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd("Particles (M)", 0.1, 8.0, 2.0, Some("F1"), "particles"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd_whole_labels("Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),
        pd("Particle Size", 1.0, 8.0, 3.0, Some("F1"), "particleSize"),
        pd("Field Res", 0.125, 1.0, 0.5, Some("F2"), "fieldRes"),
        pd("Anti-Clump", 0.0, 60.0, 20.0, Some("F0"), "antiClump"),
        pd("Wander", 0.0, 0.05, 0.01, Some("F3"), "wander"),
        pd("Respawn", 0.0, 0.01, 0.001, Some("F4"), "respawn"),
        pd("Dense Respawn", 0.0, 0.2, 0.05, Some("F3"), "denseRespawn"),
        pd_whole_labels("Color", 0.0, 5.0, 0.0, &["Mono", "Blush", "Sunset", "Ocean", "Vivid", "White"], "color"),
        pd("Color Bright", 0.5, 5.0, 2.0, Some("F1"), "colorBright"),
        pd("Zone Force", 0.0, 0.02, 0.005, Some("F3"), "zoneForce"),
        // 3D-specific params (indices 20-25)
        pd_whole_labels("Container", 0.0, 3.0, 0.0, &["None", "Cube", "Sphere", "Torus"], "container"),
        pd("Ctr Scale", 0.2, 1.0, 0.8, Some("F2"), "containerScale"),
        pd_whole_labels("Vol Res", 0.0, 2.0, 0.0, &["64", "128", "256"], "volumeRes"),
        pd("Cam Dist", 1.0, 8.0, 3.0, Some("F1"), "camDist"),
        pd("Cam Tilt", -1.0, 1.0, 0.3, Some("F2"), "camTilt"),
        pd("Flatten", 0.0, 1.0, 0.0, Some("F2"), "flatten"),
    ];
    m.insert(GeneratorType::FluidSimulation3D, create_def("Fluid Simulation 3D", false, "generator/fluidSimulation3D", params));

    m
}

fn create_def(
    display_name: &'static str,
    is_line_based: bool,
    osc_prefix: &'static str,
    param_defs: Vec<ParamDef>,
) -> GeneratorDef {
    let param_count = param_defs.len();
    GeneratorDef {
        display_name,
        is_line_based,
        param_count,
        param_defs,
        osc_prefix: Some(osc_prefix),
    }
}
