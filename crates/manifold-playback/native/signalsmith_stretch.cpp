// C ABI wrapper around Signalsmith Stretch (MIT, vendored under
// vendor/signalsmith/) for the Audio Layer warp seam — see
// docs/AUDIO_LAYER_DESIGN.md §4.1.
//
// One job: time-stretch a whole decoded buffer WITHOUT changing pitch, offline
// (ahead of playback). The Rust side hands kira the warped samples at
// playback_rate = 1.0, so Signalsmith replaces the varispeed resample rather
// than fighting kira's rate. `SignalsmithStretch::exact()` does the whole-buffer
// stretch in one call (internal output-seek priming + flush tail), which is the
// recipe the library's own cmd/main.cpp documents.

#include "signalsmith-stretch.h"

#include <vector>

extern "C" {

// Time-stretch interleaved f32 audio.
//
//   in            interleaved input,  length in_frames  * channels
//   out           interleaved output, length out_frames * channels (caller-owned)
//   the stretch factor is out_frames / in_frames; pitch is preserved.
//
// Returns 1 on success, 0 if the inputs are invalid or the clip is too short
// for the configured block size (output is zeroed in that case).
int manifold_signalsmith_stretch(
    const float* in, int in_frames,
    float* out, int out_frames,
    int channels, float sample_rate) {
    if (in == nullptr || out == nullptr || channels <= 0 || in_frames <= 0 ||
        out_frames <= 0 || sample_rate <= 0.0f) {
        return 0;
    }

    // Deinterleave into planar per-channel buffers — Signalsmith's IO wants
    // `io[channel][sample]`. Output gets its own planar buffers we re-interleave.
    std::vector<std::vector<float>> in_planar(channels, std::vector<float>(in_frames));
    std::vector<std::vector<float>> out_planar(channels, std::vector<float>(out_frames));
    for (int f = 0; f < in_frames; ++f) {
        for (int c = 0; c < channels; ++c) {
            in_planar[c][f] = in[f * channels + c];
        }
    }

    // Minimal IO adaptor: operator[](channel) → float* indexable by sample.
    struct PlanarIO {
        std::vector<std::vector<float>>* p;
        float* operator[](int c) { return (*p)[c].data(); }
    };
    PlanarIO in_io{&in_planar};
    PlanarIO out_io{&out_planar};

    signalsmith::stretch::SignalsmithStretch<float> stretch;
    stretch.presetDefault(channels, sample_rate);
    // Default transpose factor is 1 (no pitch shift) — exactly what warp wants.

    bool ok = stretch.exact(in_io, in_frames, out_io, out_frames);
    if (!ok) {
        for (int f = 0; f < out_frames * channels; ++f) out[f] = 0.0f;
        return 0;
    }

    for (int f = 0; f < out_frames; ++f) {
        for (int c = 0; c < channels; ++c) {
            out[f * channels + c] = out_planar[c][f];
        }
    }
    return 1;
}

} // extern "C"
