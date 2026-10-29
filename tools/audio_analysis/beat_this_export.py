#!/usr/bin/env python3
"""Converts a Beat This! checkpoint (Foscarin et al., ISMIR 2024, MIT) into the model container the Rust beat
tracker loads, and writes the reference fixture its tests compare against.

    pip install beat-this soxr
    beat_this_export.py final0.ckpt

The checkpoint comes from https://cloud.cp.jku.at/index.php/s/7ik4RrBKTS273gp. Writes
crates/manifold-audio/assets/beat_this.mkick: every weight matrix in half precision, the norms, biases and the shared
rotary frequencies in f32, under the PyTorch names without the `model.` prefix. The fixture
crates/manifold-audio/tests/fixtures/beats/excerpt.mkick holds a short real mix at 22050 Hz, its log-mel spectrogram, and the network's beat
and downbeat logits for that spectrogram, computed with the same half-precision weights the app ships.
"""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import soundfile as sf
import soxr
import torch

from beat_this.inference import load_model
from beat_this.preprocessing import LogMelSpect

sys.path.insert(0, str(Path(__file__).resolve().parent))
import kick_container  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
MODEL = ROOT / 'crates/manifold-audio/assets/beat_this.mkick'
FIXTURE = ROOT / 'crates/manifold-audio/tests/fixtures/beats/excerpt.mkick'
EXCERPT = ROOT / 'tests/fixtures/audio/apricots_128bpm/mix.wav'
EXCERPT_START_S, EXCERPT_LEN_S = 3.0, 6.0


def main(ckpt):
    model = load_model(ckpt, 'cpu')
    entries = {'source': kick_container.text(f'beat_this {Path(ckpt).stem}')}
    for name, t in model.state_dict().items():
        if name.endswith('num_batches_tracked') or name.endswith('rotary_embed.freqs'):
            continue
        a = t.detach().numpy()
        entries[name] = a.astype(np.float16 if a.ndim >= 2 else np.float32)
    # One rotary module is shared by every attention layer.
    entries['rotary.freqs'] = model.frontend.blocks[0].partial.attnF.rotary_embed.freqs.detach().numpy()
    kick_container.write(MODEL, entries)

    # The reference runs the weights the app ships, so the fixture checks the Rust maths, not the rounding.
    with torch.no_grad():
        for t in model.parameters():
            if t.ndim >= 2:
                t.copy_(t.half().float())
    audio, rate = sf.read(EXCERPT, dtype='float64')
    mono = audio.mean(1)[int(EXCERPT_START_S * rate):int((EXCERPT_START_S + EXCERPT_LEN_S) * rate)]
    signal = soxr.resample(mono, rate, 22050).astype(np.float32)
    with torch.no_grad():
        mel = LogMelSpect()(torch.from_numpy(signal))
        out = model(mel.unsqueeze(0))
    FIXTURE.parent.mkdir(parents=True, exist_ok=True)
    kick_container.write(FIXTURE, {
        'signal': signal,
        'mel': mel.numpy().astype(np.float32),
        'beat': out['beat'][0].numpy().astype(np.float32),
        'downbeat': out['downbeat'][0].numpy().astype(np.float32),
    })
    print(f'{MODEL}: {MODEL.stat().st_size / 1e6:.1f} MB; {FIXTURE}: {len(mel)} frames')


if __name__ == '__main__':
    main(sys.argv[1])
