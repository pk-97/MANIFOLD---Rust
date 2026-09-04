//! Dropdown containment proof — renders the real layer context menu (9 items +
//! the 70-swatch color grid, content taller than MAX_DROPDOWN_HEIGHT) headlessly
//! and asserts at the pixel level that nothing paints outside the container.
//!
//! Regression for the modal-overflow class: the color grid used to lay out below
//! the height-capped container, painting swatches past the popup's bottom edge.
//! The fix is structural — the `popup_shell` container clips children
//! (`CLIPS_CHILDREN`) and the dropdown culls off-viewport swatches — so this
//! test probes pixels where the old code painted a swatch.
//!
//! Run: `SWATCH_OUT=/some/dir cargo test -p manifold-renderer --test dropdown_clip_proof`
//! then open `$SWATCH_OUT/dropdown_clip.png`.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::color;
use manifold_ui::node::{UIFlags, Vec2};
use manifold_ui::panels::dropdown::{DropdownItem, DropdownPanel};
use manifold_ui::panels::overlay::{Overlay, OverlayPlacement};
use manifold_ui::{Rect, UITree, ZTier};

// W*4 must be 256-byte aligned for the texture→buffer readback copy.
// 640*4 = 2560 = 10*256. H is unconstrained.
const W: u32 = 640;
const H: u32 = 640;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

#[test]
fn color_grid_never_paints_outside_the_container() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/dropdown_clip.png");

    let mut tree = UITree::new();
    let mut dd = DropdownPanel::new();
    dd.set_screen_size(W as f32, H as f32);
    let items: Vec<DropdownItem> = (0..9)
        .map(|i| DropdownItem::new(&format!("Item {i}")))
        .collect();
    dd.open_context_with_colors(
        items,
        color::COLOR_GRID.to_vec(),
        color::COLOR_GRID_COLS,
        Vec2::new(100.0, 100.0),
        &mut tree,
    );
    let container = dd.container_bounds();
    assert!(
        container.height <= 400.0 + 0.01,
        "container is height-capped: {:?}",
        container
    );

    // The eager `open_at` build wraps the dropdown in its own container-sized
    // region, which clips overflow and MASKS the bug. The app replaces those
    // nodes on the next `build_overlays` cycle with a full-screen region and
    // no container-level clip — that rebuilt state is what the user actually
    // sees, so emulate it here: drop the eager nodes and rebuild exactly like
    // `UIRoot::build_overlays` does.
    // (region root is the one node before the dropdown's own range).
    let overlay_start = tree.count();
    tree.truncate_from(overlay_start - dd.node_count() - 1);
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Overlay,
        "overlay",
        UIFlags::empty(),
    );
    let start = tree.count();
    Overlay::build_at(
        &mut dd,
        &mut tree,
        OverlayPlacement {
            rect: Rect::ZERO,
            screen: Vec2::new(W as f32, H as f32),
        },
    );
    tree.end_region(region, start);
    assert!(
        dd.is_open(),
        "the overlay-driver rebuild path must not close the dropdown"
    );

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_1);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "dropdown produced no draw commands");

    let target = RenderTarget::new(&device, W, H, FORMAT, "dropdown-clip");
    {
        let mut enc = device.create_encoder("dropdown-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("dropdown clip proof → {png}");

    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };

    // Below the container, in the swatch column's x-range: the old layout
    // painted swatch rows here after the overlay-driver rebuild (no
    // container-level clip, no culling). The canvas there must be untouched —
    // identical to a far corner nothing reaches.
    let blank = px(600, 600);
    let below_y = container.y_max() as u32 + 20;
    for probe_x in [120u32, 200, 280] {
        assert_eq!(
            px(probe_x, below_y),
            blank,
            "pixel ({probe_x}, {below_y}) below the container must be untouched canvas"
        );
    }

    // Inside the container, the first swatch of the grid is visibly its grid
    // color (probe at swatch center, clear of the 1px border; the unorm
    // round-trip can be off by one, so compare against blank canvas).
    let grid_top = 100.0 + 4.0 + 9.0 * 28.0 + 9.0; // y + pad + items + separator
    let sx = 100.0 + 12.0; // x + PADDING_H
    assert_ne!(
        px((sx + 8.0) as u32, (grid_top + 8.0) as u32),
        blank,
        "first swatch paints inside the container"
    );
}

fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("dropdown-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
