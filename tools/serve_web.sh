#!/usr/bin/env sh
set -eu

ROOT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
PORT=${1:-8765}

cd "$ROOT_DIR"
echo "Serving Web Bluetooth terminal at http://127.0.0.1:$PORT/"

# Deliberately not `python3 -m http.server`: that listens with a backlog of 5
# (socketserver.TCPServer.request_queue_size) and speaks HTTP/1.0, so every
# request is its own connection. Opening this page fires off about thirty module
# requests at once, and the surplus connection is reset by the kernel instead of
# being queued. The browser reports ERR_CONNECTION_RESET for whichever module
# lost the race, and because the terminal boots from static ES modules, one lost
# request leaves a blank page with nothing on it to say why.
#
# Measured on this machine by loading the page and checking that the terminal
# came up: with the default backlog, 3 loads in 30 never booted (each time a
# different module, always ERR_CONNECTION_RESET). With the backlog below: 32 of
# 32. The sample is small, so read that as a strong indication rather than proof,
# but the mechanism is not subtle -- a queue of 5 against a burst of 30.
python3 - "$PORT" <<'PY'
import functools
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

ThreadingHTTPServer.request_queue_size = 128
handler = functools.partial(SimpleHTTPRequestHandler, directory="web")
ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), handler).serve_forever()
PY
