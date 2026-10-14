#!/usr/bin/env python3
"""Drive the Rust ZMODEM receiver against the real `sz`, and record what happened.

This is the mirror of `dev/zmodem-send-check.py`.  That script pushes our sender
at a real `rz`; this one points a real `sz` at our receiver, which is the other
half of "the protocol is right" -- and the half that ends somewhere a codec test
cannot follow, because a receive is not finished until the bytes are a file on
disk under the name the far end chose.

Topology.  `sz` gets one socketpair end as both fd 0 and fd 1; the Rust receiver
(`examples/zmodem-recv.rs`) gets the other pair's end the same way; this script
bridges them and records every byte in both directions.  Then the file in the
incoming directory is compared to the one `sz` was given.

Two things are recorded, so they can become golden fixtures later:

  * `receiver-offer.bin`  -- every byte the real `sz` wrote
  * `receiver-answer.bin` -- every byte our receiver wrote back

    python3 dev/zmodem-recv-check.py
    python3 dev/zmodem-recv-check.py --size 0     # a genuinely empty file
"""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import select
import socket
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORK = ROOT / "target" / "zmodem-recv"
CASE = ROOT / "target" / "zmodem-recv-cases"
EXAMPLE = ROOT / "target" / "debug" / "examples" / "zmodem-recv"
SZ = "sz"

# Same reasoning as the send harness: 4112 bytes is the size of the recorded
# `basic` download, so the fixtures across these scripts describe one file and
# can be read next to each other.
SIZE = 4112


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def build_payload(size: int) -> bytes:
    """The payload from `zmodem-capture.py`, so recorded cases agree by value."""
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "zmodem_capture", ROOT / "dev" / "zmodem-capture.py"
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    # Safe to import: that script's argv parsing is inside `main()`.
    spec.loader.exec_module(module)
    return module.payload(size)


def bridge(sz_ch, us_ch, timeout: float) -> dict:
    """Move bytes between the two ends until both stop, recording what passed."""
    logs = {sz_ch[0]: bytearray(), us_ch[0]: bytearray()}
    peers = {sz_ch[0]: us_ch[0], us_ch[0]: sz_ch[0]}
    live = {sz_ch[0], us_ch[0]}
    deadline = time.monotonic() + timeout
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
    return {key: bytes(value) for key, value in logs.items()}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--name", default="basic",
                    help="names the recorded case, so runs do not clobber each other")
    ap.add_argument("--size", type=int, default=SIZE,
                    help="payload size; 0 sends a genuinely empty file")
    ap.add_argument("--timeout", type=float, default=25.0)
    args = ap.parse_args()

    if not any(pathlib.Path(f"/usr/{place}/sz").exists() for place in ("bin", "local/bin")):
        print("sz not found -- install lrzsz (apt install lrzsz)", file=sys.stderr)
        return 2

    built = subprocess.run(
        ["cargo", "build", "--example", "zmodem-recv"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if built.returncode != 0:
        print(built.stdout, built.stderr, file=sys.stderr)
        return 2
    if not EXAMPLE.exists():
        print(f"expected the example at {EXAMPLE}", file=sys.stderr)
        return 2

    case = CASE / args.name
    send_dir = case / "send"
    incoming = WORK / args.name / "incoming"
    for directory in (send_dir, incoming):
        directory.mkdir(parents=True, exist_ok=True)
    data = build_payload(args.size) if args.size else b""
    (send_dir / "payload.bin").write_bytes(data)
    landed = incoming / "payload.bin"
    if landed.exists():
        landed.unlink()

    sz_ch, us_ch = socket.socketpair(), socket.socketpair()
    sz = subprocess.Popen(
        [SZ, "-y", "-q", "-b", "payload.bin"], cwd=send_dir,
        stdin=sz_ch[1], stdout=sz_ch[1], stderr=subprocess.PIPE,
    )
    us = subprocess.Popen(
        [str(EXAMPLE), str(incoming)], cwd=ROOT,
        stdin=us_ch[1], stdout=us_ch[1], stderr=subprocess.PIPE,
    )
    # The parent must drop the child ends or EOF never propagates and both sides
    # wait forever for a shutdown that has already happened.
    sz_ch[1].close()
    us_ch[1].close()

    logs = bridge(sz_ch, us_ch, args.timeout)

    for proc in (sz, us):
        if proc.poll() is None:
            proc.kill()
        proc.wait()
    sz_err = sz.stderr.read().decode("utf-8", "replace")
    us_err = us.stderr.read().decode("utf-8", "replace")
    sz.stderr.close(), us.stderr.close()
    sz_ch[0].close(), us_ch[0].close()

    offer, answer = logs[sz_ch[0]], logs[us_ch[0]]
    (case / "receiver-offer.bin").write_bytes(offer)
    (case / "receiver-answer.bin").write_bytes(answer)
    (case / "stderr.txt").write_text(
        f"--- receiver stderr ---\n{us_err}\n--- sz stderr ---\n{sz_err}\n"
    )

    got = landed.read_bytes() if landed.exists() else b""
    # Two empty files agreeing is not a pass: it is the shape a session that
    # never started leaves behind, so the file has to actually be there.
    exists = landed.exists()
    ok = exists and got == data
    print(f"sz -> receiver: offered {len(data)}B in {len(offer)}B of protocol, "
          f"our receiver answered {len(answer)}B")
    print(f"  landed {len(got)}B sha256 {sha256(got)}"
          if exists else "  nothing landed in the incoming directory")
    print(f"  expected {len(data)}B sha256 {sha256(data)}")
    print(f"  verdict  {'IDENTICAL' if ok else 'DIFFERENT'}")
    if not ok:
        print(f"  receiver stderr: {us_err.strip()}", file=sys.stderr)
        print(f"  sz stderr:       {sz_err.strip()}", file=sys.stderr)
    print(f"  case:     {case}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
