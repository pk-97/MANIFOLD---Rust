# MTL4FX Temporal Denoised Scaler — minimal creation repro

Tests whether creating an `MTL4FXTemporalDenoisedScaler` via the MTL4 compiler
path crashes on this machine, with zero Manifold code — pure ObjC calling only
Apple frameworks. All MTL4 denoiser APIs are private SPI (not in public SDK
headers); these reach them via NSClassFromString.

## Build and run

```sh
cd tools/mtl4fx_repro

# MTL4 compiler path (private)
clang -framework Metal -framework MetalFX -framework Foundation main.m -o repro
./repro

# Classic path (control)
clang -framework Metal -framework MetalFX -framework Foundation classic.m -o classic
./classic
```

## Result (2026-08-11, macOS 26.6.1, M4 Max)

- **MTL4 path (main.m):** SIGABRT in MPSGraphExecutable.mm:3467 —
  "Incompatible shape for parameter at index 0". Identical to our Rust crash.
- **Classic path (classic.m):** succeeds — same descriptor, no MTL4 compiler.

## Verdict

**Apple defect.** The MTL4-compiler-based denoiser creation path is broken on
macOS 26.6.1 (Tahoe) with M4 Max. The crash reproduces identically outside
Manifold, in a pure ObjC CLI with no bindings layer. Our BUG-woji hard-off
is correct.

The entire MTL4 denoiser API surface is private SPI — none of MTL4Compiler,
MTL4CompilerDescriptor, MTLFXTemporalDenoisedScalerDescriptor, or
MTL4FXTemporalDenoisedScaler appear in the public MetalFX.framework headers
shipped with Xcode. The classic (public) MTLFX denoised scaler path works.
