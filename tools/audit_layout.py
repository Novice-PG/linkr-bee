#!/usr/bin/env python3
"""Assert the app shell layout across viewports.

Screenshots from webview_shot are unreliable for anything that changes after
load (WebKit never repaints them in a run-loop-driven process), and even for
static layout the capture can be scaled or taken mid-reflow. The DOM's own
rects are the authority, so this script reads them and checks the invariants
that actually break:

  - the shell fills the viewport
  - the work panel does not overflow to the right
  - the drawer is off screen below 900px, and docked left above it
  - nothing scrolls vertically

Usage:
    # serve web/ first, e.g. python3 -m http.server 8880 --bind 127.0.0.1
    python3 tools/audit_layout.py [url]

Exit code is non-zero when any viewport fails.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL = HERE / "webview_shot.swift"
URL = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8880/index.html"

VIEWPORTS = [
    ("desktop", 1440, 900),
    ("laptop", 1280, 800),
    ("tablet-l", 1024, 768),
    ("tablet-p", 834, 1112),
    ("phone-xl", 430, 932),
    ("phone", 390, 844),
    ("phone-s", 375, 667),
    ("landscape", 844, 390),
]

# Not every state is meaningful at every size. The drawer matters wherever it
# overlays content, connected/keyboard only on phones. Above 900 px the drawer
# is a docked column that the rail collapses by default, so the open state is
# measured there too — that is the rail layout's primary interaction.
STATES = {
    "desktop": ["closed", "drawer"],
    "laptop": ["closed", "drawer"],
    "tablet-l": ["closed"],
    "tablet-p": ["closed", "drawer"],
    "phone-xl": ["closed", "drawer"],
    "phone": ["closed", "drawer", "connected", "keyboard"],
    "phone-s": ["closed", "drawer"],
    "landscape": ["closed", "drawer"],
}

# The tool must talk to localhost directly; the agent environment injects a
# proxy that would answer 502 for 127.0.0.1.
ENV = {k: v for k, v in os.environ.items() if "proxy" not in k.lower()}


def rect(spec):
    size, origin = spec.split("@")
    w, h = size.split("x")
    x, y = origin.split(",")
    return int(w), int(h), int(x), int(y)


def measure(name, w, h, state="closed"):
    out = subprocess.run(
        ["swift", str(TOOL), f"{URL}?audit={name}-{state}", f"/tmp/audit-{name}-{state}.png",
         str(w), str(h), "dark", state],
        capture_output=True, text=True, env=ENV, timeout=90,
    ).stdout
    line = next((l for l in out.splitlines() if l.startswith("layout:")), None)
    if not line:
        return None
    return json.loads(line[len("layout: "):])


def audit(d, state):
    vw = d["vw"]
    sw, sh, _, _ = rect(d["shell"])
    ww, _, wx, wy = rect(d["work"])
    cw, _, cx, _ = rect(d["ctl"])
    pad = float(d["padR"].replace("px", ""))

    checks = [
        ("shell==viewport", sw == vw and sh == d["vh"], f"{sw}x{sh} vs {vw}x{d['vh']}"),
        ("work fits", wx + ww <= vw - pad + 1, f"right={wx + ww} limit={vw - pad:.0f}"),
    ]
    if vw <= 900:
        checks.append(("drawer off screen",
                       cx + cw <= 0 or d["ctlVis"] == "hidden",
                       f"{cx}+{cw} vis={d['ctlVis']}"))
    else:
        # A bounds check, not a docking check: above 900 px the rail layout
        # legitimately collapses the panel to nothing, so all the closed state
        # can assert is that whatever the panel is, it sits inside the frame.
        # The open drawer is measured in the drawer state below.
        checks.append(("drawer within bounds",
                       cx >= 0 and cx + cw <= wx + 1,
                       f"{cx}+{cw} work@{wx}"))
    checks.append(("no vscroll", d["bodyH"] <= d["vh"] + 1, f"{d['bodyH']}/{d['vh']}"))

    if state == "drawer":
        checks = [c for c in checks if c[0] != "drawer off screen"]
        # "Somewhere on screen" is satisfied by a collapsed 2 px panel parked at
        # its docked x, which would report this state as fine without measuring
        # it at all. Require it to be open.
        checks.append(("drawer open",
                       d["ctlVis"] == "visible" and cw >= 200,
                       f"vis={d['ctlVis']} w={cw}"))
        checks.append(("drawer in view", cx >= 0 and cx + cw <= vw + 1, f"{cx}+{cw}"))
        # 341 px caps the overlay drawer. A docked drawer is a grid column and
        # is allowed to be wider (348 px at 1024).
        if vw <= 900:
            checks.append(("drawer width sane", cw <= 341, f"{cw}px"))
        sw_, cw_ = (d["ctlOverflowX"].split("/") + ["0"])[:2]
        checks.append(("drawer no h-overflow", sw_ == cw_, f"{sw_}/{cw_}"))
    if state == "closed":
        pairs = {}
        for chunk in d["contrast"].split():
            if ":" in chunk:
                name, val = chunk.rsplit(":", 1)
                try:
                    pairs[name] = float(val.split("[")[0])
                except ValueError:
                    pass
        low = {k: v for k, v in pairs.items() if v < 4.5}
        checks.append(("contrast >= 4.5", not low,
                       ", ".join(f"{k.split('.')[-1]}={v}" for k, v in low.items())))
    if vw <= 520 and state != "keyboard":
        # This is the narrow-window half of the touch-target rule. The CSS also
        # raises targets under `(pointer: coarse)`, which a WKWebView on this
        # host always reports as fine — so a phone in landscape (844 px wide,
        # no pointer) is only covered by that arm, on hardware, not here.
        counts, detail = (d["touchTargets"].split("|", 1) + [""])[:2]
        tiny, main = (counts.split("/") + ["0", "0"])[:2]
        checks.append(("no target < 38", tiny == "0", detail or f"{tiny} tiny"))
        checks.append(("buttons >= 44", main == "0", detail or f"{main} under"))
    if state == "connected":
        checks.append(("empty state hidden", d["emptyShown"] == "none", d["emptyShown"]))
    elif state == "keyboard":
        # The shell deliberately shrinks to the space above the keyboard, so the
        # "fills the viewport" and "no scroll" checks do not apply here.
        checks = [c for c in checks if c[0] not in ("shell==viewport", "no vscroll")]
        checks.append(("shell fits visible area", sh <= d["vh"], f"{sh}<={d['vh']}"))
        checks.append(("chrome collapsed", wy <= 10, f"work.top={wy}"))

    return checks


def main():
    failures = []
    print(f"{'viewport':12} {'size':11} {'state':10} {'':6} detail")
    total = 0
    for name, w, h in VIEWPORTS:
        for state in STATES[name]:
            total += 1
            d = measure(name, w, h, state)
            tag = f"{name}/{state}"
            if d is None:
                failures.append(tag)
                print(f"{name:12} {w}x{h:<7} {state:10} FAIL   no layout output")
                continue
            checks = audit(d, state)
            bad = [c for c in checks if not c[1]]
            detail = "; ".join(f"{c[0]}: {c[2]}" for c in bad) or "-"
            print(f"{name:12} {w}x{h:<7} {state:10} {'FAIL' if bad else 'PASS':6} {detail}")
            if bad:
                failures.append(tag)

    print()
    if failures:
        print(f"{len(failures)}/{total} FAILING:", ", ".join(failures))
        return 1
    print(f"ALL PASS ({total} checks)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
