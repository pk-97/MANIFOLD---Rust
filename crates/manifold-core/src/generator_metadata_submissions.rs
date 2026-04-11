//! Static `GeneratorMetadata` submissions for all built-in generators.
//!
//! These live in `manifold-core` so that any binary linking `manifold-core`
//! (including test binaries) gets the metadata via `inventory`.
//! The GPU-dependent `GeneratorFactory` submissions remain in `manifold-renderer`.

use crate::generator_registration::{GeneratorMetadata, ParamSpec};
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
            ParamSpec::whole_labels("Pattern", 0.0, 7.0, 0.0, &["Classic","Rings","Diamond","Warp","Cells","Noise","Fractal","Lattice"], "pattern"),
            ParamSpec::continuous("Complexity", 0.0, 1.0, 0.5, "F2", "complexity"),
            ParamSpec::continuous("Contrast", 0.0, 1.0, 0.63, "F2", "contrast"),
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 1.0, "snap"),
        ],
        string_params: &[],
    }
}

// ── Basic Shapes Snap ──────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::BASIC_SHAPES_SNAP,
        display_name: "Basic Shapes Snap",
        is_line_based: false,
        available: true,
        osc_prefix: "basicShapesSnap",
        legacy_discriminant: Some(2),
        params: &[
            ParamSpec::continuous("Line", 0.0005, 0.03, 0.015, "F4", "line"),
            ParamSpec::whole_labels("Shape", 0.0, 2.0, 0.0, &["Square","Diamond","Octagon"], "shape"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::whole_labels("Fill", 0.0, 2.0, 1.0, &["Solid","Mixed","Wireframe"], "fill"),
        ],
        string_params: &[],
    }
}

// ── Concentric Tunnel ──────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::CONCENTRIC_TUNNEL,
        display_name: "Concentric Tunnel",
        is_line_based: false,
        available: true,
        osc_prefix: "concentricTunnel",
        legacy_discriminant: Some(5),
        params: &[
            ParamSpec::whole_labels("Shape", 0.0, 5.0, 0.0, &["Circle","Triangle","Square","Pentagon","Hexagon","Star"], "shape"),
            ParamSpec::continuous("Line", 0.0005, 0.03, 0.008, "F4", "line"),
            ParamSpec::whole_labels("Rate", 0.0, 4.0, 2.0, &["1/4","1/2","1","2","4"], "speed"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 0.0, "snap"),
            ParamSpec::whole_labels("Snap Mode", 0.0, 2.0, 0.0, &["Shape","Spawn","Both"], "snapmode"),
        ],
        string_params: &[],
    }
}

// ── Tesseract ──────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::TESSERACT,
        display_name: "Tesseract",
        is_line_based: true,
        available: true,
        osc_prefix: "tesseract",
        legacy_discriminant: Some(4),
        params: &[
            ParamSpec::continuous("XY", 0.0, 2.0, 0.6, "F2", "rotXY"),
            ParamSpec::continuous("ZW", 0.0, 2.0, 0.4, "F2", "rotZW"),
            ParamSpec::continuous("XW", 0.0, 2.0, 0.25, "F2", "rotXW"),
            ParamSpec::continuous("Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::continuous("Dist", 1.0, 6.0, 3.0, "F1", "dist"),
            ParamSpec::toggle("Verts", 0.0, 1.0, 1.0, "verts"),
            ParamSpec::continuous("VSize", 0.1, 4.0, 1.0, "F1", "vsize"),
            ParamSpec::toggle("Anim", 0.0, 1.0, 0.0, "anim"),
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Window", 0.01, 1.0, 0.1, "F2", "window"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
        ],
        string_params: &[],
    }
}

// ── Duocylinder ────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::DUOCYLINDER,
        display_name: "Duocylinder",
        is_line_based: true,
        available: true,
        osc_prefix: "duocylinder",
        legacy_discriminant: Some(3),
        params: &[
            ParamSpec::continuous("XY", 0.0, 2.0, 0.4, "F2", "rotXY"),
            ParamSpec::continuous("ZW", 0.0, 2.0, 0.25, "F2", "rotZW"),
            ParamSpec::continuous("XW", 0.0, 2.0, 0.15, "F2", "rotXW"),
            ParamSpec::continuous("Line", 0.0005, 0.03, 0.0015, "F4", "line"),
            ParamSpec::continuous("Dist", 1.0, 6.0, 3.0, "F1", "dist"),
            ParamSpec::toggle("Verts", 0.0, 1.0, 1.0, "verts"),
            ParamSpec::continuous("VSize", 0.1, 4.0, 1.0, "F1", "vsize"),
            ParamSpec::toggle("Anim", 0.0, 1.0, 0.0, "anim"),
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Window", 0.01, 1.0, 0.1, "F2", "window"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
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
            ParamSpec::continuous("Freq X", 0.0, 2.0, 0.13, "F2", "freqX"),
            ParamSpec::continuous("Freq Y", 0.0, 2.0, 0.09, "F2", "freqY"),
            ParamSpec::continuous("Phase", 0.0, 2.0, 0.07, "F2", "phase"),
            ParamSpec::continuous("Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::toggle("Verts", 0.0, 1.0, 0.0, "verts"),
            ParamSpec::continuous("VSize", 0.1, 4.0, 0.5, "F1", "vsize"),
            ParamSpec::toggle("Anim", 0.0, 1.0, 1.0, "anim"),
            ParamSpec::continuous("Speed", 0.1, 5.0, 2.67, "F1", "speed"),
            ParamSpec::continuous("Window", 0.01, 1.0, 0.74, "F2", "window"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.55, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 1.0, "snap"),
        ],
        string_params: &[],
    }
}

// ── Wireframe Zoo ──────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::WIREFRAME_ZOO,
        display_name: "Wireframe Zoo",
        is_line_based: true,
        available: true,
        osc_prefix: "wireframeZoo",
        legacy_discriminant: Some(10),
        params: &[
            ParamSpec::continuous("XY", 0.0, 2.0, 0.5, "F2", "rotXY"),
            ParamSpec::continuous("ZW", 0.0, 2.0, 0.3, "F2", "rotZW"),
            ParamSpec::continuous("XW", 0.0, 2.0, 0.2, "F2", "rotXW"),
            ParamSpec::continuous("Line", 0.0005, 0.03, 0.003, "F4", "line"),
            ParamSpec::whole_labels("Shape", 0.0, 4.0, 0.0, &["Tetra","Cube","Octa","Icosa","Dodeca"], "shape"),
            ParamSpec::toggle("Verts", 0.0, 1.0, 1.0, "verts"),
            ParamSpec::continuous("VSize", 0.1, 4.0, 1.0, "F1", "vsize"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
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
            ParamSpec::continuous("Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::toggle("Verts", 0.0, 1.0, 0.0, "verts"),
            ParamSpec::continuous("VSize", 0.1, 4.0, 0.5, "F1", "vsize"),
            ParamSpec::toggle("Anim", 0.0, 1.0, 1.0, "anim"),
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.63, "F1", "speed"),
            ParamSpec::continuous("Window", 0.01, 1.0, 0.59, "F2", "window"),
            ParamSpec::continuous("Wave", 0.1, 3.0, 0.3, "F1", "wave"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.75, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 1.0, "snap"),
        ],
        string_params: &[],
    }
}

// ── Parametric Surface ─────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::PARAMETRIC_SURFACE,
        display_name: "Parametric Surface",
        is_line_based: false,
        available: true,
        osc_prefix: "parametricSurface",
        legacy_discriminant: Some(13),
        params: &[
            ParamSpec::whole_labels("Shape", 0.0, 4.0, 0.0, &["Gyroid","Schwarz P","Schwarz D","Torus Knot","Klein"], "shape"),
            ParamSpec::continuous("Morph", 0.0, 1.0, 0.0, "F2", "morph"),
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 1.0, "snap"),
        ],
        string_params: &[],
    }
}

// ── Mycelium ───────────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::MYCELIUM,
        display_name: "Mycelium",
        is_line_based: false,
        available: true,
        osc_prefix: "mycelium",
        legacy_discriminant: Some(17),
        params: &[
            ParamSpec::continuous("SensDist", 0.005, 0.1, 0.02, "F3", "sensdist"),
            ParamSpec::continuous("SensAngle", 0.1, 1.5, 0.8, "F2", "sensangle"),
            ParamSpec::continuous("Turn", 0.05, 1.5, 0.4, "F2", "turn"),
            ParamSpec::continuous("Step", 0.0002, 0.005, 0.001, "F4", "step"),
            ParamSpec::continuous("Deposit", 0.1, 5.0, 1.5, "F1", "deposit"),
            ParamSpec::continuous("Decay", 0.85, 1.0, 0.98, "F3", "decay"),
            ParamSpec::continuous("Color", 0.0, 1.0, 0.08, "F2", "color"),
            ParamSpec::continuous("Glow", 0.0, 3.0, 1.0, "F1", "glow"),
            ParamSpec::continuous("Reactivity", 0.0, 1.0, 0.5, "F2", "reactivity"),
            ParamSpec::whole("Agents", 10.0, 500.0, 200.0, "agents"),
            ParamSpec::continuous("Scale", 0.1, 2.0, 1.0, "F2", "scale"),
            ParamSpec::whole("Seeds", 1.0, 5.0, 1.0, "seeds"),
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
            ParamSpec::continuous("Flow", -0.1, -0.001, -0.01, "F3", "flow"),
            ParamSpec::whole("Feather", 4.0, 60.0, 20.0, "feather"),
            ParamSpec::continuous("Curl", 30.0, 90.0, 85.0, "F0", "curl"),
            ParamSpec::continuous("Turbulence", 0.0, 0.01, 0.001, "F4", "turbulence"),
            ParamSpec::continuous("Speed", 0.1, 3.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Contrast", 1.0, 8.0, 3.5, "F1", "contrast"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("Count (M)", 0.1, 8.0, 2.0, "F1", "count"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 0.0, "snap"),
            ParamSpec::whole_labels("Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),
            ParamSpec::continuous("Size", 1.0, 8.0, 3.0, "F1", "size"),
            ParamSpec::continuous("Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),
            ParamSpec::continuous("Force", 0.0, 0.1, 0.005, "F3", "force"),
        ],
        string_params: &[],
    }
}

// ── Fluid Simulation 3D ───────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::FLUID_SIMULATION_3D,
        display_name: "Fluid Sim 3D",
        is_line_based: false,
        available: true,
        osc_prefix: "fluidSimulation3D",
        legacy_discriminant: Some(19),
        params: &[
            ParamSpec::continuous("Flow", -0.1, -0.001, -0.01, "F3", "flow"),
            ParamSpec::whole("Feather", 4.0, 60.0, 20.0, "feather"),
            ParamSpec::continuous("Curl", 30.0, 90.0, 85.0, "F0", "curl"),
            ParamSpec::continuous("Turbulence", 0.0, 0.01, 0.001, "F4", "turbulence"),
            ParamSpec::continuous("Speed", 0.1, 3.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Contrast", 1.0, 8.0, 3.5, "F1", "contrast"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("Count (M)", 0.1, 8.0, 2.0, "F1", "count"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 0.0, "snap"),
            ParamSpec::whole_labels("Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),
            ParamSpec::continuous("Size", 1.0, 8.0, 3.0, "F1", "size"),
            ParamSpec::continuous("Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),
            ParamSpec::continuous("Force", 0.0, 0.1, 0.005, "F3", "force"),
            ParamSpec::whole_labels("Container", 0.0, 3.0, 0.0, &["None", "Cube", "Sphere", "Torus"], "container"),
            ParamSpec::continuous("Ctr Scale", 0.2, 1.0, 0.8, "F2", "containerScale"),
            ParamSpec::whole_labels("Vol Res", 0.0, 2.0, 0.0, &["64", "128", "256"], "volumeRes"),
            ParamSpec::continuous("Cam Dist", 1.0, 8.0, 3.0, "F1", "camDist"),
            ParamSpec::continuous("Rotate X", -1.0, 1.0, 0.0, "F2", "rotX"),
            ParamSpec::continuous("Rotate Y", -1.0, 1.0, 0.0, "F2", "rotY"),
            ParamSpec::continuous("Rotate Z", -1.0, 1.0, 0.0, "F2", "rotZ"),
            ParamSpec::continuous("Flatten", 0.0, 1.0, 0.0, "F2", "flatten"),
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
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Filter", 0.1, 10.0, 2.0, "F1", "filter"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("Scatter", 0.0, 1.0, 0.0, "F2", "scatter"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 0.0, "snap"),
            ParamSpec::whole_labels(
                "Snap Mode", 0.0, 1.0, 0.0,
                &["Envelope", "Pose"],
                "snapMode",
            ),
        ],
        string_params: &[],
    }
}

// ── Galactic Rock ──────────────────────────────────────────────────────

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::GALACTIC_ROCK,
        display_name: "Galactic Rock",
        is_line_based: false,
        available: true,
        osc_prefix: "galacticRock",
        legacy_discriminant: Some(22),
        params: &[
            ParamSpec::continuous("Speed", 0.0, 5.0, 1.0, "F2", "speed"),
            ParamSpec::continuous("Wave Amp", 0.0, 0.5, 0.1, "F3", "waveAmp"),
            ParamSpec::continuous("Wave Freq", 0.1, 2.0, 0.5, "F2", "waveFreq"),
            ParamSpec::continuous("Twist", 0.0, 20.0, 10.0, "F1", "twist"),
            ParamSpec::continuous("Grain", 0.0, 0.01, 0.001, "F4", "grain"),
            ParamSpec::continuous("Roughness", 0.0, 1.0, 0.5, "F2", "roughness"),
            ParamSpec::continuous("Light Int", 0.1, 10.0, 2.5, "F1", "lightInt"),
            ParamSpec::continuous("Blur", 0.0, 20.0, 10.0, "F0", "blur"),
            ParamSpec::continuous("Cam Dist", 0.1, 10.0, 0.8, "F2", "camDist"),
            ParamSpec::continuous("Cam Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),
            ParamSpec::continuous("Cam Tilt", -90.0, 90.0, 10.0, "F0", "camTilt"),
            ParamSpec::continuous("Cam FOV", 20.0, 120.0, 60.0, "F0", "camFov"),
            ParamSpec::continuous("Look Y", -2.0, 2.0, 0.0, "F2", "lookY"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
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
            ParamSpec::continuous("Speed", 0.0, 5.0, 0.3, "F2", "speed"),
            ParamSpec::continuous("Cam Dist", 0.1, 50.0, 20.0, "F1", "camDist"),
            ParamSpec::continuous("Tilt", 0.0, 90.0, 15.0, "F0", "tilt"),
            ParamSpec::continuous("Rotate", -180.0, 180.0, 0.0, "F0", "rotate"),
            ParamSpec::whole("Steps", 50.0, 500.0, 150.0, "steps"),
            ParamSpec::continuous("Disk Inner", 2.0, 6.0, 3.0, "F1", "diskInner"),
            ParamSpec::continuous("Disk Outer", 5.0, 20.0, 10.0, "F1", "diskOuter"),
            ParamSpec::continuous("Disk Glow", 0.5, 5.0, 2.0, "F1", "diskGlow"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::continuous("Stars", 0.0, 2.0, 0.5, "F2", "stars"),
            ParamSpec::continuous("Spin", -1.0, 1.0, 0.0, "F2", "spin"),
            ParamSpec::continuous("Particles", 0.0, 1.0, 0.0, "F2", "particles"),
            ParamSpec::continuous("Turbulence", 0.0, 5.0, 0.5, "F2", "turbulence"),
            ParamSpec::continuous("Cam Velocity", 0.0, 0.99, 0.0, "F2", "camVelocity"),
            ParamSpec::continuous("Freefall", 0.0, 1.0, 0.0, "F0", "freefall"),
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
            ParamSpec::continuous("Feedback", 0.5, 1.0, 0.98, "F2", "feedback"),
            ParamSpec::continuous("Noise Scale", 0.1, 2.0, 0.75, "F2", "noiseScale"),
            ParamSpec::continuous("Noise Speed", 0.01, 1.0, 0.1, "F3", "noiseSpeed"),
            ParamSpec::continuous("Edge Str", 0.5, 20.0, 5.0, "F1", "edgeStr"),
            ParamSpec::continuous("Mirror", 0.0, 90.0, 45.0, "F0", "mirror"),
            ParamSpec::continuous("Displace", 0.0, 0.5, 0.2, "F3", "displace"),
            ParamSpec::continuous("Roughness", 0.01, 1.0, 0.05, "F3", "roughness"),
            ParamSpec::continuous("Light Int", 0.1, 10.0, 3.5, "F1", "lightInt"),
            ParamSpec::continuous("Cam Dist", 0.5, 10.0, 2.5, "F2", "camDist"),
            ParamSpec::continuous("Cam Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),
            ParamSpec::continuous("Cam Tilt", -90.0, 90.0, -10.0, "F0", "camTilt"),
            ParamSpec::continuous("Cam FOV", 20.0, 120.0, 54.0, "F0", "camFov"),
            ParamSpec::continuous("Look Y", -2.0, 2.0, 0.0, "F2", "lookY"),
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
            ParamSpec::continuous("Speed", 0.1, 4.0, 1.0, "F2", "speed"),
            ParamSpec::continuous("Feedback", 0.95, 0.9999, 0.998, "F4", "feedback"),
            ParamSpec::continuous("Noise", 0.0, 0.02, 0.002, "F4", "noise"),
            ParamSpec::continuous("VelDamp", 0.85, 0.999, 0.98, "F3", "veldamp"),
            ParamSpec::continuous("Curl", 0.0, 1.0, 0.2, "F2", "curl"),
            ParamSpec::continuous("Relief", 0.05, 2.0, 0.5, "F2", "relief"),
            ParamSpec::continuous("Chroma", 0.0, 8.0, 2.0, "F2", "chroma"),
            ParamSpec::continuous("Contrast", 0.5, 3.0, 1.4, "F2", "contrast"),
            ParamSpec::continuous("Hue", 0.0, 1.0, 0.0, "F2", "hue"),
            ParamSpec::continuous("Sat", 0.0, 2.0, 1.0, "F2", "sat"),
            ParamSpec::continuous("Bright", 0.0, 2.0, 1.0, "F2", "bright"),
            ParamSpec::continuous("VelDisp", 0.1, 10.0, 1.0, "F2", "velDisp"),
            ParamSpec::continuous("ColDisp", 0.1, 10.0, 1.0, "F2", "colDisp"),
            ParamSpec::whole_labels("Mode", 0.0, 4.0, 0.0, &["Oil Slick", "Flow Field", "Height Map", "PBR", "Lines"], "mode"),
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
            ParamSpec::whole_labels("Slice Axis", 0.0, 2.0, 0.0, &["Axial", "Sagittal", "Coronal"], "sliceAxis"),
            ParamSpec::continuous("Slice Pos", 0.0, 1.0, 0.5, "F2", "slicePos"),
            ParamSpec::continuous("Center", 0.0, 1.0, 0.5, "F2", "center"),
            ParamSpec::continuous("Width", 0.01, 1.0, 0.8, "F2", "width"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("Invert", 0.0, 1.0, 0.0, "invert"),
            ParamSpec::continuous("Sharpen", 0.0, 3.0, 1.0, "F1", "sharpen"),
            ParamSpec::whole_labels("Scan", 0.0, 2.0, 0.0, &["250µm 7T", "300µm HiRes", "Edlow 100µm"], "scan"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 0.0, "snap"),
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
            ParamSpec::continuous("Density", 0.0, 1.0, 0.5, "F2", "density"),
            ParamSpec::continuous("Brightness", 0.0, 2.0, 0.7, "F2", "brightness"),
            ParamSpec::continuous("Depth", 0.0, 1.0, 0.5, "F2", "depth"),
            ParamSpec::continuous("Drift Speed", 0.0, 1.0, 0.15, "F2", "driftSpeed"),
            ParamSpec::continuous("Drift X", -1.0, 1.0, 0.3, "F2", "driftX"),
            ParamSpec::continuous("Drift Y", -1.0, 1.0, 0.1, "F2", "driftY"),
            ParamSpec::continuous("Twinkle", 0.0, 1.0, 0.3, "F2", "twinkle"),
            ParamSpec::continuous("Warmth", -1.0, 1.0, 0.0, "F2", "warmth"),
            ParamSpec::continuous("Nebula", 0.0, 1.0, 0.2, "F2", "nebula"),
            ParamSpec::continuous("Glow", 0.0, 1.0, 0.3, "F2", "glow"),
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
            ParamSpec::continuous("Size", 0.02, 1.0, 0.25, "F2", "size"),
            ParamSpec::continuous("Position X", -1.0, 1.0, 0.0, "F2", "posX"),
            ParamSpec::continuous("Position Y", -1.0, 1.0, 0.0, "F2", "posY"),
            ParamSpec::continuous("Scale", 0.1, 5.0, 1.0, "F2", "scale"),
        ],
        string_params: &[("Text", "text", "HELLO")],
    }
}
