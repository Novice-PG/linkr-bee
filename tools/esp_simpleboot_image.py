#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Supply the ROM-visible SHA-256 in Zephyr's RAM-only simple-boot image.

The ESP image header/checksum format is documented by Espressif. The digest
covers the ROM-visible RAM prefix, not the hidden memory-mapped segments.
Use existing zero padding so all Flash payload addresses and file size stay
unchanged. The matching loader shim skips the digest before scanning those
hidden segments. This tool must only be used with that loader.
"""
import argparse
import hashlib
import os
from pathlib import Path
import struct
import tempfile

HEADER = 24
SEGMENT = struct.Struct("<II")
DIGEST_SIZE = 32


def rom_end(image: bytes) -> int:
    """Validate the visible segments and XOR checksum; return the footer end."""
    if len(image) < HEADER or image[0] != 0xE9 or not 1 <= image[1] <= 16:
        raise ValueError("invalid ESP image header")
    if image[23] not in (0, 1):
        raise ValueError("invalid hash-appended flag")
    cursor = HEADER
    checksum = 0xEF
    for _ in range(image[1]):
        if cursor + SEGMENT.size > len(image):
            raise ValueError("truncated RAM segment header")
        _, size = SEGMENT.unpack_from(image, cursor)
        begin = cursor + SEGMENT.size
        cursor = begin + size
        if cursor > len(image):
            raise ValueError("truncated RAM segment data")
        for value in image[begin:cursor]:
            checksum ^= value
    end = (cursor + 16) & ~15
    if end > len(image) or any(image[cursor:end - 1]):
        raise ValueError("invalid RAM checksum padding")
    if image[end - 1] != checksum:
        raise ValueError("RAM XOR checksum mismatch")
    return end


def with_rom_digest(image: bytes) -> bytes:
    end = rom_end(image)
    if image[23] == 1:
        digest = image[end:end + DIGEST_SIZE]
        if len(digest) != DIGEST_SIZE or digest != hashlib.sha256(image[:end]).digest():
            raise ValueError("existing ROM SHA-256 mismatch")
        return image
    if end + SEGMENT.size > len(image):
        raise ValueError("missing hidden padding segment")
    address, size = SEGMENT.unpack_from(image, end)
    following = end + SEGMENT.size + size
    if address != 0 or size < DIGEST_SIZE or following + SEGMENT.size > len(image):
        raise ValueError("no room for a ROM digest before the mapped segments")
    if any(image[end + SEGMENT.size:following]):
        raise ValueError("hidden padding contains nonzero payload")
    result = bytearray(image)
    result[23] = 1
    result[end:end + DIGEST_SIZE] = hashlib.sha256(result[:end]).digest()
    SEGMENT.pack_into(result, end + DIGEST_SIZE, 0, size - DIGEST_SIZE)
    return bytes(result)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path)
    parser.add_argument("--check", action="store_true", help="verify only; do not modify")
    args = parser.parse_args()
    try:
        original = args.image.read_bytes()
        updated = with_rom_digest(original)
        if args.check:
            if updated != original:
                raise ValueError("ROM SHA-256 is missing")
        elif updated != original:
            temporary = None
            try:
                with tempfile.NamedTemporaryFile(dir=args.image.parent, delete=False) as output:
                    temporary = Path(output.name)
                    output.write(updated)
                temporary.chmod(args.image.stat().st_mode & 0o777)
                os.replace(temporary, args.image)
            finally:
                if temporary is not None:
                    temporary.unlink(missing_ok=True)
        print(f"ROM SHA-256 verified: {args.image}")
        return 0
    except (OSError, ValueError) as error:
        parser.exit(1, f"ESP simple-boot image: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
