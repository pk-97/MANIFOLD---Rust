fn main() {
    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("native/MetalEncoderPlugin.m")
            .flag("-fobjc-arc")
            .compile("metal_encoder");

        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
    }
}
