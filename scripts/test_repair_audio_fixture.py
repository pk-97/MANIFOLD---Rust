import hashlib
import json
import struct
import tempfile
import unittest
import wave
from pathlib import Path

from repair_audio_fixture import RepairError, reconstruct


def write_wav(path: Path, *, rate: int, channels: int, width: int, samples: bytes) -> None:
    with wave.open(str(path), "wb") as stream:
        stream.setnchannels(channels)
        stream.setsampwidth(width)
        stream.setframerate(rate)
        stream.writeframes(samples)


class RepairAudioFixtureTests(unittest.TestCase):
    def make_fixture(self, mix_samples: bytes = b"\x00" * 12, *, rate: int = 44_100, frames: int = 2) -> tuple[Path, Path]:
        root = Path(tempfile.mkdtemp())
        fixture = root / "fixture"
        backup = root / "backup"
        fixture.mkdir()
        write_wav(fixture / "mix.wav", rate=48_000, channels=2, width=3, samples=mix_samples)
        stem_values = ((10, -20), (30, 40), (-5, 6), (1, 2))
        stem_samples = b"".join(struct.pack("<hh", *pair) for pair in stem_values[:frames])
        for name in ("bass", "drums", "others", "vocals"):
            write_wav(fixture / f"{name}.wav", rate=rate, channels=2, width=2, samples=stem_samples)
        return fixture, backup

    def make_custom_fixture(self, stems: dict[str, list[tuple[int, int]]]) -> tuple[Path, Path]:
        root = Path(tempfile.mkdtemp())
        fixture = root / "fixture"
        backup = root / "backup"
        fixture.mkdir()
        frame_count = len(stems["bass"])
        write_wav(fixture / "mix.wav", rate=48_000, channels=2, width=3, samples=b"\x00" * frame_count * 6)
        for name, values in stems.items():
            samples = b"".join(struct.pack("<hh", *pair) for pair in values)
            write_wav(fixture / f"{name}.wav", rate=44_100, channels=2, width=2, samples=samples)
        return fixture, backup

    @staticmethod
    def output_samples(path: Path) -> list[tuple[int, int]]:
        with wave.open(str(path), "rb") as stream:
            raw = stream.readframes(stream.getnframes())
        samples = []
        for offset in range(0, len(raw), 6):
            channels = []
            for channel_offset in (0, 3):
                value = int.from_bytes(raw[offset + channel_offset : offset + channel_offset + 3], "little")
                channels.append(value - (1 << 24) if value & (1 << 23) else value)
            samples.append(tuple(channels))
        return samples

    def test_signed16_scaling_and_aligned_sum(self) -> None:
        zeros = [(0, 0)] * 4
        fixture, backup = self.make_custom_fixture(
            {
                "bass": [(32767, -100), (16384, -16384), (-32768, 100), (40, -40)],
                "drums": [(0, 0), (16384, 0), (0, -100), (60, 0)],
                "others": zeros,
                "vocals": zeros,
            }
        )
        reconstruct(fixture, backup, apply=True)
        # Signed 16-bit scale is 32768: +32767 stays one LSB below the
        # positive endpoint, while a +32768 aligned sum clamps to 24-bit max.
        self.assertEqual(self.output_samples(fixture / "mix.wav"), [
            (32767 * 256, -100 * 256),
            (8_388_607, -16_384 * 256),
            (-8_388_608, 0),
            (100 * 256, -40 * 256),
        ])

    def test_full_scale_sum_is_rejected(self) -> None:
        fixture, backup = self.make_fixture()
        samples = struct.pack("<hh", 32767, 0) * 2
        for name in ("bass", "drums"):
            write_wav(fixture / f"{name}.wav", rate=44_100, channels=2, width=2, samples=samples)
        with self.assertRaises(RepairError):
            reconstruct(fixture, backup, apply=True)
        self.assertFalse(backup.exists())

    def test_backup_directory_must_be_separate(self) -> None:
        fixture, _ = self.make_fixture()
        with self.assertRaises(RepairError):
            reconstruct(fixture, fixture, apply=True)

    def test_rerun_with_missing_original_backup_refuses(self) -> None:
        fixture, backup = self.make_fixture()
        reconstruct(fixture, backup, apply=True)
        (backup / "mix.wav").unlink()
        with self.assertRaises(RepairError):
            reconstruct(fixture, backup, apply=True)

    def test_mismatch_refuses_before_mutation(self) -> None:
        fixture, backup = self.make_fixture()
        write_wav(fixture / "vocals.wav", rate=44_100, channels=2, width=2, samples=struct.pack("<hh", 1, 2))
        before = (fixture / "mix.wav").read_bytes()
        with self.assertRaises(RepairError):
            reconstruct(fixture, backup, apply=True)
        self.assertEqual((fixture / "mix.wav").read_bytes(), before)
        self.assertFalse(backup.exists())

    def test_conflicting_backup_refuses_before_mix_mutation(self) -> None:
        fixture, backup = self.make_fixture()
        backup.mkdir()
        (backup / "mix.wav").write_bytes(b"different")
        before = (fixture / "mix.wav").read_bytes()
        with self.assertRaises(RepairError):
            reconstruct(fixture, backup, apply=True)
        self.assertEqual((fixture / "mix.wav").read_bytes(), before)
        self.assertEqual((backup / "mix.wav").read_bytes(), b"different")

    def test_apply_is_idempotent_and_preserves_original_backup(self) -> None:
        fixture, backup = self.make_fixture()
        original = (fixture / "mix.wav").read_bytes()
        stem_file_hash = hashlib.sha256((fixture / "bass.wav").read_bytes()).hexdigest()
        first = reconstruct(fixture, backup, apply=True)
        backup_bytes = (backup / "mix.wav").read_bytes()
        report_bytes = (backup / "provenance.json").read_bytes()
        second = reconstruct(fixture, backup, apply=True)
        self.assertEqual(backup_bytes, original)
        self.assertEqual((backup / "mix.wav").read_bytes(), backup_bytes)
        self.assertEqual((backup / "provenance.json").read_bytes(), report_bytes)
        self.assertEqual(first["mix"]["output_sha256"], second["mix"]["output_sha256"])
        report = json.loads(report_bytes)
        self.assertEqual(report["mix"]["source_sha256"], hashlib.sha256(original).hexdigest())
        self.assertEqual(report["stems"][0]["sha256"], stem_file_hash)


if __name__ == "__main__":
    unittest.main()
