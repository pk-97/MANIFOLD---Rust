//! Picks the backend: Vulkan with the `vulkan` feature or on any target but
//! macOS, Metal otherwise. The choice is the `vulkan_backend` cfg.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(vulkan_backend)");
    let feature = std::env::var_os("CARGO_FEATURE_VULKAN").is_some();
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if feature || !macos {
        println!("cargo::rustc-cfg=vulkan_backend");
    }
}
