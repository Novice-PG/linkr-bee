"""ROM-format regression tests, independent of esptool and a connected board."""
import hashlib
import importlib.util
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
TOOL = ROOT / "tools/esp_simpleboot_image.py"
spec = importlib.util.spec_from_file_location("simpleboot_image", TOOL)
image_tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(image_tool)


def fixture(lengths=(7, 16, 33), padding=96, pad_address=0):
    header = bytearray(24)
    header[0:4] = bytes((0xE9, len(lengths), 2, 0x2F))
    checksum = 0xEF
    for index, size in enumerate(lengths):
        content = bytes((index + byte) % 256 for byte in range(size))
        header.extend(struct.pack("<II", 0x3FC80000 + index * 0x1000, size))
        header.extend(content)
        for value in content:
            checksum ^= value
    while len(header) % 16 != 15:
        header.append(0)
    header.append(checksum)
    end = len(header)
    header.extend(struct.pack("<II", pad_address, padding))
    header.extend(bytes(padding))
    mapped = len(header)
    header.extend(struct.pack("<II", 0x42000000, 12))
    header.extend(b"mapped-code!")
    return bytes(header), end, mapped


class SimpleBootImageTests(unittest.TestCase):
    def test_digest_covers_visible_header_segments_and_checksum(self):
        for lengths in [(1,), (16,), (15, 17), (7, 16, 33)]:
            with self.subTest(lengths=lengths):
                original, end, mapped = fixture(lengths)
                actual = image_tool.with_rom_digest(original)
                self.assertEqual(actual[23], 1)
                self.assertEqual(actual[end:end + 32], hashlib.sha256(actual[:end]).digest())
                self.assertEqual(len(actual), len(original))
                self.assertEqual(actual[mapped:], original[mapped:])
                self.assertEqual(actual[:23], original[:23])
                self.assertEqual(actual[24:end], original[24:end])
                address, size = struct.unpack_from("<II", actual, end + 32)
                self.assertEqual((address, size), (0, 64))
                self.assertEqual(end + 32 + 8 + size, mapped)

    def test_existing_valid_digest_is_idempotent(self):
        original, _, _ = fixture()
        fixed = image_tool.with_rom_digest(original)
        self.assertEqual(image_tool.with_rom_digest(fixed), fixed)

    def test_corrupt_digest_is_rejected_instead_of_replaced(self):
        original, end, _ = fixture()
        fixed = bytearray(image_tool.with_rom_digest(original))
        fixed[end] ^= 1
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            image_tool.with_rom_digest(bytes(fixed))

    def test_corrupt_ram_checksum_is_rejected(self):
        original, _, _ = fixture()
        broken = bytearray(original)
        broken[32] ^= 1
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            image_tool.with_rom_digest(bytes(broken))

    def test_short_padding_and_real_segment_cannot_be_overwritten(self):
        for image, _, _ in [fixture(padding=31), fixture(pad_address=0x42000000)]:
            with self.assertRaisesRegex(ValueError, "no room"):
                image_tool.with_rom_digest(image)

    def test_nonzero_padding_is_rejected(self):
        original, end, _ = fixture()
        broken = bytearray(original)
        broken[end + 8] = 1
        with self.assertRaisesRegex(ValueError, "nonzero"):
            image_tool.with_rom_digest(bytes(broken))

    def test_truncated_inputs_and_invalid_flags_are_rejected(self):
        original, end, _ = fixture()
        cases = [b"", original[:24], original[:33], original[:end - 1], original[:end]]
        for offset, value in [(0, 0), (1, 0), (1, 17), (23, 2)]:
            broken = bytearray(original)
            broken[offset] = value
            cases.append(bytes(broken))
        for image in cases:
            with self.subTest(size=len(image), header=image[:24]):
                with self.assertRaises(ValueError):
                    image_tool.with_rom_digest(image)

    def test_check_mode_never_modifies_image_and_rejects_missing_digest(self):
        original, _, _ = fixture()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "firmware.bin"
            path.write_bytes(original)
            result = subprocess.run([sys.executable, str(TOOL), "--check", str(path)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(path.read_bytes(), original)
            subprocess.run([sys.executable, str(TOOL), str(path)], check=True, capture_output=True)
            fixed = path.read_bytes()
            subprocess.run([sys.executable, str(TOOL), "--check", str(path)], check=True, capture_output=True)
            self.assertEqual(path.read_bytes(), fixed)

    def test_invalid_cli_input_is_left_intact(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bad.bin"
            path.write_bytes(b"not firmware")
            result = subprocess.run([sys.executable, str(TOOL), str(path)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(path.read_bytes(), b"not firmware")
