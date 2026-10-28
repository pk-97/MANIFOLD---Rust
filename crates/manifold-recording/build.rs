fn main() {
    // Build scripts run on the host, so `cfg(target_os)` here would describe
    // the build machine; the target comes from Cargo's environment.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rerun-if-changed=native/LiveRecordingPlugin.m");

    cc::Build::new()
        .file("native/LiveRecordingPlugin.m")
        .flag("-fobjc-arc")
        .compile("live_recording");

    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=VideoToolbox");
    println!("cargo:rustc-link-lib=framework=AudioToolbox");
}
