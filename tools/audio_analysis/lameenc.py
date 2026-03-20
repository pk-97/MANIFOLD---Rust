"""
Minimal compatibility shim for environments where `lameenc` is unavailable.

Demucs imports `lameenc` unconditionally, even when exporting WAV stems.
This shim keeps the import path alive for WAV-only workflows and fails fast
if MP3 encoding is actually requested.
"""


class Encoder:
    def __init__(self):
        self._unsupported()

    def set_bit_rate(self, *_args, **_kwargs):
        self._unsupported()

    def set_in_sample_rate(self, *_args, **_kwargs):
        self._unsupported()

    def set_channels(self, *_args, **_kwargs):
        self._unsupported()

    def set_quality(self, *_args, **_kwargs):
        self._unsupported()

    def encode(self, *_args, **_kwargs):
        self._unsupported()

    def flush(self):
        self._unsupported()

    @staticmethod
    def _unsupported():
        raise RuntimeError(
            "lameenc is not available in this environment. "
            "MP3 encoding paths are unsupported; use WAV output."
        )
