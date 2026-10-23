//! Ocean: an Object whose Shape is Ocean (SCENE_DOCUMENT_DESIGN.md section
//! 7.10 (Water, Water Source, Ocean)). Three wave cascades (swell, chop,
//! ripple: `node.ocean_spectrum` into `node.inverse_fft_2d`) displace a grid
//! cast from the active camera onto the sea plane (`node.projected_grid`,
//! `node.ocean_displace`, `node.make_triangles`).
//!
//! The dials are the swell cascade's wind, size and speed, the displacement's
//! Choppiness and Foam, all performance rows. The chop and ripple cascades
//! follow the swell's dials (`Builder::follow`), so one knob moves the whole
//! sea. Everything else is fixed setup.

use manifold_foundation::scene_surface::{CardBand, ControlGroup};

use super::{Exposure, PropertyRow, Reach, perform, row};
use crate::scene_doc::{ItemFields, Shape};

const S: ControlGroup = ControlGroup::Shape;

fn ocean(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Ocean)
}

const fn fixed(property: &'static str, role: &'static str, param: &'static str, why: &'static str) -> PropertyRow {
    row(property, Reach::Own(role), S, Exposure::Internal(why)).param(param).when(ocean)
}

const fn auto(property: &'static str, role: &'static str, param: &'static str, why: &'static str) -> PropertyRow {
    row(property, Reach::Own(role), S, Exposure::Automatic(why)).param(param).when(ocean)
}

const CASCADE: &str = "the starter sea's cascade split: three tiles that never double-count a wave";
const FOLLOWS: &str = "follows the swell cascade's dial";
const CLOCK: &str = "the playback clock drives the waves";
const FFT: &str = "the cascade's field size, matched to its spectrum";

/// One wave cascade's spectrum: its tile, the band of wavenumbers it holds
/// and its seed are fixed; wind, wave size and speed are the dials on the
/// swell and follow it on the others.
macro_rules! cascade {
    (
        $role:literal, $p:literal, tile: $tile:expr, band: ($low:expr, $high:expr), seed: $seed:expr,
        dials: [$wind:expr, $dir:expr, $size:expr, $speed:expr]
    ) => {
        [
            fixed(concat!($p, "_size"), $role, "size", FFT),
            fixed(concat!($p, "_tile_size"), $role, "tile_size", CASCADE).default($tile),
            fixed(concat!($p, "_band_low"), $role, "band_low", CASCADE).default($low),
            fixed(concat!($p, "_band_high"), $role, "band_high", CASCADE).default($high),
            fixed(concat!($p, "_fetch"), $role, "fetch_km", "the starter sea's open-water fetch").default(60.0),
            fixed(concat!($p, "_seed"), $role, "seed", "each cascade has its own random phases").default($seed),
            auto(concat!($p, "_time"), $role, "time", CLOCK),
            $wind,
            $dir,
            $size,
            $speed,
        ]
    };
}

const fn dial(property: &'static str, param: &'static str, order: u8) -> PropertyRow {
    row(property, Reach::Own("swell_spectrum"), S, perform(CardBand::Scene, order)).param(param).when(ocean)
}

const fn follows(property: &'static str, role: &'static str, param: &'static str) -> PropertyRow {
    auto(property, role, param, FOLLOWS)
}

static SWELL: [PropertyRow; 11] = cascade!(
    "swell_spectrum", "swell", tile: 1000.0, band: (0.0, 0.225_743_34), seed: 1.0,
    dials: [
        dial("wind_speed", "wind_speed", 0).range(1.0, 30.0),
        dial("wind_direction", "wind_direction", 1).range(0.0, 360.0).default(205.0),
        dial("wave_size", "wave_size", 3).range(0.0, 3.0),
        dial("swell_speed", "swell_speed", 4)
    ]
);

static CHOP: [PropertyRow; 11] = cascade!(
    "chop_spectrum", "chop", tile: 167.0, band: (0.225_743_34, 1.396_263_4), seed: 2.0,
    dials: [
        follows("chop_wind_speed", "chop_spectrum", "wind_speed"),
        follows("chop_wind_direction", "chop_spectrum", "wind_direction"),
        follows("chop_wave_size", "chop_spectrum", "wave_size"),
        follows("chop_swell_speed", "chop_spectrum", "swell_speed")
    ]
);

// A band top of 0 holds every wavenumber above the band's bottom.
static RIPPLE: [PropertyRow; 11] = cascade!(
    "ripple_spectrum", "ripple", tile: 27.0, band: (1.396_263_4, 0.0), seed: 3.0,
    dials: [
        follows("ripple_wind_speed", "ripple_spectrum", "wind_speed"),
        follows("ripple_wind_direction", "ripple_spectrum", "wind_direction"),
        follows("ripple_wave_size", "ripple_spectrum", "wave_size"),
        follows("ripple_swell_speed", "ripple_spectrum", "swell_speed")
    ]
);

static FIELDS: [PropertyRow; 6] = [
    fixed("swell_field_size", "swell_field", "size", FFT),
    fixed("swell_field_batch", "swell_field", "batch", "a spectrum is six fields"),
    fixed("chop_field_size", "chop_field", "size", FFT),
    fixed("chop_field_batch", "chop_field", "batch", "a spectrum is six fields"),
    fixed("ripple_field_size", "ripple_field", "size", FFT),
    fixed("ripple_field_batch", "ripple_field", "batch", "a spectrum is six fields"),
];

const GRID: &str = "the grid is cast from the camera at a fixed density";
const FROM_CAMERA: &str = "the layer's output size sets the grid's aspect";
const TILES: &str = "tiles match their cascade's spectrum";
const FADE: &str = "the cascade fades out with distance at the starter sea's range";

static SURFACE: [PropertyRow; 27] = [
    fixed("grid_columns", "grid", "columns", GRID).default(640.0),
    fixed("grid_rows", "grid", "rows", GRID).default(360.0),
    fixed("grid_level", "grid", "level", "the Object's Position Y lifts the sea"),
    fixed("grid_max_distance", "grid", "max_distance", "the sea runs to the horizon"),
    fixed("grid_margin", "grid", "margin", GRID),
    fixed("water_color_r", "grid", "color_r", "the deep water colour; Material tints it").default(0.012),
    fixed("water_color_g", "grid", "color_g", "the deep water colour; Material tints it").default(0.034),
    fixed("water_color_b", "grid", "color_b", "the deep water colour; Material tints it").default(0.045),
    auto("grid_width", "grid", "width", FROM_CAMERA),
    auto("grid_height", "grid", "height", FROM_CAMERA),
    row("choppiness", Reach::Own("displace"), S, perform(CardBand::Scene, 2)).range(0.0, 2.5).default(1.8).when(ocean),
    row("foam", Reach::Own("displace"), S, perform(CardBand::Scene, 5))
        .param("foam_threshold")
        .name("Foam")
        .range(-0.5, 1.0)
        .default(0.45)
        .when(ocean),
    fixed("foam_softness", "displace", "foam_width", "the foam edge's softness is part of the look"),
    fixed("swell_displace_size", "displace", "size_0", FFT),
    fixed("swell_displace_tile", "displace", "tile_size_0", TILES),
    fixed("swell_fade_start", "displace", "fade_start_0", FADE),
    fixed("swell_fade_end", "displace", "fade_end_0", FADE),
    fixed("chop_displace_size", "displace", "size_1", FFT),
    fixed("chop_displace_tile", "displace", "tile_size_1", TILES),
    fixed("chop_fade_start", "displace", "fade_start_1", FADE),
    fixed("chop_fade_end", "displace", "fade_end_1", FADE),
    fixed("ripple_displace_size", "displace", "size_2", FFT),
    fixed("ripple_displace_tile", "displace", "tile_size_2", TILES),
    fixed("ripple_fade_start", "displace", "fade_start_2", FADE),
    fixed("ripple_fade_end", "displace", "fade_end_2", FADE),
    auto("triangle_columns", "triangles", "src_cols", "the grid's columns"),
    auto("triangle_rows", "triangles", "src_rows", "the grid's rows"),
];

/// The material an Ocean takes when its Shape is set:
/// white, so the grid's deep water colour shows untinted, with water's
/// index of refraction and a calm sea's gloss.
pub const OCEAN_LOOK: [(&str, f32); 7] = [
    ("color_r", 1.0),
    ("color_g", 1.0),
    ("color_b", 1.0),
    ("ambient", 0.0),
    ("metallic", 0.0),
    ("roughness", 0.22),
    ("ior", 1.333),
];

/// Every table an Ocean's rows come from.
pub static OCEAN_TABLES: [&[PropertyRow]; 5] = [&SWELL, &CHOP, &RIPPLE, &FIELDS, &SURFACE];
