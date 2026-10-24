//! Ocean (SCENE_DOCUMENT_DESIGN.md section 7.10 (Water, Water Source,
//! Ocean)): an Object whose Shape is Ocean draws the sea chain.
//! Three wave cascades (`node.ocean_spectrum` into `node.inverse_fft_2d`) feed
//! `node.ocean_displace`, which moves a grid cast from the active camera onto
//! the sea plane (`node.projected_grid`); `node.make_triangles` turns the grid
//! into the triangles the Object draws. The chain is the Ocean preset's, built
//! from the compiler.
//!
//! The grid follows the camera, so an Ocean with no camera in the scene draws
//! nothing. It has no rest pose (`Builder::drawn`) and no mesh source, so a
//! Modifier or Copies cannot reach it: the modifier frames read a static mesh
//! source (`load/expand/frames.rs`), which a surface made on the GPU each frame
//! is not.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::Item;

use super::builder::Builder;

/// The grid's density: columns by rows of cast points.
const COLUMNS: u32 = 640;
const ROWS: u32 = 360;

/// The swell cascade's dials the other two cascades follow.
const DIALS: [&str; 4] = ["wind_speed", "wind_direction", "wave_size", "swell_speed"];

/// The cascades, swell first: the role of each spectrum and of its field.
const CASCADES: [(&str, &str); 3] = [
    ("swell_spectrum", "swell_field"),
    ("chop_spectrum", "chop_field"),
    ("ripple_spectrum", "ripple_field"),
];

/// Emit the chain for `item`; the returned `node.make_triangles` is the
/// Ocean's vertices. `None` when the scene has no camera to cast the grid from.
pub(crate) fn emit_surface(b: &mut Builder<'_>, item: &Item) -> Option<u32> {
    let camera = b.active_lens?;
    let owner = item.id.as_str();
    let input = b.scene("input");
    let displace = b.add(owner, "displace", "node.ocean_displace");
    for (k, (spectrum_role, field_role)) in CASCADES.into_iter().enumerate() {
        let spectrum = b.add(owner, spectrum_role, "node.ocean_spectrum");
        let field = b.add(owner, field_role, "node.inverse_fft_2d");
        b.wire(spectrum, "spectrum", field, "spectrum");
        b.wire(field, "field", displace, &format!("field_{k}"));
    }
    let grid = b.add(owner, "grid", "node.projected_grid");
    b.wire(camera, "out", grid, "camera");
    b.wire(input, "output_width", grid, "width");
    b.wire(input, "output_height", grid, "height");
    b.wire(grid, "vertices", displace, "mesh");
    b.wire(camera, "out", displace, "camera");
    let triangles = b.add(owner, "triangles", "node.make_triangles");
    b.set(
        triangles,
        "src_cols",
        SerializedParamValue::Int {
            value: COLUMNS as i32,
        },
    );
    b.set(
        triangles,
        "src_rows",
        SerializedParamValue::Int { value: ROWS as i32 },
    );
    b.wire(displace, "out", triangles, "in");
    Some(triangles)
}

/// After the Ocean's card rows are bound: the chop and ripple cascades take
/// the swell's wind, direction, wave size and speed, so one dial moves the
/// whole sea.
pub(crate) fn follow_dials(b: &mut Builder<'_>, item: &Item) {
    let owner = item.id.as_str();
    let Some(swell) = b.id(owner, CASCADES[0].0) else {
        return;
    };
    for (spectrum_role, _) in &CASCADES[1..] {
        let Some(spectrum) = b.id(owner, spectrum_role) else {
            continue;
        };
        for dial in DIALS {
            b.follow(swell, dial, spectrum, dial);
        }
    }
}
