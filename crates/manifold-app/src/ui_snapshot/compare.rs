//! `--vs-mockup`: render the HTML mockup via a headless Chromium browser and
//! composite it next to the app render, so the app and the design target sit
//! side by side in one image. No pass/fail — a human-judged comparison (kept
//! separate from any app-vs-app check, per the doc's honesty rail).
//! See `docs/HEADLESS_UI_HARNESS.md` §5.

use std::path::Path;

/// Headless Chromium-family browsers, in preference order.
const BROWSERS: &[&str] = &[
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

/// The mockup HTML, pinned in the repo (the scratchpad copy is session-specific).
const MOCKUP_HTML: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/timeline-mockup.html");

/// Render the mockup, then write `<scene>.vs-mockup.png` = app | mockup.
pub fn vs_mockup(dir: &Path, scene: &str, app_png: &Path) {
    let mock_png = dir.join("mockup.png");
    if !render_mockup(&mock_png) {
        return;
    }
    let out = dir.join(format!("{scene}.vs-mockup.png"));
    side_by_side(app_png, &mock_png, &out);
    println!("ui-snap: wrote {}", out.display());
}

fn render_mockup(out_png: &Path) -> bool {
    let Some(browser) = BROWSERS.iter().copied().find(|p| Path::new(p).exists()) else {
        eprintln!("ui-snap: no headless browser found; skipping --vs-mockup");
        return false;
    };
    let status = std::process::Command::new(browser)
        .args([
            "--headless",
            "--no-sandbox",
            "--disable-gpu",
            "--hide-scrollbars",
            "--force-device-scale-factor=2",
            "--window-size=1240,1000",
        ])
        .arg(format!("--screenshot={}", out_png.display()))
        .arg(format!("file://{MOCKUP_HTML}"))
        .status();
    match status {
        Ok(s) if s.success() && out_png.exists() => true,
        _ => {
            eprintln!("ui-snap: mockup render failed via {browser}");
            false
        }
    }
}

fn side_by_side(app_png: &Path, mock_png: &Path, out: &Path) {
    let app = image::open(app_png).expect("open app png").to_rgba8();
    let mock = image::open(mock_png).expect("open mockup png").to_rgba8();
    let gap: u32 = 24;
    let w = app.width() + gap + mock.width();
    let h = app.height().max(mock.height());
    let mut canvas = image::RgbaImage::from_pixel(w, h, image::Rgba([16, 16, 18, 255]));
    image::imageops::overlay(&mut canvas, &app, 0, 0);
    image::imageops::overlay(&mut canvas, &mock, i64::from(app.width() + gap), 0);
    canvas.save(out).unwrap_or_else(|e| panic!("save {}: {e}", out.display()));
}
