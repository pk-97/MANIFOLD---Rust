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

static DEFINITIONS: LazyLock<HashMap<GeneratorTypeId, GeneratorDef>> =
    LazyLock::new(build_definitions);

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

// ─── ParamDef Helpers ───

fn pd(name: &str, min: f32, max: f32, default: f32, fmt: Option<&str>, osc: &str) -> ParamDef {
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

fn pd_toggle(name: &str, min: f32, max: f32, default: f32, osc: &str) -> ParamDef {
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

fn pd_whole(name: &str, min: f32, max: f32, default: f32, osc: &str) -> ParamDef {
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
    m.insert(
        GeneratorTypeId::TESSERACT,
        create_def("Tesseract", true, "tesseract", params),
    );

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
    m.insert(
        GeneratorTypeId::DUOCYLINDER,
        create_def("Duocylinder", true, "duocylinder", params),
    );

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
    m.insert(
        GeneratorTypeId::LISSAJOUS,
        create_def("Lissajous", true, "lissajous", params),
    );

    // ── WireframeZoo ──
    let params = vec![
        pd("XY", 0.0, 2.0, 0.5, Some("F2"), "rotXY"),
        pd("ZW", 0.0, 2.0, 0.3, Some("F2"), "rotZW"),
        pd("XW", 0.0, 2.0, 0.2, Some("F2"), "rotXW"),
        pd("Line", 0.0005, 0.03, 0.003, Some("F4"), "line"),
        pd_whole_labels(
            "Shape",
            0.0,
            4.0,
            0.0,
            &["Tetra", "Cube", "Octa", "Icosa", "Dodeca"],
            "shape",
        ),
        pd_toggle("Verts", 0.0, 1.0, 1.0, "verts"),
        pd("VSize", 0.1, 4.0, 1.0, Some("F1"), "vsize"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(
        GeneratorTypeId::WIREFRAME_ZOO,
        create_def("Wireframe Zoo", true, "wireframeZoo", params),
    );

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
    m.insert(
        GeneratorTypeId::OSCILLOSCOPE_XY,
        create_def("Oscilloscope XY", true, "oscilloscopeXY", params),
    );

    // ── BasicShapesSnap ──
    let params = vec![
        pd("Line", 0.0005, 0.03, 0.015, Some("F4"), "line"),
        pd_whole_labels(
            "Shape",
            0.0,
            2.0,
            0.0,
            &["Square", "Diamond", "Octagon"],
            "shape",
        ),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_whole_labels(
            "Fill",
            0.0,
            2.0,
            1.0,
            &["Solid", "Mixed", "Wireframe"],
            "fill",
        ),
    ];
    m.insert(
        GeneratorTypeId::BASIC_SHAPES_SNAP,
        create_def("Basic Shapes Snap", false, "basicShapesSnap", params),
    );

    // ── ConcentricTunnel ──
    let params = vec![
        pd_whole_labels(
            "Shape",
            0.0,
            5.0,
            0.0,
            &[
                "Circle", "Triangle", "Square", "Pentagon", "Hexagon", "Star",
            ],
            "shape",
        ),
        pd("Line", 0.0005, 0.03, 0.008, Some("F4"), "line"),
        pd_whole_labels(
            "Rate",
            0.0,
            4.0,
            2.0,
            &["1/4", "1/2", "1", "2", "4"],
            "speed",
        ),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd_whole_labels(
            "Snap Mode",
            0.0,
            2.0,
            0.0,
            &["Shape", "Spawn", "Both"],
            "snapmode",
        ),
    ];
    m.insert(
        GeneratorTypeId::CONCENTRIC_TUNNEL,
        create_def("Concentric Tunnel", false, "concentricTunnel", params),
    );

    // ── Plasma ──
    let params = vec![
        pd_whole_labels(
            "Pattern",
            0.0,
            7.0,
            0.0,
            &[
                "Classic", "Rings", "Diamond", "Warp", "Cells", "Noise", "Fractal",
                "Lattice",
            ],
            "pattern",
        ),
        pd("Complexity", 0.0, 1.0, 0.5, Some("F2"), "complexity"),
        pd("Contrast", 0.0, 1.0, 0.63, Some("F2"), "contrast"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 1.0, "snap"),
    ];
    m.insert(
        GeneratorTypeId::PLASMA,
        create_def("Plasma", false, "plasma", params),
    );

    // ── ParametricSurface ──
    let params = vec![
        pd_whole_labels(
            "Shape",
            0.0,
            4.0,
            0.0,
            &["Gyroid", "Schwarz P", "Schwarz D", "Torus Knot", "Klein"],
            "shape",
        ),
        pd("Morph", 0.0, 1.0, 0.0, Some("F2"), "morph"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 1.0, "snap"),
    ];
    m.insert(
        GeneratorTypeId::PARAMETRIC_SURFACE,
        create_def("Parametric Surface", false, "parametricSurface", params),
    );

    // ── FluidSimulation ──
    let params = vec![
        pd("Flow", -0.1, -0.001, -0.01, Some("F3"), "flow"),
        pd_whole("Feather", 4.0, 60.0, 20.0, "feather"),
        pd("Curl", 30.0, 90.0, 85.0, Some("F0"), "curl"),
        pd("Turbulence", 0.0, 0.01, 0.001, Some("F4"), "turbulence"),
        pd("Speed", 0.1, 3.0, 1.0, Some("F1"), "speed"),
        pd("Contrast", 1.0, 8.0, 3.5, Some("F1"), "contrast"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd("Count (M)", 0.1, 8.0, 2.0, Some("F1"), "count"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd_whole_labels(
            "Snap Mode",
            0.0,
            4.0,
            0.0,
            &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"],
            "snapMode",
        ),
        pd("Size", 1.0, 8.0, 3.0, Some("F1"), "size"),
        pd("Anti-Clump", 0.0, 60.0, 20.0, Some("F0"), "antiClump"),
        pd("Force", 0.0, 0.1, 0.005, Some("F3"), "force"),
    ];
    m.insert(
        GeneratorTypeId::FLUID_SIMULATION,
        create_def("Fluid Simulation", false, "fluidSimulation", params),
    );

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
    m.insert(
        GeneratorTypeId::MYCELIUM,
        create_def("Mycelium", false, "mycelium", params),
    );

    // ── FluidSimulation3D ──
    let params = vec![
        // Shared with 2D FluidSimulation (indices 0-12)
        pd("Flow", -0.1, -0.001, -0.01, Some("F3"), "flow"),
        pd_whole("Feather", 4.0, 60.0, 20.0, "feather"),
        pd("Curl", 30.0, 90.0, 85.0, Some("F0"), "curl"),
        pd("Turbulence", 0.0, 0.01, 0.001, Some("F4"), "turbulence"),
        pd("Speed", 0.1, 3.0, 1.0, Some("F1"), "speed"),
        pd("Contrast", 1.0, 8.0, 3.5, Some("F1"), "contrast"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd("Count (M)", 0.1, 8.0, 2.0, Some("F1"), "count"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd_whole_labels(
            "Snap Mode",
            0.0,
            4.0,
            0.0,
            &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"],
            "snapMode",
        ),
        pd("Size", 1.0, 8.0, 3.0, Some("F1"), "size"),
        pd("Anti-Clump", 0.0, 60.0, 20.0, Some("F0"), "antiClump"),
        pd("Force", 0.0, 0.1, 0.005, Some("F3"), "force"),
        // 3D-specific params (indices 13-20)
        pd_whole_labels(
            "Container",
            0.0,
            3.0,
            0.0,
            &["None", "Cube", "Sphere", "Torus"],
            "container",
        ),
        pd("Ctr Scale", 0.2, 1.0, 0.8, Some("F2"), "containerScale"),
        pd_whole_labels("Vol Res", 0.0, 2.0, 0.0, &["64", "128", "256"], "volumeRes"),
        pd("Cam Dist", 1.0, 8.0, 3.0, Some("F1"), "camDist"),
        pd("Rotate X", -1.0, 1.0, 0.0, Some("F2"), "rotX"),
        pd("Rotate Y", -1.0, 1.0, 0.0, Some("F2"), "rotY"),
        pd("Rotate Z", -1.0, 1.0, 0.0, Some("F2"), "rotZ"),
        pd("Flatten", 0.0, 1.0, 0.0, Some("F2"), "flatten"),
    ];
    m.insert(
        GeneratorTypeId::FLUID_SIMULATION_3D,
        create_def("Fluid Simulation 3D", false, "fluidSimulation3D", params),
    );

    // ── MRI Volume ──
    let params = vec![
        pd_whole_labels(
            "Slice Axis",
            0.0,
            2.0,
            0.0,
            &["Axial", "Sagittal", "Coronal"],
            "sliceAxis",
        ),
        pd("Slice Pos", 0.0, 1.0, 0.5, Some("F2"), "slicePos"),
        pd("Center", 0.0, 1.0, 0.5, Some("F2"), "center"),
        pd("Width", 0.01, 1.0, 0.8, Some("F2"), "width"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Invert", 0.0, 1.0, 0.0, "invert"),
        pd("Sharpen", 0.0, 3.0, 1.0, Some("F1"), "sharpen"),
        pd_whole_labels(
            "Scan",
            0.0,
            2.0,
            0.0,
            &["250μm 7T", "300μm HiRes", "Edlow 100μm"],
            "scan",
        ),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
    ];
    m.insert(
        GeneratorTypeId::MRI_VOLUME,
        create_def("MRI Volume", false, "mriVolume", params),
    );

    // ── BlackHole ──
    let params = vec![
        pd("Speed", 0.0, 5.0, 0.3, Some("F2"), "speed"),
        pd("Cam Dist", 0.1, 50.0, 20.0, Some("F1"), "camDist"),
        pd("Tilt", 0.0, 90.0, 15.0, Some("F0"), "tilt"),
        pd("Rotate", -180.0, 180.0, 0.0, Some("F0"), "rotate"),
        pd_whole("Steps", 50.0, 500.0, 150.0, "steps"),
        pd("Disk Inner", 2.0, 6.0, 3.0, Some("F1"), "diskInner"),
        pd("Disk Outer", 5.0, 20.0, 10.0, Some("F1"), "diskOuter"),
        pd("Disk Glow", 0.5, 5.0, 2.0, Some("F1"), "diskGlow"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd("Stars", 0.0, 2.0, 0.5, Some("F2"), "stars"),
        pd("Spin", -1.0, 1.0, 0.0, Some("F2"), "spin"),
        pd("Particles", 0.0, 1.0, 0.0, Some("F2"), "particles"),
        pd("Turbulence", 0.0, 5.0, 0.5, Some("F2"), "turbulence"),
        pd("Cam Velocity", 0.0, 0.99, 0.0, Some("F2"), "camVelocity"),
        pd("Freefall", 0.0, 1.0, 0.0, Some("F0"), "freefall"),
    ];
    m.insert(
        GeneratorTypeId::BLACK_HOLE,
        create_def("Black Hole", false, "blackHole", params),
    );

    // ── GalacticRock ──
    let params = vec![
        pd("Speed", 0.0, 5.0, 1.0, Some("F2"), "speed"),
        pd("Wave Amp", 0.0, 0.5, 0.1, Some("F3"), "waveAmp"),
        pd("Wave Freq", 0.1, 2.0, 0.5, Some("F2"), "waveFreq"),
        pd("Twist", 0.0, 20.0, 10.0, Some("F1"), "twist"),
        pd("Grain", 0.0, 0.01, 0.001, Some("F4"), "grain"),
        pd("Roughness", 0.0, 1.0, 0.5, Some("F2"), "roughness"),
        pd("Light Int", 0.1, 10.0, 2.5, Some("F1"), "lightInt"),
        pd("Blur", 0.0, 20.0, 10.0, Some("F0"), "blur"),
        pd("Cam Dist", 0.1, 10.0, 0.8, Some("F2"), "camDist"),
        pd("Cam Orbit", -180.0, 180.0, 0.0, Some("F0"), "camOrbit"),
        pd("Cam Tilt", -90.0, 90.0, 10.0, Some("F0"), "camTilt"),
        pd("Cam FOV", 20.0, 120.0, 60.0, Some("F0"), "camFov"),
        pd("Look Y", -2.0, 2.0, 0.0, Some("F2"), "lookY"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
    ];
    m.insert(
        GeneratorTypeId::GALACTIC_ROCK,
        create_def("Galactic Rock", false, "galacticRock", params),
    );

    // ── MetallicGlass ──
    // All defaults match the TD tutorial spec exactly.
    let params = vec![
        pd("Feedback", 0.5, 1.0, 0.98, Some("F2"), "feedback"),
        pd("Noise Scale", 0.1, 2.0, 0.75, Some("F2"), "noiseScale"),
        pd("Noise Speed", 0.01, 1.0, 0.1, Some("F3"), "noiseSpeed"),
        pd("Edge Str", 0.5, 20.0, 5.0, Some("F1"), "edgeStr"),
        pd("Mirror", 0.0, 90.0, 45.0, Some("F0"), "mirror"),
        pd("Displace", 0.0, 0.5, 0.2, Some("F3"), "displace"),
        pd("Roughness", 0.01, 1.0, 0.05, Some("F3"), "roughness"),
        pd("Light Int", 0.1, 10.0, 3.5, Some("F1"), "lightInt"),
        pd("Cam Dist", 0.5, 10.0, 2.5, Some("F2"), "camDist"),
        pd("Cam Orbit", -180.0, 180.0, 0.0, Some("F0"), "camOrbit"),
        pd("Cam Tilt", -90.0, 90.0, -10.0, Some("F0"), "camTilt"),  // look slightly up
        pd("Cam FOV", 20.0, 120.0, 54.0, Some("F0"), "camFov"),    // 35mm focal = ~54°
        pd("Look Y", -2.0, 2.0, 0.0, Some("F2"), "lookY"),
    ];
    m.insert(
        GeneratorTypeId::METALLIC_GLASS,
        create_def("Metallic Glass", false, "metallicGlass", params),
    );

    // ── ComputeStrangeAttractor ──
    let params = vec![
        pd_whole_labels(
            "Type",
            0.0,
            4.0,
            0.0,
            &["Lorenz", "Rossler", "Aizawa", "Thomas", "Halvorsen"],
            "type",
        ),
        pd("Contrast", 1.0, 8.0, 3.5, Some("F1"), "contrast"),
        pd("Chaos", 0.0, 1.0, 0.0, Some("F2"), "chaos"),
        pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed"),
        pd("Scale", 0.25, 3.0, 1.0, Some("F2"), "scale"),
        pd_toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        pd("Count (M)", 0.1, 2.0, 0.5, Some("F1"), "count"),
        pd("Diffusion", 0.0, 0.05, 0.0, Some("F3"), "diffusion"),
        pd("Tilt", -1.0, 1.0, 0.3, Some("F2"), "tilt"),
        pd("Size", 1.0, 8.0, 3.0, Some("F1"), "size"),
        pd_toggle("Invert", 0.0, 1.0, 0.0, "invert"),
    ];
    m.insert(
        GeneratorTypeId::COMPUTE_STRANGE_ATTRACTOR,
        create_def("Strange Attractor", false, "strangeAttractor", params),
    );

    // ── OilyFluid ──
    // Faithful port of Bileam Tschepe's "red oily fluid" TouchDesigner tutorial.
    // All defaults match the source material exactly.
    let params = vec![
        pd("Speed", 0.1, 4.0, 1.0, Some("F2"), "speed"),
        pd("Feedback", 0.95, 0.9999, 0.998, Some("F4"), "feedback"),
        pd("Noise", 0.0, 0.02, 0.002, Some("F4"), "noise"),
        pd("VelDamp", 0.85, 0.999, 0.98, Some("F3"), "veldamp"),
        pd("Curl", 0.0, 1.0, 0.2, Some("F2"), "curl"),
        pd("Relief", 0.05, 2.0, 0.5, Some("F2"), "relief"),
        pd("Chroma", 0.0, 8.0, 2.0, Some("F2"), "chroma"),
        pd("Contrast", 0.5, 3.0, 1.4, Some("F2"), "contrast"),
        pd("Hue", 0.0, 1.0, 0.0, Some("F2"), "hue"),
        pd("Sat", 0.0, 2.0, 1.0, Some("F2"), "sat"),
        pd("Bright", 0.0, 2.0, 1.0, Some("F2"), "bright"),
        pd("VelDisp", 0.1, 10.0, 1.0, Some("F2"), "velDisp"),
        pd("ColDisp", 0.1, 10.0, 1.0, Some("F2"), "colDisp"),
        pd_whole_labels(
            "Mode",
            0.0,
            4.0,
            0.0,
            &["Oil Slick", "Flow Field", "Height Map", "PBR", "Lines"],
            "mode",
        ),
    ];
    m.insert(
        GeneratorTypeId::OILY_FLUID,
        create_def("Oily Fluid", false, "oilyFluid", params),
    );

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
        string_param_defs: Vec::new(),
        osc_prefix: Some(osc_prefix),
    }
}
