#!/usr/bin/env swift
// Snapshot a page in an offscreen WKWebView at an exact CSS viewport.
//
// This exists because headless Chrome is unreliable in this sandbox and, more
// importantly, because Chrome is not the renderer the app actually ships with:
// Android uses Chrome, but iOS and HarmonyOS both run WebKit-derived engines.
// Taking the shot with WKWebView means the width, the font metrics and the
// safe-area behaviour match iOS more closely than a Chrome capture would.
//
//   swift tools/webview_shot.swift <url> <out.png> <width> <height> <dark|light> [panel]
//
// LIMITATION: this WebView is driven by a run loop, not by the compositor, so
// style changes made after load are not necessarily repainted. Inline styles,
// data-theme toggles and class changes can all leave existing elements showing
// their first computed values, while freshly created elements report the new
// ones. Snapshot therefore reliably captures the FIRST frame only.
// To review a theme, make it the first frame (e.g. set data-theme in the HTML
// before loading) rather than switching it from JavaScript.
//
// Example:
//   swift tools/webview_shot.swift http://127.0.0.1:8778/index.html \
//     /tmp/phone.png 390 844 dark closed

import Cocoa
import WebKit

let args = CommandLine.arguments
guard args.count >= 6 else {
    FileHandle.standardError.write("usage: webview_shot <url> <out.png> <w> <h> <dark|light> [panel]\n".data(using: .utf8)!)
    exit(1)
}
let urlString = args[1]
let outPath = args[2]
let width = CGFloat(Double(args[3]) ?? 390)
let height = CGFloat(Double(args[4]) ?? 844)
let theme = args[5]
let panel = args.count > 6 ? args[6] : "closed"

guard let url = URL(string: urlString) else {
    FileHandle.standardError.write("bad url\n".data(using: .utf8)!)
    exit(1)
}

// WKWebView keeps a process-wide HTTP cache; a non-persistent data store does
// not touch it, so sub-resources (the stylesheet) kept coming back stale.
URLCache.shared.removeAllCachedResponses()
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)

final class Shooter: NSObject, WKNavigationDelegate {
    var web: WKWebView!
    // An offscreen WKWebView never enters the render loop: existing elements keep
    // their first computed style, inline style changes are ignored and the
    // snapshot returns the first frame. Parking it in a real (borderless) window
    // is what makes later DOM mutations actually paint.
    var window: NSWindow!
    let outPath: String
    // Stored as properties: a bare `panel` inside a method resolves to AppKit's
    // global and fails to compile, and init parameters are out of scope there.
    let themeName: String
    let panelState: String
    let stateScript: String

    init(url: URL, width: CGFloat, height: CGFloat, outPath: String, theme: String, panel: String) {
        self.outPath = outPath
        self.themeName = theme
        self.panelState = panel
        self.stateScript = Shooter.fixture(for: panel)
        super.init()
        let config = WKWebViewConfiguration()
        // A persistent store kept serving the previous stylesheet, which made
        // every iteration after the first review the build before it.
        config.websiteDataStore = .nonPersistent()
        let seed = """
        try { localStorage.setItem('linkr-theme','\(theme)');
              localStorage.setItem('linkr-sidebar','\(panel)'); } catch (e) {}
        """
        config.userContentController.addUserScript(
            WKUserScript(source: seed, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        web = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: height), configuration: config)
        web.navigationDelegate = self
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: width, height: height),
                          styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = web
        window.backgroundColor = .black
        window.orderFrontRegardless()
        let request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData,
                               timeoutInterval: 20)
        web.load(request)
    }


    static func fixture(for state: String) -> String {
        switch state {
        case "drawer":
            // Kill the transform transition first: this process never drives
            // animation frames, so a transition would pin the rect at its start
            // value and every drawer assertion would read the closed geometry.
            // `sidebar-open` alone only reaches the overlay drawer (<=900 px);
            // above that the panel is a docked column hidden by
            // `controls-hidden`, which is what the panel toggle removes.
            return """
            var c = document.querySelector('.controls');
            if (c) c.style.transition = 'none';
            document.querySelector('.app-shell')?.classList.add('sidebar-open');
            document.querySelector('.app-shell')?.classList.remove('controls-hidden');
            """
        case "keyboard":
            return """
            document.documentElement.classList.add('soft-keyboard-open');
            document.documentElement.style.setProperty('--terminal-viewport-height', Math.round(innerHeight * 0.55) + 'px');
            """
        case "connected":
            return """
            document.querySelector('.terminal-output')?.classList.add('connected');
            var h = document.getElementById('terminalInputHint');
            if (h) h.textContent = 'Modifiers apply once';
            """
        case "drawer+connected":
            return Shooter.fixture(for: "drawer") + Shooter.fixture(for: "connected")
        default:
            return ""
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // WKWebView caches sub-resources far harder than the top document, and
        // neither a non-persistent store nor URLCache eviction defeated it. Re-request
        // every stylesheet with a fresh query so the capture always reflects disk.
        webView.evaluateJavaScript("""
        document.querySelectorAll('link[rel=stylesheet]').forEach(function (l) {
          l.href = l.href.split('?')[0] + '?cb=' + Date.now();
        });
        """) { _, _ in }
        // The localStorage seed runs at document start, before the origin is
        // established, so it silently no-ops. Force the state on the DOM just
        // before the shot instead, after app.js has finished its own pass.
        webView.evaluateJavaScript(
            "document.documentElement.setAttribute('data-theme','\(themeName)');"
        )
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.2) {
            // `panelState` doubles as the state fixture for audits: the DOM is
            // forced into the state under test instead of clicking through to it,
            // because clicks are not repainted in this process. Rects are still
            // correct after a mutation, so layout assertions remain valid.
            webView.evaluateJavaScript(
                "document.documentElement.setAttribute('data-theme','\(self.themeName)');"
                + self.stateScript
            ) { _, error in
                if let error { print("set failed: \(error)") }
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
            webView.evaluateJavaScript("""
            (function () {
              var r = function (s) {
                var e = document.querySelector(s);
                if (!e) return 'missing';
                var b = e.getBoundingClientRect();
                return Math.round(b.width) + 'x' + Math.round(b.height) + '@' + Math.round(b.left) + ',' + Math.round(b.top);
              };
              var cs2 = function (sel, prop) {
                var e = document.querySelector(sel);
                return e ? getComputedStyle(e)[prop] : 'missing';
              };
              var sh = document.querySelector('.app-shell');
              var cs = sh ? getComputedStyle(sh) : {};
              return JSON.stringify({
                vw: innerWidth, vh: innerHeight,
                theme: document.documentElement.getAttribute('data-theme'),
                bgVar: getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(),
                shell: r('.app-shell'),
                padL: cs.paddingLeft, padR: cs.paddingRight, padT: cs.paddingTop,
                cols: cs.gridTemplateColumns,
                work: r('.workpanel'), ctl: r('.controls'),
                ctlTf: cs2('.controls', 'transform'),
                ctlVis: cs2('.controls', 'visibility'),
                ctlPos: cs2('.controls', 'position'),
                ctlW: cs2('.controls', 'width'),
                bodyH: document.body.scrollHeight,
                shellClass: (document.querySelector('.app-shell') || {}).className || '',
                emptyShown: (function () {
                  var e = document.querySelector('.terminal-empty');
                  return e ? getComputedStyle(e).display : 'missing';
                })(),
                touchTargets: (function () {
                  // 44 px is the target for a standalone button; segmented
                  // controls and tabs are allowed 38, which is what iOS itself
                  // uses for a segmented control.
                  var small = [], main = [];
                  var nodes = document.querySelectorAll('button, .preset-chip, .rail-btn');
                  for (var i = 0; i < nodes.length; i++) {
                    var b = nodes[i];
                    if (b.hidden || b.disabled) continue;
                    var r = b.getBoundingClientRect();
                    if (r.width === 0 || r.height === 0) continue;
                    var id = b.id || (b.className || '').split(' ')[0] || b.textContent.trim().slice(0, 10);
                    var tag = id + ':' + Math.round(r.width) + 'x' + Math.round(r.height);
                    if (r.width < 38 || r.height < 38) small.push(tag);
                    else if ((b.classList.contains('btn') || b.classList.contains('icon-btn')) &&
                             (r.width < 44 || r.height < 44)) main.push(tag);
                  }
                  return small.length + '/' + main.length + '|' +
                         small.concat(main).slice(0, 12).join(', ');
                })(),
                contrast: (function () {
                  var parse = function (c) {
                    var i = c.indexOf('('), j = c.indexOf(')');
                    if (i < 0 || j < 0) return null;
                    var p = c.slice(i + 1, j).split(',').map(Number);
                    return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
                  };
                  var bgOf = function (el) {
                    var e = el;
                    while (e && e !== document.documentElement) {
                      var c = parse(getComputedStyle(e).backgroundColor);
                      if (c && c.a > 0.55) return c;
                      e = e.parentElement;
                    }
                    // Fall back to the real body colour; a hardcoded dark value
                    // silently passed in dark mode and reported nonsense in light.
                    var body = parse(getComputedStyle(document.body).backgroundColor);
                    return body && body.a > 0.05 ? body : { r: 5, g: 8, b: 13, a: 1 };
                  };
                  var lum = function (c) {
                    var f = function (v) { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
                    return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
                  };
                  var ratio = function (a, b) {
                    var la = lum(a), lb = lum(b);
                    return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
                  };
                  // :last-child matters — the first span inside these headings is
                  // the coloured dot, which is deliberately near the background.
                  var sels = ['.te-title', '.te-hint', '.term-title span:last-child',
                              '.statusbar .sb-item', '.settings-tab', '.btn-label',
                              '.card-title span:last-child', '.preset-chip',
                              '.support-text', '.terminal-input-hint'];
                  var out = [];
                  for (var i = 0; i < sels.length; i++) {
                    var el = document.querySelector(sels[i]);
                    if (!el) continue;
                    var fg = parse(getComputedStyle(el).color);
                    if (!fg) continue;
                    var name = sels[i].split('.').join('').split(' ').join('');
                    var bg = bgOf(el);
                    out.push(name + ':' + ratio(fg, bg).toFixed(1) + '[' + fg.r + ',' + fg.g + ',' + fg.b + '|' + bg.r + ',' + bg.g + ',' + bg.b + ']');
                  }
                  return out.join(' ');
                })(),
                bgChain: (function () {
                  var out = [], e = document.querySelector('.term-title');
                  while (e && out.length < 7) {
                    out.push(((e.className || e.tagName) + '').split(' ')[0].slice(0, 16) +
                             '=' + getComputedStyle(e).backgroundColor);
                    e = e.parentElement;
                  }
                  return out.join(' > ');
                })(),
                clippedText: (function () {
                  var out = [];
                  var nodes = document.querySelectorAll(
                    '.controls .btn-label, .controls .card-title span, .settings-tab, .term-title, .statusbar .sb-item, .preset-chip');
                  for (var i = 0; i < nodes.length; i++) {
                    var e = nodes[i];
                    if (e.scrollWidth > e.clientWidth + 1) {
                      out.push((e.className || e.tagName) + ':' + e.scrollWidth + '/' + e.clientWidth);
                    }
                  }
                  return out.length + '|' + out.slice(0, 8).join(', ');
                })(),
                ctlOverflowX: (function () {
                  var e = document.querySelector('.controls');
                  return e ? (e.scrollWidth + '/' + e.clientWidth) : 'missing';
                })()
              });
            })()
            """) { value, _ in print("layout: \(value ?? "nil")") }
        }
        // Force a synchronous reflow and let fonts settle: capturing before the
        // grid has finished resolving produced a frame with a 390px dead band on
        // the right even though the computed layout was already correct.
        DispatchQueue.main.asyncAfter(deadline: .now() + 4.6) {
            webView.evaluateJavaScript("void document.body.offsetHeight;") { _, _ in }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 5.2) {
            let snapshot = WKSnapshotConfiguration()
            // Offscreen WKWebView does not repaint on its own: without this the
            // capture returns the frame from before the theme attribute changed.
            snapshot.afterScreenUpdates = true
            snapshot.rect = CGRect(x: 0, y: 0, width: webView.bounds.width, height: webView.bounds.height)
            webView.takeSnapshot(with: snapshot) { image, error in
                guard let image,
                      let tiff = image.tiffRepresentation,
                      let rep = NSBitmapImageRep(data: tiff),
                      let png = rep.representation(using: .png, properties: [:]) else {
                    FileHandle.standardError.write("snapshot failed: \(String(describing: error))\n".data(using: .utf8)!)
                    exit(2)
                }
                try? png.write(to: URL(fileURLWithPath: self.outPath))
                print("\(self.outPath)  \(Int(image.size.width))x\(Int(image.size.height))")
                exit(0)
            }
        }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        FileHandle.standardError.write("navigation failed: \(error.localizedDescription)\n".data(using: .utf8)!)
        exit(3)
    }
}

let shooter = Shooter(url: url, width: width, height: height, outPath: outPath, theme: theme, panel: panel)
DispatchQueue.main.asyncAfter(deadline: .now() + 20) {
    FileHandle.standardError.write("timed out\n".data(using: .utf8)!)
    exit(4)
}
withExtendedLifetime(shooter) { app.run() }
