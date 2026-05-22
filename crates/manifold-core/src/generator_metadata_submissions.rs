//! Static `GeneratorMetadata` submissions for all built-in generators.
//!
//! These live in `manifold-core` so that any binary linking `manifold-core`
//! (including test binaries) gets the metadata via `inventory`.
//! The GPU-dependent `GeneratorFactory` submissions remain in `manifold-renderer`.

use crate::generator_registration::{GeneratorAliasMetadata, GeneratorMetadata, ParamSpec};
use crate::generator_type_id::GeneratorTypeId;

// ── Plasma ─────────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::PLASMA,
        display_name: "Plasma",
        is_line_based: false,
        available: true,
        osc_prefix: "plasma",
        legacy_discriminant: Some(6),
        params: &[
            ParamSpec::whole_labels("pattern", "Pattern", 0.0, 7.0, 0.0, &["Classic","Rings","Diamond","Warp","Cells","Noise","Fractal","Lattice"], "pattern"),
            ParamSpec::continuous("complexity", "Complexity", 0.0, 1.0, 0.5, "F2", "complexity"),
            ParamSpec::continuous("contrast", "Contrast", 0.0, 1.0, 0.5, "F2", "contrast"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
        ],
        string_params: &[],
    }
}

// ── Basic Shapes ───────────────────────────────────────────────────────
//
// First generator on the §11 unified-registry path: its schema lives
// entirely in `assets/generator-presets/BasicShapes.json`. The JSON
// preset's `presetMetadata` block is the canonical source of params,
// defaults, value labels, OSC suffixes, and `legacyDiscriminant: 2`
// — the inventory submission that used to live here was overridden by
// the JSON anyway and is now deleted to eliminate the parallel-list
// drift class structurally (no two sources to keep in sync).

// ── Concentric Tunnel ──────────────────────────────────────────────────
//
// Migrated to the §11 unified-registry path: canonical schema lives in
// `assets/generator-presets/ConcentricTunnel.json` (line-rendered
// concentric polygon rings via polygon_shape + concentric_outlines +
// render_lines). The legacy SDF-based generator with Star variant and
// 3-mode clip_trigger split is gone. The new schema has 5 outer-card
// params (Star removed, clip_trigger_mode dropped, legacy `scale`
// renamed to `ring_spacing`). The inventory entry below matches the
// new schema; JSON overrides it at runtime in renderer-linking
// processes. Saved-project migration: `scale` → `ring_spacing` via
// the JSON's paramAliases, `clip_trigger_mode` drops silently (the
// 3 modes are collapsed into a single on/off toggle).

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::CONCENTRIC_TUNNEL,
        display_name: "Concentric Tunnel",
        is_line_based: true,
        available: true,
        osc_prefix: "concentricTunnel",
        legacy_discriminant: Some(5),
        params: &[
            ParamSpec::whole_labels("shape", "Shape", 0.0, 4.0, 0.0, &["Circle","Triangle","Square","Pentagon","Hexagon"], "shape"),
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.008, "F4", "line"),
            ParamSpec::whole_labels("rate", "Rate", 0.0, 4.0, 2.0, &["1/4","1/2","1","2","4"], "rate"),
            ParamSpec::continuous("ring_spacing", "Ring Spacing", 0.05, 0.3, 0.12, "F2", "ringSpacing"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
        ],
        string_params: &[],
    }
}

// ── Tesseract ──────────────────────────────────────────────────────────
//
// Migrated to the §11 unified-registry path: canonical schema lives in
// `assets/generator-presets/Tesseract.json` (with the new general-user
// param names, `legacyDiscriminant: 4`, and `paramAliases` for legacy
// `xy/zw/xw/verts/v_size/anim` names). The JSON-loaded preset
// overrides this inventory entry at runtime in any process that links
// manifold-renderer; the inventory remains as a fallback so upstream
// test binaries that don't link the renderer (e.g.,
// manifold-editing's `command_roundtrips`) still resolve TESSERACT
// through `generator_definition_registry::get`. Same pattern as
// Lissajous and WireframeZoo.

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::TESSERACT,
        display_name: "Tesseract",
        is_line_based: true,
        available: true,
        osc_prefix: "tesseract",
        legacy_discriminant: Some(4),
        params: &[
            ParamSpec::continuous("xy", "XY", 0.0, 2.0, 0.6, "F2", "rotXY"),
            ParamSpec::continuous("zw", "ZW", 0.0, 2.0, 0.4, "F2", "rotZW"),
            ParamSpec::continuous("xw", "XW", 0.0, 2.0, 0.25, "F2", "rotXW"),
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::continuous("dist", "Distance", 1.0, 6.0, 3.0, "F1", "dist"),
            ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 1.0, "verts"),
            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 1.0, "F1", "vsize"),
            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 0.0, "anim"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
        ],
        string_params: &[],
    }
}

// ── Duocylinder ────────────────────────────────────────────────────────
//
// Migrated to the §11 unified-registry path: canonical schema lives in
// `assets/generator-presets/Duocylinder.json`. Inventory remains as
// fallback for non-renderer-linking crates. See Tesseract above for
// the rationale.

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::DUOCYLINDER,
        display_name: "Duocylinder",
        is_line_based: true,
        available: true,
        osc_prefix: "duocylinder",
        legacy_discriminant: Some(3),
        params: &[
            ParamSpec::continuous("xy", "XY", 0.0, 2.0, 0.4, "F2", "rotXY"),
            ParamSpec::continuous("zw", "ZW", 0.0, 2.0, 0.25, "F2", "rotZW"),
            ParamSpec::continuous("xw", "XW", 0.0, 2.0, 0.15, "F2", "rotXW"),
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.0015, "F4", "line"),
            ParamSpec::continuous("dist", "Distance", 1.0, 6.0, 3.0, "F1", "dist"),
            ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 1.0, "verts"),
            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 1.0, "F1", "vsize"),
            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 0.0, "anim"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
        ],
        string_params: &[],
    }
}

// ── Lissajous ──────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::LISSAJOUS,
        display_name: "Lissajous",
        is_line_based: true,
        available: true,
        osc_prefix: "lissajous",
        legacy_discriminant: Some(7),
        params: &[
            ParamSpec::continuous("freq_x", "Freq X", 0.0, 2.0, 0.1, "F2", "freqX"),
            ParamSpec::continuous("freq_y", "Freq Y", 0.0, 2.0, 0.1, "F2", "freqY"),
            ParamSpec::continuous("phase", "Phase", 0.0, 2.0, 0.0, "F2", "phase"),
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 0.0, "verts"),
            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 0.5, "F1", "vsize"),
            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 1.0, "anim"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.5, "F2", "window"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
        ],
        string_params: &[],
    }
}

// ── Wireframe ──────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::WIREFRAME_ZOO,
        display_name: "Wireframe",
        is_line_based: true,
        available: true,
        osc_prefix: "wireframeZoo",
        legacy_discriminant: Some(10),
        params: &[
            ParamSpec::continuous("rotate_x_speed", "Rotate X Speed", 0.0, 2.0, 0.5, "F2", "rotateXSpeed"),
            ParamSpec::continuous("rotate_y_speed", "Rotate Y Speed", 0.0, 2.0, 0.3, "F2", "rotateYSpeed"),
            ParamSpec::continuous("rotate_z_speed", "Rotate Z Speed", 0.0, 2.0, 0.2, "F2", "rotateZSpeed"),
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.003, "F4", "line"),
            ParamSpec::whole_labels("shape", "Shape", 0.0, 4.0, 0.0, &["Tetra","Cube","Octa","Icosa","Dodeca"], "shape"),
            ParamSpec::toggle("show_verts", "Show Vertices", 0.0, 1.0, 1.0, "showVerts"),
            ParamSpec::continuous("vert_size", "Vertex Size", 0.1, 4.0, 1.0, "F1", "vertSize"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
        ],
        string_params: &[],
    }
}

// ── Oscilloscope XY ────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::OSCILLOSCOPE_XY,
        display_name: "Oscilloscope XY",
        is_line_based: true,
        available: true,
        osc_prefix: "oscilloscopeXY",
        legacy_discriminant: Some(9),
        params: &[
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 0.0, "verts"),
            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 0.5, "F1", "vsize"),
            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 1.0, "anim"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.63, "F1", "speed"),
            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.59, "F2", "window"),
            ParamSpec::continuous("wave", "Wave", 0.1, 3.0, 0.3, "F1", "wave"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.75, "F2", "scale"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
        ],
        string_params: &[],
    }
}

// ── Fluid Simulation ──────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::FLUID_SIMULATION,
        display_name: "Fluid Simulation",
        is_line_based: false,
        available: true,
        osc_prefix: "fluidSimulation",
        legacy_discriminant: Some(15),
        params: &[
            ParamSpec::continuous("flow", "Flow", -0.1, -0.001, -0.01, "F3", "flow"),
            ParamSpec::whole("feather", "Feather", 4.0, 60.0, 20.0, "feather"),
            ParamSpec::continuous("curl", "Curl", 30.0, 90.0, 85.0, "F0", "curl"),
            ParamSpec::continuous("turbulence", "Turbulence", 0.0, 0.01, 0.001, "F4", "turbulence"),
            ParamSpec::continuous("speed", "Speed", 0.1, 3.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("contrast", "Contrast", 1.0, 8.0, 3.5, "F1", "contrast"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("count_m", "Particle Count", 0.1, 8.0, 2.0, "F1", "M"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
            ParamSpec::whole_labels("clip_trigger_mode", "Clip Trigger Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "clipTriggerMode"),
            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),
            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),
            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),
            ParamSpec::continuous("fill", "Fill", 0.0, 1.0, 1.0, "F2", "fill"),
        ],
        string_params: &[],
    }
}

// ── Fluid Simulation 3D ───────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::FLUID_SIMULATION_3D,
        display_name: "Fluid Simulation 3D",
        is_line_based: false,
        available: true,
        osc_prefix: "fluidSimulation3D",
        legacy_discriminant: Some(19),
        params: &[
            ParamSpec::continuous("flow", "Flow", -0.1, -0.001, -0.01, "F3", "flow"),
            ParamSpec::whole("feather", "Feather", 4.0, 60.0, 20.0, "feather"),
            ParamSpec::continuous("curl", "Curl", 30.0, 90.0, 85.0, "F0", "curl"),
            ParamSpec::continuous("turbulence", "Turbulence", 0.0, 0.01, 0.001, "F4", "turbulence"),
            ParamSpec::continuous("speed", "Speed", 0.1, 3.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("contrast", "Contrast", 1.0, 8.0, 3.5, "F1", "contrast"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("count_m", "Particle Count", 0.1, 8.0, 2.0, "F1", "M"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
            ParamSpec::whole_labels("clip_trigger_mode", "Clip Trigger Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "clipTriggerMode"),
            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),
            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),
            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),
            ParamSpec::whole_labels("container", "Container", 0.0, 3.0, 0.0, &["None", "Cube", "Sphere", "Torus"], "container"),
            ParamSpec::continuous("ctr_scale", "Container Scale", 0.2, 1.0, 0.8, "F2", "containerScale"),
            ParamSpec::whole_labels("vol_res", "Volume Resolution", 0.0, 2.0, 0.0, &["64", "128", "256"], "volumeRes"),
            ParamSpec::continuous("cam_dist", "Cam Dist", 1.0, 8.0, 3.0, "F1", "camDist"),
            ParamSpec::continuous("rotate_x", "Rotate X", -1.0, 1.0, 0.0, "F2", "rotX"),
            ParamSpec::continuous("rotate_y", "Rotate Y", -1.0, 1.0, 0.0, "F2", "rotY"),
            ParamSpec::continuous("rotate_z", "Rotate Z", -1.0, 1.0, 0.0, "F2", "rotZ"),
            ParamSpec::continuous("flatten", "Flatten", 0.0, 1.0, 0.0, "F2", "flatten"),
        ],
        string_params: &[],
    }
}

// ── Nested Cubes ───────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::NESTED_CUBES,
        display_name: "Nested Cubes",
        is_line_based: false,
        available: true,
        osc_prefix: "nestedCubes",
        legacy_discriminant: Some(25),
        params: &[
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("filter", "Filter", 0.1, 10.0, 2.0, "F1", "filter"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("scatter", "Scatter", 0.0, 1.0, 0.0, "F2", "scatter"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
            ParamSpec::whole_labels("clip_trigger_mode", "Clip Trigger Mode", 0.0, 1.0, 0.0,
                &["Envelope", "Pose"],
                "clipTriggerMode",
            ),
        ],
        string_params: &[],
    }
}

// ── Black Hole ─────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::BLACK_HOLE,
        display_name: "Black Hole",
        is_line_based: false,
        available: true,
        osc_prefix: "blackHole",
        legacy_discriminant: Some(21),
        params: &[
            ParamSpec::continuous("speed", "Speed", 0.0, 5.0, 0.3, "F2", "speed"),
            ParamSpec::continuous("cam_dist", "Camera Distance", 0.1, 50.0, 20.0, "F1", "camDist"),
            ParamSpec::continuous("tilt", "Tilt", 0.0, 90.0, 15.0, "F0", "tilt"),
            ParamSpec::continuous("rotate", "Rotate", -180.0, 180.0, 0.0, "F0", "rotate"),
            ParamSpec::whole("steps", "Steps", 50.0, 500.0, 150.0, "steps"),
            ParamSpec::continuous("disk_inner", "Disk Inner", 2.0, 6.0, 3.0, "F1", "diskInner"),
            ParamSpec::continuous("disk_outer", "Disk Outer", 5.0, 20.0, 10.0, "F1", "diskOuter"),
            ParamSpec::continuous("disk_glow", "Disk Glow", 0.5, 5.0, 2.0, "F1", "diskGlow"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("stars", "Stars", 0.0, 2.0, 0.5, "F2", "stars"),
            ParamSpec::continuous("spin", "Spin", -1.0, 1.0, 0.0, "F2", "spin"),
            ParamSpec::continuous("particles", "Particles", 0.0, 1.0, 0.0, "F2", "particles"),
            ParamSpec::continuous("turbulence", "Turbulence", 0.0, 5.0, 0.5, "F2", "turbulence"),
            ParamSpec::continuous("cam_velocity", "Camera Velocity", 0.0, 0.99, 0.0, "F2", "camVelocity"),
            ParamSpec::continuous("freefall", "Freefall", 0.0, 1.0, 0.0, "F2", "freefall"),
        ],
        string_params: &[],
    }
}

// ── Metallic Glass ─────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::METALLIC_GLASS,
        display_name: "Metallic Glass",
        is_line_based: false,
        available: true,
        osc_prefix: "metallicGlass",
        legacy_discriminant: Some(23),
        params: &[
            ParamSpec::continuous("feedback", "Feedback", 0.5, 1.0, 0.98, "F2", "feedback"),
            ParamSpec::continuous("noise_scale", "Noise Scale", 0.1, 2.0, 0.75, "F2", "noiseScale"),
            ParamSpec::continuous("noise_speed", "Noise Speed", 0.01, 1.0, 0.1, "F3", "noiseSpeed"),
            ParamSpec::continuous("edge_str", "Edge Strength", 0.5, 20.0, 5.0, "F1", "edgeStr"),
            ParamSpec::continuous("mirror", "Mirror", 0.0, 90.0, 45.0, "F0", "mirror"),
            ParamSpec::continuous("displace", "Displace", 0.0, 0.5, 0.2, "F3", "displace"),
            ParamSpec::continuous("roughness", "Roughness", 0.01, 1.0, 0.05, "F3", "roughness"),
            ParamSpec::continuous("light_int", "Light Intensity", 0.1, 10.0, 3.5, "F1", "lightInt"),
            ParamSpec::continuous("cam_dist", "Camera Distance", 0.5, 10.0, 2.5, "F2", "camDist"),
            ParamSpec::continuous("cam_orbit", "Camera Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),
            ParamSpec::continuous("cam_tilt", "Camera Tilt", -90.0, 90.0, -10.0, "F0", "camTilt"),
            ParamSpec::continuous("cam_fov", "Camera FOV", 20.0, 120.0, 54.0, "F0", "camFov"),
            ParamSpec::continuous("look_y", "Look Y", -2.0, 2.0, 0.0, "F2", "lookY"),
        ],
        string_params: &[],
    }
}

// ── Oily Fluid ─────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::OILY_FLUID,
        display_name: "Oily Fluid",
        is_line_based: false,
        available: true,
        osc_prefix: "oilyFluid",
        legacy_discriminant: Some(24),
        params: &[
            ParamSpec::continuous("speed", "Speed", 0.1, 4.0, 1.0, "F2", "speed"),
            ParamSpec::continuous("feedback", "Feedback", 0.95, 0.9999, 0.998, "F4", "feedback"),
            ParamSpec::continuous("noise", "Noise", 0.0, 0.02, 0.002, "F4", "noise"),
            ParamSpec::continuous("vel_damp", "Velocity Damp", 0.85, 0.999, 0.98, "F3", "veldamp"),
            ParamSpec::continuous("curl", "Curl", 0.0, 1.0, 0.2, "F2", "curl"),
            ParamSpec::continuous("relief", "Relief", 0.05, 2.0, 0.5, "F2", "relief"),
            ParamSpec::continuous("chroma", "Chroma", 0.0, 8.0, 2.0, "F2", "chroma"),
            ParamSpec::continuous("contrast", "Contrast", 0.5, 3.0, 1.4, "F2", "contrast"),
            ParamSpec::continuous("hue", "Hue", 0.0, 1.0, 0.0, "F2", "hue"),
            ParamSpec::continuous("sat", "Saturation", 0.0, 2.0, 1.0, "F2", "sat"),
            ParamSpec::continuous("bright", "Brightness", 0.0, 2.0, 1.0, "F2", "bright"),
            ParamSpec::continuous("vel_disp", "Velocity Displace", 0.1, 10.0, 1.0, "F2", "velDisp"),
            ParamSpec::continuous("col_disp", "Color Displace", 0.1, 10.0, 1.0, "F2", "colDisp"),
            ParamSpec::whole_labels("mode", "Mode", 0.0, 4.0, 0.0, &["Oil Slick", "Flow Field", "Height Map", "PBR", "Lines"], "mode"),
        ],
        string_params: &[],
    }
}

// ── MRI Volume ─────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::MRI_VOLUME,
        display_name: "MRI Volume",
        is_line_based: false,
        available: true,
        osc_prefix: "mriVolume",
        legacy_discriminant: Some(20),
        params: &[
            ParamSpec::whole_labels("slice_axis", "Slice Axis", 0.0, 2.0, 0.0, &["Axial", "Sagittal", "Coronal"], "sliceAxis"),
            ParamSpec::continuous("slice_pos", "Slice Pos", 0.0, 1.0, 0.5, "F2", "slicePos"),
            ParamSpec::continuous("center", "Center", 0.0, 1.0, 0.5, "F2", "center"),
            ParamSpec::continuous("width", "Width", 0.01, 1.0, 0.8, "F2", "width"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("invert", "Invert", 0.0, 1.0, 0.0, "invert"),
            ParamSpec::continuous("sharpen", "Sharpen", 0.0, 3.0, 1.0, "F1", "sharpen"),
            ParamSpec::whole_labels("scan", "Scan", 0.0, 2.0, 0.0, &["250µm 7T", "300µm HiRes", "Edlow 100µm"], "scan"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
        ],
        string_params: &[],
    }
}

// ── Star Field ─────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::STAR_FIELD,
        display_name: "Star Field",
        is_line_based: false,
        available: true,
        osc_prefix: "starField",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("density", "Density", 0.0, 1.0, 0.5, "F2", "density"),
            ParamSpec::continuous("brightness", "Brightness", 0.0, 2.0, 0.7, "F2", "brightness"),
            ParamSpec::continuous("depth", "Depth", 0.0, 1.0, 0.5, "F2", "depth"),
            ParamSpec::continuous("drift_speed", "Drift Speed", 0.0, 1.0, 0.15, "F2", "driftSpeed"),
            ParamSpec::continuous("drift_x", "Drift X", -1.0, 1.0, 0.3, "F2", "driftX"),
            ParamSpec::continuous("drift_y", "Drift Y", -1.0, 1.0, 0.1, "F2", "driftY"),
            ParamSpec::continuous("twinkle", "Twinkle", 0.0, 1.0, 0.3, "F2", "twinkle"),
            ParamSpec::continuous("warmth", "Warmth", -1.0, 1.0, 0.0, "F2", "warmth"),
            ParamSpec::continuous("glow", "Glow", 0.0, 1.0, 0.3, "F2", "glow"),
        ],
        string_params: &[],
    }
}

// ── Text ───────────────────────────────────────────────────────────────
// NOTE: Another agent may be modifying the text generator. This metadata
// is the baseline; the text generator agent should update it here if params change.

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::TEXT,
        display_name: "Text",
        is_line_based: false,
        available: true,
        osc_prefix: "text",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("size", "Size", 0.02, 1.0, 0.25, "F2", "size"),
            ParamSpec::continuous("position_x", "Position X", -1.0, 1.0, 0.0, "F2", "posX"),
            ParamSpec::continuous("position_y", "Position Y", -1.0, 1.0, 0.0, "F2", "posY"),
            ParamSpec::continuous("scale", "Scale", 0.1, 5.0, 1.0, "F2", "scale"),
            ParamSpec::whole_labels("h_align", "H Align", 0.0, 2.0, 1.0, &["Left", "Center", "Right"], "hAlign"),
            ParamSpec::whole_labels("v_align", "V Align", 0.0, 2.0, 1.0, &["Top", "Center", "Bottom"], "vAlign"),
            ParamSpec::continuous("letter_spacing", "Letter Spacing", -0.5, 2.0, 0.0, "F2", "letterSpacing"),
            ParamSpec::continuous("line_spacing", "Line Spacing", 0.5, 3.0, 1.2, "F1", "lineSpacing"),
        ],
        string_params: &[("Text", "text", "HELLO", false), ("Font", "fontFamily", "", true)],
    }
}

// ── Particle Text ───────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::PARTICLE_TEXT,
        display_name: "Particle Text",
        is_line_based: false,
        available: true,
        osc_prefix: "particleText",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("flow", "Flow", -0.1, -0.001, -0.01, "F3", "flow"),
            ParamSpec::whole("feather", "Feather", 4.0, 60.0, 20.0, "feather"),
            ParamSpec::continuous("curl", "Curl", 30.0, 90.0, 85.0, "F0", "curl"),
            ParamSpec::continuous("turbulence", "Turbulence", 0.0, 0.01, 0.001, "F4", "turbulence"),
            ParamSpec::continuous("speed", "Speed", 0.1, 3.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("contrast", "Contrast", 1.0, 8.0, 3.5, "F1", "contrast"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("count_m", "Particle Count", 0.1, 8.0, 2.0, "F1", "M"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 0.0, "clipTrigger"),
            ParamSpec::whole_labels("clip_trigger_mode", "Clip Trigger Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "clipTriggerMode"),
            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),
            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),
            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),
            ParamSpec::continuous("text_size", "Text Size", 0.05, 1.0, 0.25, "F2", "textSize"),
        ],
        string_params: &[("Text", "text", "HELLO", false), ("Font", "fontFamily", "", true)],
    }
}

// ── Digital Plants ────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::DIGITAL_PLANTS,
        display_name: "Digital Plants",
        is_line_based: false,
        available: true,
        osc_prefix: "digitalPlants",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("noise_scale", "Noise Scale", 0.1, 5.0, 1.5, "F2", "noiseScale"),
            ParamSpec::continuous("anim_speed", "Animation Speed", 0.0, 1.0, 0.5, "F2", "animSpeed"),
            ParamSpec::continuous("morph", "Morph", 0.0, 1.0, 0.0, "F2", "morph"),
            ParamSpec::continuous("base_radius", "Base Radius", 0.1, 2.0, 0.6, "F2", "baseRadius"),
            ParamSpec::continuous("height", "Height", 0.5, 4.0, 2.0, "F2", "height"),
            ParamSpec::continuous("taper", "Taper", 0.0, 3.0, 1.5, "F2", "taper"),
            ParamSpec::continuous("torus_radius", "Torus Radius", 0.5, 3.0, 1.2, "F2", "torusRadius"),
            ParamSpec::continuous("petal_amp", "Petal Amplitude", 0.0, 80.0, 60.0, "F0", "petalAmp"),
            ParamSpec::continuous("rot_speed", "Rotation Speed", 0.0, 3.0, 0.3, "F2", "rotSpeed"),
            ParamSpec::continuous("box_scale", "Box Scale", 0.005, 0.08, 0.025, "F3", "boxScale"),
            ParamSpec::continuous("cam_dist", "Camera Distance", 0.5, 10.0, 3.5, "F1", "camDist"),
            ParamSpec::continuous("cam_orbit", "Camera Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),
            ParamSpec::continuous("cam_tilt", "Camera Tilt", -90.0, 90.0, 15.0, "F0", "camTilt"),
            ParamSpec::continuous("cam_fov", "Camera FOV", 20.0, 120.0, 50.0, "F0", "camFov"),
        ],
        string_params: &[],
    }
}

// ── Param aliases ─────────────────────────────────────────────────────
//
// Backward-compat for the `snap` / `snap_mode` → `clip_trigger` /
// `clip_trigger_mode` rename. Projects saved before the rename
// reference these params by their old id in driver bindings and
// Ableton mappings; the alias table redirects lookups on load.
//
// One submission per generator that had a snap param. Driver values
// stored positionally in `Layer.gen_params.param_values` aren't
// affected — they're keyed by index, not by id — so the rename is
// transparent for plain slider state. Only id-keyed wire / mapping
// targets need the alias.

const SNAP_ALIASES: &[crate::effect_registration::ParamAlias] = &[
    ("snap", Some("clip_trigger")),
];

const SNAP_AND_MODE_ALIASES: &[crate::effect_registration::ParamAlias] = &[
    ("snap", Some("clip_trigger")),
    ("snap_mode", Some("clip_trigger_mode")),
];

/// WireframeZoo rename aliases — the legacy outer-card param IDs
/// (`xy` / `zw` / `xw` / `verts` / `v_size`) are 4D-plane / shorthand
/// names that don't communicate to general users. The decomposition
/// pass renamed them to `rotate_x_speed` / `rotate_y_speed` /
/// `rotate_z_speed` / `show_verts` / `vert_size`. This alias table
/// redirects id-keyed wire / mapping targets in saved projects.
const WIREFRAME_ZOO_ALIASES: &[crate::effect_registration::ParamAlias] = &[
    ("xy", Some("rotate_x_speed")),
    ("zw", Some("rotate_y_speed")),
    ("xw", Some("rotate_z_speed")),
    ("verts", Some("show_verts")),
    ("v_size", Some("vert_size")),
];

inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::PLASMA, aliases: SNAP_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::WIREFRAME_ZOO, aliases: WIREFRAME_ZOO_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::CONCENTRIC_TUNNEL, aliases: SNAP_AND_MODE_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::LISSAJOUS, aliases: SNAP_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::OSCILLOSCOPE_XY, aliases: SNAP_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::FLUID_SIMULATION, aliases: SNAP_AND_MODE_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::FLUID_SIMULATION_3D, aliases: SNAP_AND_MODE_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::NESTED_CUBES, aliases: SNAP_AND_MODE_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::MRI_VOLUME, aliases: SNAP_ALIASES }
}
inventory::submit! {
    GeneratorAliasMetadata { id: GeneratorTypeId::PARTICLE_TEXT, aliases: SNAP_AND_MODE_ALIASES }
}
