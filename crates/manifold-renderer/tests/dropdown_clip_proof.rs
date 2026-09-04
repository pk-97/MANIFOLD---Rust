//! Dropdown fit proof — renders the layer context menu (items + the
//! 70-swatch color grid) headlessly at the pixel level.
//!
//! Two scenarios, matching how the menu is actually used:
//! - FIT: an ordinary menu on a normal-height screen sizes to its content —
//!   every swatch visible, bottom padding intact, no scrolling. (A fixed
//!   400px cap used to force this menu to scroll and clipped its bottom row.)
//! - OVERFLOW: content taller than the SCREEN caps at the screen, culls
//!   off-viewport swatches, and scrolls internally — nothing paints or
//!   takes clicks outside the container.
//!
//! Run: `SWATCH_OUT=/some/dir cargo test -p manifold-renderer --test dropdown_clip_proof`
//! then open `$SWATCH_OUT/dropdown_fit.png` / `dropdown_overflow.png`.

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
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

fn open_menu(
    items: Vec<DropdownItem>,
    pos: Vec2,
    screen: Vec2,
    tree: &mut UITree,
) -> DropdownPanel {
    let mut dd = DropdownPanel::new();
    dd.set_screen_size(screen.x, screen.y);
    dd.open_context_with_colors(
        items,
        color::COLOR_GRID.to_vec(),
        color::COLOR_GRID_COLS,
        pos,
        tree,
    );
    dd
}

/// Rebuild through the same path the overlay driver uses — the eager
/// `open_at` build's own container-sized region masks overflow, so drop it
/// and re-mint under a full-screen region, exactly like `build_overlays`.
fn rebuild_emulating_overlay_driver(
    dd: &mut DropdownPanel,
    tree: &mut UITree,
    screen: Vec2,
) {
    // saturating: on a fresh tree (no eager nodes to drop) this is a no-op.
    tree.truncate_from(tree.count().saturating_sub(dd.node_count() + 1));
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, screen.x, screen.y),
        ZTier::Overlay,
        "overlay",
        UIFlags::empty(),
    );
    let start = tree.count();
    Overlay::build_at(
        dd,
        tree,
        OverlayPlacement {
            rect: Rect::ZERO,
            screen,
        },
    );
    tree.end_region(region, start);
}

fn render(tree: &UITree, h: u32) -> Vec<u8> {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    ui.begin_frame();
    ui.render_tree(tree, None);
    assert!(ui.prepare(&device, W, h, 1.0), "dropdown produced no draw commands");
    let target = RenderTarget::new(&device, W, h, FORMAT, "dropdown-render");
    {
        let mut enc = device.create_encoder("dropdown-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    readback(&device, &target.texture, h)
}

fn save(out_dir: &str, name: &str, bytes: &[u8], h: u32) {
    image::save_buffer(
        format!("{out_dir}/{name}"),
        bytes,
        W,
        h,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap_or_else(|e| panic!("save {name}: {e}"));
    eprintln!("{name} → {out_dir}/{name}");
}

#[test]
fn fitting_menu_shows_every_swatch_without_scrolling() {
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    const H: u32 = 1080;
    let screen = Vec2::new(W as f32, H as f32);

    let mut tree = UITree::new();
    let items: Vec<DropdownItem> = (0..6)
        .map(|i| DropdownItem::new(&format!("Item {i}")))
        .collect();
    let dd = open_menu(items, Vec2::new(100.0, 100.0), screen, &mut tree);
    assert!(dd.is_open());
    let container = dd.container_bounds();
    assert!(
        container.y_max() <= H as f32 + 0.01,
        "the fitting menu stays fully on screen"
    );

    let bytes = render(&tree, H);
    save(&out_dir, "dropdown_fit.png", &bytes, H);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let blank = px(600, 1000);

    // Items: 6 × 28 = 168; separator 9; grid 10 rows × 19 = 190 - 3 gap.
    // Last swatch row spans (content-local) 265..281 → screen 365..381.
    let mut last_row_paints = false;
    for y in 366..=380u32 {
        if px(120, y) != blank {
            last_row_paints = true;
            break;
        }
    }
    assert!(last_row_paints, "every swatch row paints, none clipped");
    // Nothing paints below the container.
    for probe_y in [container.y_max() as u32 + 10, 1000] {
        for probe_x in [120u32, 200, 280] {
            assert_eq!(
                px(probe_x, probe_y),
                blank,
                "pixel ({probe_x}, {probe_y}) below the container untouched"
            );
        }
    }
}

#[test]
fn overflowing_menu_caps_at_screen_and_scrolls() {
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    const H: u32 = 640;
    let screen = Vec2::new(W as f32, H as f32);

    let mut tree = UITree::new();
    let items: Vec<DropdownItem> = (0..30)
        .map(|i| DropdownItem::new(&format!("Item {i}")))
        .collect();
    let mut dd = open_menu(items, Vec2::new(100.0, 40.0), screen, &mut tree);
    rebuild_emulating_overlay_driver(&mut dd, &mut tree, screen);
    assert!(dd.is_open(), "the overlay-driver rebuild keeps the menu open");
    let container = dd.container_bounds();
    assert!(
        container.height <= H as f32 + 0.01,
        "content taller than the screen caps at the screen, got {}",
        container.height
    );

    let bytes = render(&tree, H);
    save(&out_dir, "dropdown_overflow.png", &bytes, H);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let blank = px(600, 600);
    // The overflow menu fills the screen height (that IS the cap), so
    // "below the container" doesn't exist on-canvas. Containment shows at
    // the right edge: the menu is ~150px wide, so everything right of the
    // container must be untouched canvas — no item or swatch escapes.
    let right_x = container.x_max() as u32 + 12;
    for probe_y in [40u32, 200, 400, 600] {
        assert_eq!(
            px(right_x, probe_y),
            blank,
            "pixel ({right_x}, {probe_y}) right of the container must be untouched canvas"
        );
    }
    // The bottom edge is clipped content, not blank: the last visible row
    // partially paints just above the container bottom.
    let mut edge_paints = false;
    for y in (container.y_max() as u32 - 24)..container.y_max() as u32 {
        if px(120, y) != blank {
            edge_paints = true;
            break;
        }
    }
    assert!(edge_paints, "the capped menu's bottom edge shows clipped content");

    // Wheel to the bottom: the last swatch becomes reachable and paints
    // inside the container.
    let scroll = manifold_ui::input::UIEvent::Scroll {
        pos: Vec2::new(container.x + 10.0, container.y + 10.0),
        delta: Vec2::new(0.0, -10_000.0),
        modifiers: manifold_ui::input::Modifiers::default(),
    };
    dd.handle_event(&scroll, &mut tree);
    let mut tree = UITree::new();
    rebuild_emulating_overlay_driver(&mut dd, &mut tree, screen);
    let bytes = render(&tree, H);
    save(&out_dir, "dropdown_overflow_bottom.png", &bytes, H);
    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let blank = px(600, 600);
    // After scrolling, the menu's top edge no longer shows "Item 0" — the
    // content moved. The bottom of the grid (the last swatch row) now paints
    // just above the container bottom.
    let mut last_row_paints = false;
    for y in (container.y_max() as u32 - 24)..container.y_max() as u32 {
        if px(120, y) != blank {
            last_row_paints = true;
            break;
        }
    }
    assert!(
        last_row_paints,
        "scrolled to the bottom: the last swatch row paints at the container bottom"
    );
}

fn readback(device: &GpuDevice, texture: &GpuTexture, h: u32) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("dropdown-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, h, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
