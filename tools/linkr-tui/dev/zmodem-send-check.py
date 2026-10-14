#!/usr/bin/env python3
"""Drive the Rust ZMODEM sender against the real `rz`, and record what happened.

Why this exists: `dev/zmodem-capture.py` records the *receiving* half of ZMODEM
by running the real `sz` and `rz` against each other.  The sending half added
later has no counterpart there -- it needs a real receiver to push at, and the
point of the exercise is that the real receiver is the judge.

Topology.  `rz` gets one socketpair end as both fd 0 and fd 1, the Rust sender
(`examples/zmodem-send.rs`) gets the other pair's end the same way, and this
script bridges the two, recording every byte in both directions.  Then the file
that `rz` wrote is compared to the file that was sent.

Two things are recorded as fixtures, and both are used by the Rust test suite:

  * `sender-sent.bin`  -- every byte the Rust sender wrote
  * `sender-reply.bin` -- every byte the real `rz` wrote back

Replaying `sender-reply.bin` through `Sender` must reproduce `sender-sent.bin`
byte for byte.  That is the strongest statement available about the sender: the
bytes it has to produce are the bytes a real receiver drove it to produce, and
the file that receiver wrote is checked against the original as well.

    python3 dev/zmodem-send-check.py
    python3 dev/zmodem-send-check.py --roundtrip    # and a real sz behind it
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
WORK = ROOT / "target" / "zmodem-send"
FIXTURES = ROOT / "tests" / "fixtures" / "zmodem"
EXAMPLE = ROOT / "target" / "debug" / "examples" / "zmodem-send"
RZ = "rz"

# The size is not arbitrary: 4112 is the size in the recorded `basic` download,
# so the two fixtures describe the same file and can be read together.  It is
# also four full 1024-byte blocks plus a 16-byte tail, which is where the
# `ZCRCG`/`ZCRCE` distinction lives.
SIZE = 4112


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def build_payload(size: int) -> bytes:
    """The payload from `zmodem-capture.py`, copied so the two agree by value.

    Imported by path rather than by module name, because `dev/` is not a
    package.
    """
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "zmodem_capture", ROOT / "dev" / "zmodem-capture.py"
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    # The capture script parses argv at import? It does not: `main()` is guarded.
    spec.loader.exec_module(module)
    return module.payload(size)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--name", default="basic",
                    help="names the checked-in fixture, so cases do not clobber each other")
    ap.add_argument("--size", type=int, default=SIZE,
                    help="payload size; 0 sends a genuinely empty file")
    ap.add_argument("--timeout", type=float, default=25.0)
    args = ap.parse_args()

    if not pathlib.Path("/usr/bin/rz").exists() and not pathlib.Path("/usr/local/bin/rz").exists():
        print("rz not found -- install lrzsz (apt install lrzsz)", file=sys.stderr)
        return 2

    built = subprocess.run(
        ["cargo", "build", "--example", "zmodem-send"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if built.returncode != 0:
        print(built.stdout, built.stderr, file=sys.stderr)
        return 2
    if not EXAMPLE.exists():
        print(f"expected the example at {EXAMPLE}", file=sys.stderr)
        return 2

    case = WORK / args.name
    send_dir = case / "send"
    recv_dir = case / "recv"
    for directory in (send_dir, recv_dir):
        directory.mkdir(parents=True, exist_ok=True)
    data = build_payload(args.size) if args.size else b""
    (send_dir / "payload.bin").write_bytes(data)
    stale = recv_dir / "payload.bin"
    if stale.exists():
        stale.unlink()

    rz_ch, us_ch = socket.socketpair(), socket.socketpair()
    rz = subprocess.Popen(
        [RZ, "-y", "-q", "-b"], cwd=recv_dir,
        stdin=rz_ch[1], stdout=rz_ch[1], stderr=subprocess.PIPE,
    )
    us = subprocess.Popen(
        [str(EXAMPLE), str(send_dir / "payload.bin")], cwd=ROOT,
        stdin=us_ch[1], stdout=us_ch[1], stderr=subprocess.PIPE,
    )
    # The parent must drop the child ends, or EOF never propagates and both
    # sides wait forever for a shutdown that has already happened.
    rz_ch[1].close()
    us_ch[1].close()

    logs = {rz_ch[0]: bytearray(), us_ch[0]: bytearray()}
    peers = {rz_ch[0]: us_ch[0], us_ch[0]: rz_ch[0]}
    live = {rz_ch[0], us_ch[0]}
    deadline = time.monotonic() + args.timeout
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

    for proc in (rz, us):
        if proc.poll() is None:
            proc.kill()
        proc.wait()
    rz_err = rz.stderr.read().decode("utf-8", "replace")
    us_err = us.stderr.read().decode("utf-8", "replace")
    rz.stderr.close(), us.stderr.close()
    rz_ch[0].close(), us_ch[0].close()

    sent, reply = bytes(logs[us_ch[0]]), bytes(logs[rz_ch[0]])
    (case / "sender-sent.bin").write_bytes(sent)
    (case / "sender-reply.bin").write_bytes(reply)
    (case / "stderr.txt").write_text(
        f"--- sender stderr ---\n{us_err}\n--- rz stderr ---\n{rz_err}\n"
    )

    got = stale.read_bytes() if stale.exists() else b""
    # "The far end wrote nothing and we sent nothing" is not a pass: it is the
    # shape a stalled session leaves behind, and comparing two empty files would
    # call it identical.
    exists = stale.exists()
    ok = exists and got == data
    print(f"sender -> rz: sent {len(data)}B in {len(sent)}B of protocol, "
          f"rz answered {len(reply)}B")
    print(f"  received {len(got)}B sha256 {sha256(got)}"
          if exists else "  the far end wrote no file at all")
    print(f"  expected {len(data)}B sha256 {sha256(data)}")
    print(f"  verdict  {'IDENTICAL' if ok else 'DIFFERENT'}")
    if not ok:
        print(f"  sender stderr: {us_err.strip()}", file=sys.stderr)
        print(f"  rz stderr:     {rz_err.strip()}", file=sys.stderr)

    if ok:
        FIXTURES.mkdir(parents=True, exist_ok=True)
        (FIXTURES / f"sender-{args.name}-sent.bin").write_bytes(sent)
        (FIXTURES / f"sender-{args.name}-reply.bin").write_bytes(reply)
        (FIXTURES / f"sender-{args.name}-payload.bin").write_bytes(data)
        print(f"  fixtures: {FIXTURES}/sender-{args.name}-*.bin")

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
