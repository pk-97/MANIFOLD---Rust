fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=native/MetalEncoderPlugin.m");
        println!("cargo:rerun-if-changed=native/MetalVideoDecoderPlugin.m");

        cc::Build::new()
            .file("native/MetalEncoderPlugin.m")
            .flag("-fobjc-arc")
            .compile("metal_encoder");

        cc::Build::new()
            .file("native/MetalVideoDecoderPlugin.m")
            .flag("-fobjc-arc")
            .compile("metal_decoder");

        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        println!("cargo:rustc-link-lib=framework=VideoToolbox");
    }
}
