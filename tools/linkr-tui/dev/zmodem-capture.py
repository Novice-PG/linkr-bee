#!/usr/bin/env python3
"""Capture reference ZMODEM sessions between the real `sz` and `rz`.

Why this exists: the TUI's ZMODEM layer has no upstream web client to port from
(the web client transfers files with the base64 paging shell protocol instead),
so correctness has to be anchored to something else.  lrzsz -- the reference
implementation, shipped by every Linux distro as `sz`/`rz` -- is that anchor.

Topology.  `sz` reads the incoming stream from fd 0 and writes the outgoing
stream to fd 1; `rz` does the same mirrored.  ZMODEM is an *in band* protocol
over a *single* bidirectional channel, so each side is given one socketpair end
as both its fd 0 and fd 1, and the parent bridges the two ends while recording
every byte in both directions.  Two independent pipes would be two channels and
is not the same protocol situation.

Two traps this script exists to avoid, both of which cost real time:

  * Python's `subprocess` pipes are buffered.  A blocking `.read(4096)` on them
    waits for the *whole* 4096 bytes, so a 65-byte ZRQINIT is never forwarded
    and neither side ever sees the other.  Everything here reads raw fds.
  * lrzsz wants a tty, but a `devpts` mount is not enough -- `/dev/ptmx` has to
    exist as a node.  In a container where `/dev` is a small tmpfs it does not,
    and `openpty()` fails with "out of pty devices".  Pipes are fine on their
    own: `sz` accepts a spec-conformant ZRINIT on a plain pipe and advances to
    ZFILE.  So no pty is used.

The logs are golden fixtures: the Rust decoder is tested against `sz2rz.bin`
(what a real sender emits) and the Rust encoder against `rz2sz.bin` (what a real
receiver wants to hear).

    python3 dev/zmodem-capture.py                # the standard fixture set
    python3 dev/zmodem-capture.py --size 70000   # multi-subpacket payload
    python3 dev/zmodem-capture.py --ascii        # ASCII mode (-a), CR/LF folded
    python3 dev/zmodem-capture.py --crc16        # 16-bit CRC (-o) not CRC-32
    python3 dev/zmodem-capture.py --dialogue     # annotate the basic case
"""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import select
import socket
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "zmodem-ref"

SZ = "sz"
RZ = "rz"

# A payload with everything that stresses the wire format: a ZDLE byte that must
# be escaped, a CR that is only escaped when a control char follows it, NUL, and
# high bytes.
HOSTILE = bytes(
    [
        0x18,  # ZDLE itself -- must always be escaped
        0x0D,  # CR
        0x0D,  # CR followed by XON: the classic "CR XON" escape case
        0x11,
        0x00,
        0x0A,
        0x13,  # XOFF
        0x7F,
        0x91,  # high bit set, and the two values lrzsz calls ZRUB0 / ZRUB1
        0xFF,
        0x1A,  # SUB
    ]
)


def payload(size: int) -> bytes:
    """Deterministic payload: hostile prefix, LCG stream, then a marker tail.

    The tail matters: a truncation bug that drops the final subpacket would
    still pass if the payload were uniform filler.
    """
    out = bytearray(HOSTILE)
    x = 0x12345678
    while len(out) < size:
        x = (1103515245 * x + 12345) & 0xFFFFFFFF
        out.append((x >> 16) & 0xFF)
    out = out[: max(size, len(HOSTILE))]
    out += b"\nEND-OF-PAYLOAD\n"
    return bytes(out)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def crc16xmodem(data: bytes, init: int = 0) -> int:
    """CRC-16/XMODEM: poly 0x1021, init 0, MSB-first, no final xor.

    Verified against a real rz ZRINIT.  CRC over the five header bytes
    [TYPE, F3, F2, F1, F0] reproduces the transmitted `be50`, and *only* the
    non-reflected form does; the widespread description of ZMODEM's frame check
    as CRC-16/X-25 is wrong.
    """
    crc = init
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
    return crc


# --------------------------------------------------------------------------- #
# Session driver
# --------------------------------------------------------------------------- #


class Channel:
    """one socketpair, both ends usable: the parent's end logs, the child's end
    becomes the process's fd 0 *and* fd 1"""

    def __init__(self):
        self.parent, self.child = socket.socketpair()


def run_session(
    name: str,
    data: bytes,
    *,
    ascii_mode: bool = False,
    crc16: bool = False,
    chunk: int = 0,
    timeout: float = 25.0,
    quiet: bool = True,
) -> tuple[int, bytes, bytes]:
    case_dir = WORK / name
    (case_dir / "send").mkdir(parents=True, exist_ok=True)
    (case_dir / "recv").mkdir(parents=True, exist_ok=True)
    for stale in (case_dir / "recv" / "payload.bin",):
        if stale.exists():
            stale.unlink()

    (case_dir / "send" / "payload.bin").write_bytes(data)
    expect = data.replace(b"\r\n", b"\n") if ascii_mode else data

    sz_cmd = [SZ, "-b", "-q", "--delay-startup", "0"]
    rz_cmd = [RZ, "-y", "-q", "-b"]
    if ascii_mode:
        sz_cmd, rz_cmd = [SZ, "-a", "-q"], [RZ, "-y", "-q", "-a"]
    if crc16:
        sz_cmd.append("-o")
    if chunk:
        sz_cmd += ["-L", str(chunk), "-l", str(chunk)]
    sz_cmd.append("payload.bin")

    sz_ch, rz_ch = Channel(), Channel()
    sz = subprocess.Popen(
        sz_cmd, cwd=case_dir / "send",
        stdin=sz_ch.child.fileno(), stdout=sz_ch.child.fileno(),
        stderr=subprocess.PIPE,
    )
    rz = subprocess.Popen(
        rz_cmd, cwd=case_dir / "recv",
        stdin=rz_ch.child.fileno(), stdout=rz_ch.child.fileno(),
        stderr=subprocess.PIPE,
    )
    # The parent must drop the child ends, or EOF never propagates.
    sz_ch.child.close()
    rz_ch.child.close()

    logs = {sz_ch.parent: bytearray(), rz_ch.parent: bytearray()}
    peers = {sz_ch.parent: rz_ch.parent, rz_ch.parent: sz_ch.parent}
    import time

    deadline = time.monotonic() + timeout
    live = {sz_ch.parent, rz_ch.parent}
    while live and time.monotonic() < deadline:
        readable, _, _ = select.select(list(live), [], [], 0.2)
        for src in readable:
            try:
                got = src.recv(65536)
            except OSError:
                got = b""
            if not got:
                live.discard(src)
                continue
            logs[src].extend(got)
            dst = peers[src]
            if dst in live:
                try:
                    dst.sendall(got)
                except OSError:
                    live.discard(dst)

    for proc in (sz, rz):
        if proc.poll() is None:
            proc.kill()
        proc.wait()
    sz_err = sz.stderr.read().decode("utf-8", "replace")
    rz_err = rz.stderr.read().decode("utf-8", "replace")
    sz.stderr.close(), rz.stderr.close()
    sz_ch.parent.close(), rz_ch.parent.close()

    sz2rz, rz2sz = bytes(logs[sz_ch.parent]), bytes(logs[rz_ch.parent])
    (case_dir / "sz2rz.bin").write_bytes(sz2rz)
    (case_dir / "rz2sz.bin").write_bytes(rz2sz)
    (case_dir / "stderr.txt").write_text(
        f"--- sz stderr ---\n{sz_err}\n--- rz stderr ---\n{rz_err}\n"
    )

    got_path = case_dir / "recv" / "payload.bin"
    got = got_path.read_bytes() if got_path.exists() else b""
    (case_dir / "transcript.txt").write_text(
        render_transcript(name, data, expect, got, sz2rz, rz2sz, sz_err, rz_err)
    )

    ok = got == expect
    if not quiet:
        print(f"[{name}] sent={len(data)}B expect={len(expect)}B got={len(got)}B "
              f"sz2rz={len(sz2rz)}B rz2sz={len(rz2sz)}B -> {'OK' if ok else 'MISMATCH'}")
        if not ok:
            print(f"    sha256 expect {sha256(expect)}")
            print(f"    sha256 got    {sha256(got)}")
            print(f"    transcript: {case_dir / 'transcript.txt'}")
    return (0 if ok else 1), sz2rz, rz2sz


# --------------------------------------------------------------------------- #
# Annotation -- the same decoder the Rust side will implement, in miniature, so
# that a bad fixture is obvious on sight instead of three sessions later.
# --------------------------------------------------------------------------- #

ZPAD, ZDLE = 0x2A, 0x18

FRAME_NAMES = {
    0x00: "ZRQINIT", 0x01: "ZRINIT", 0x02: "ZSINIT", 0x03: "ZACK",
    0x04: "ZFILE", 0x05: "ZSKIP", 0x06: "ZNAK", 0x07: "ZABORT",
    0x08: "ZFIN", 0x09: "ZRPOS", 0x0A: "ZDATA", 0x0B: "ZEOF",
    0x0C: "ZFERR", 0x0D: "ZCRC", 0x0E: "ZCHALLENGE", 0x0F: "ZCOMPL",
    0x10: "ZCAN", 0x11: "ZFREECNT", 0x12: "ZCOMMAND", 0x13: "ZSTDERR",
}

# The four bytes that may follow ZDLE to end a data subpacket, and they are the
# *literal* letters -- unlike an escaped byte, which is sent XOR 0x40.  Read off
# the recordings: the name block ends `18 6b`, the last block of a file ends
# `18 68`, and the blocks in between end `18 69`.
SUBPACKET_NAMES = {0x68: "ZCRCE", 0x69: "ZCRCG", 0x6A: "ZCRCQ", 0x6B: "ZCRCW"}


def frame_name(t: int) -> str:
    return FRAME_NAMES.get(t, f"0x{t:02x}")


def annotate(blob: bytes, limit: int = 120) -> list[str]:
    """Walk a ZMODEM stream and describe each header / control sequence."""
    out: list[str] = []
    i, n = 0, len(blob)
    while i < n and len(out) < limit:
        b = blob[i]
        if b == ZPAD and blob[i + 1 : i + 3] == b"*\x18":
            marker = blob[i + 3 : i + 4]
            if marker == b"B":
                hexs = blob[i + 4 : i + 18].decode("ascii", "replace")
                raw = bytes.fromhex(hexs) if len(hexs) == 14 else b""
                if len(raw) == 7:
                    typ, f, crc = raw[0], raw[1:5], int.from_bytes(raw[5:7], "big")
                    want = crc16xmodem(raw[:5])
                    tail = blob[i + 18 : i + 22]
                    out.append(
                        f"@{i:6d} HEX   {frame_name(typ):<10} F3..F0={f.hex()} "
                        f"crc={crc:04x}{' OK' if crc == want else f' BAD(want {want:04x})'} "
                        f"tail={tail.hex()}"
                    )
                    i += 18
                    continue
            elif marker in (b"A", b"C"):
                width = 2 if marker == b"A" else 4
                raw = blob[i + 3 : i + 3 + 5 + width]
                if len(raw) == 5 + width:
                    out.append(
                        f"@{i:6d} BIN{marker.decode()}  {frame_name(raw[0]):<10} "
                        f"F3..F0={raw[1:5].hex()} crc={int.from_bytes(raw[5:], 'big'):0{width * 2}x}"
                    )
                    i += 3 + 5 + width
                    continue
        elif b == ZDLE:
            nxt = blob[i + 1] if i + 1 < n else None
            if nxt is None:
                out.append(f"@{i:6d} ZDLE <eof>")
                i += 1
            elif nxt == ZDLE:
                out.append(f"@{i:6d} ZDLE ZDLE   (a literal ZDLE; lrzsz sends 18 58)")
                i += 2
            elif nxt in SUBPACKET_NAMES:
                out.append(f"@{i:6d} ZDLE {SUBPACKET_NAMES[nxt]}")
                i += 2
            else:
                out.append(f"@{i:6d} ZDLE 0x{nxt:02x}   (escaped -> 0x{nxt ^ 0x40:02x})")
                i += 2
        else:
            i += 1
    return out or ["  (nothing recognisable)"]


def render_transcript(name, sent, expect, got, sz2rz, rz2sz, sz_err, rz_err) -> str:
    lines = [
        f"# ZMODEM reference session: {name}",
        f"# sz -> rz {len(sz2rz)} bytes | rz -> sz {len(rz2sz)} bytes",
        f"# payload  {len(sent)}B sha256 {sha256(sent)}",
        f"# received {len(got)}B sha256 {sha256(got)}",
        f"# verdict  {'IDENTICAL' if got == expect else 'DIFFERENT'}",
        "",
        "## sender -> receiver (sz2rz.bin)",
    ]
    lines += annotate(sz2rz)
    lines += ["", "## receiver -> sender (rz2sz.bin)"]
    lines += annotate(rz2sz)
    lines += ["", "## stderr", f"--- sz ---\n{sz_err}", f"--- rz ---\n{rz_err}"]
    return "\n".join(lines) + "\n"


CASES = [
    # name, size, ascii, crc16, chunk-limit
    ("basic", 4096, False, False, 0),
    ("hostile-small", 64, False, False, 0),
    ("crc16", 4096, False, True, 0),
    ("crlf-ascii", 4096, True, False, 0),
    ("small-block", 70000, False, False, 256),
    ("empty", 0, False, False, 0),
]


def payload_or_empty(size: int) -> bytes:
    """`payload()`, except that size 0 means a genuinely empty file.

    The empty file is worth recording on its own: `payload()` would have padded
    it up to the hostile prefix, and a zero-byte transfer is the one case where
    the sender has to decide whether a `ZDATA` header is worth writing at all.
    """
    return b"" if size == 0 else payload(size)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--size", type=int, help="run a single custom-size case")
    ap.add_argument("--ascii", action="store_true", help="only the ASCII/CRLF case")
    ap.add_argument("--crc16", action="store_true", help="only the CRC-16 case")
    ap.add_argument("--empty", action="store_true", help="only the zero-byte file case")
    ap.add_argument("--dialogue", action="store_true", help="print transcripts")
    ap.add_argument("--timeout", type=float, default=20.0)
    args = ap.parse_args()

    if not pathlib.Path("/usr/bin/sz").exists() and not pathlib.Path("/usr/local/bin/sz").exists():
        print("sz not found -- install lrzsz (apt install lrzsz)", file=sys.stderr)
        return 2

    cases = CASES
    if args.ascii:
        cases = [c for c in CASES if c[0] == "crlf-ascii"]
    elif args.crc16:
        cases = [c for c in CASES if c[0] == "crc16"]
    elif args.size is not None:
        cases = [("custom", args.size, False, False, 0)]
    if args.empty:
        cases = [c for c in CASES if c[0] == "empty"]

    WORK.mkdir(parents=True, exist_ok=True)
    failures = 0
    for name, size, ascii_mode, crc16_on, chunk in cases:
        body = payload_or_empty(size)
        rc, sz2rz, rz2sz = run_session(
            name, body, ascii_mode=ascii_mode, crc16=crc16_on,
            chunk=chunk, timeout=args.timeout, quiet=False,
        )
        failures += rc
        if args.dialogue and name == "basic":
            print()
            print(render_transcript(name, body, body, body,
                                    sz2rz, rz2sz, "", ""))

    print(f"\nfixtures under {WORK}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
