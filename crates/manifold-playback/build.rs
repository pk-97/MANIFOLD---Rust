// Compiles the Signalsmith Stretch C ABI wrapper (Audio Layer warp, §4.1).
// The library is a vendored single-header C++ project under vendor/signalsmith/;
// the wrapper exposes one `extern "C"` time-stretch entry point. C++17 is
// required by the Signalsmith headers.
fn main() {
    println!("cargo:rerun-if-changed=native/signalsmith_stretch.cpp");
    println!("cargo:rerun-if-changed=vendor/signalsmith/signalsmith-stretch.h");
    println!("cargo:rerun-if-changed=vendor/signalsmith/signalsmith-linear/stft.h");
    println!("cargo:rerun-if-changed=vendor/signalsmith/signalsmith-linear/fft.h");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include("vendor/signalsmith")
        .file("native/signalsmith_stretch.cpp")
        .compile("manifold_signalsmith");
}
