//! Pixel picking (SCENE_DOCUMENT_DESIGN.md section 7.14): which render slot
//! a scene draws at one point of its picture. The scene node draws its item
//! ids around the point through the camera the frame rendered with, and
//! reads the answer back on its next render. Editing chrome only: the output
//! never changes (REALTIME_3D_DESIGN.md D9).

/// One pick: a point of the picture, as a fraction of its width and height
/// from the top left, and the id the answer carries back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickRequest {
    pub id: u64,
    pub uv: [f32; 2],
}

/// The answer to [`PickRequest`] `id`: the render slot drawn nearest the
/// point, or `None` when nothing is drawn there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickHit {
    pub id: u64,
    pub slot: Option<u32>,
}

/// The pick region's side in render pixels. A small square, as Blender
/// picks, so a thin or tiny item is still hit near the pointer; the drawn
/// item nearest the centre wins.
pub const PICK_REGION: u32 = 7;

/// The render slot nearest the centre of a [`PICK_REGION`]-square of item
/// ids, row by row from the top, each slot plus one (0 is nothing drawn).
pub fn nearest_slot(ids: &[f32]) -> Option<u32> {
    let n = PICK_REGION as i64;
    let centre = n / 2;
    ids.iter()
        .take((n * n) as usize)
        .enumerate()
        .filter(|(_, id)| id.is_finite() && **id >= 0.5)
        .min_by_key(|(at, _)| {
            let (x, y) = (*at as i64 % n - centre, *at as i64 / n - centre);
            (x * x + y * y, *at)
        })
        .map(|(_, id)| id.round() as u32 - 1)
}

/// `view_proj` narrowed so the [`PICK_REGION`]-square of render pixels
/// centred on `uv` fills the whole target: x and y scale by the render size
/// over the region, after moving the point to the centre (OpenGL's pick
/// matrix). Column-major, like every scene matrix.
pub fn pick_view_proj(
    view_proj: [[f32; 4]; 4],
    uv: [f32; 2],
    width: u32,
    height: u32,
) -> [[f32; 4]; 4] {
    let n = PICK_REGION as f32;
    let (sx, sy) = (width.max(1) as f32 / n, height.max(1) as f32 / n);
    // The point in normalized device coordinates; y points up there.
    let (cx, cy) = (2.0 * uv[0] - 1.0, 1.0 - 2.0 * uv[1]);
    view_proj.map(|col| {
        [
            sx * (col[0] - cx * col[3]),
            sy * (col[1] - cy * col[3]),
            col[2],
            col[3],
        ]
    })
}
