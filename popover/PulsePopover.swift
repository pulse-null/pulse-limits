// PulsePopover: a borderless, non-activating panel under the menu bar that
// shows the monitor page. SwiftBar's own webview popover paints a
// "SwiftBar: <plugin>" title bar we cannot remove; this one has no chrome.
//
//   pulse-popover [width] [height]
//
// Resident: the first launch shows the panel; SIGUSR1 toggles it; a click
// outside, Escape, or a second click on the menu bar item hides it. While
// hidden the page is unloaded (no animation, little memory), and after
// PULSE_IDLE_EXIT seconds hidden (default 600) the process exits so nothing
// stays around when you are not looking. Reads $XDG_CACHE_HOME/pulse-limits/panel.url
// (default ~/.cache/pulse-limits) on every show, so it always opens the latest
// data the bar wrote.
//
// Everything it knows comes from the Rust binary next to it (bin/pulse-limits):
// on show and every 2 min while shown it runs `pulse-limits payload` (a fresh
// reading, JSON only; the providers keep their API throttle) and hands the
// result to window.pulse.usage({...}), so the page updates in place. Every 2 s
// it runs `pulse-limits activity` (what Claude Code is doing right now, from its
// transcripts) and, every 10 s while the reading is Claude's, `pulse-limits
// estimate PCT FETCHED` (the dead-reckoned session %), and hands them to
// window.pulse.activity({...}). This file is only the window.
// Build: ./build.sh

import Cocoa
import WebKit

let env = ProcessInfo.processInfo.environment
func envPath(_ name: String) -> String? { env[name].flatMap { $0.isEmpty ? nil : $0 } }   // empty means unset, as in the shell
let cacheDir  = envPath("XDG_CACHE_HOME").map { $0 + "/pulse-limits" } ?? NSString(string: "~/.cache/pulse-limits").expandingTildeInPath
let urlFile   = cacheDir + "/panel.url"
let pidFile   = cacheDir + "/popover.pid"
let stampFile = cacheDir + "/popover.closed"   // "hidden at" epoch, read by `pulse-limits open`
let argv = CommandLine.arguments
let width  = Double(argv.count > 1 ? argv[1] : "") ?? 520
let height = Double(argv.count > 2 ? argv[2] : "") ?? 316
let idleExit = TimeInterval(env["PULSE_IDLE_EXIT"] ?? "") ?? 600
let pulseBin = Bundle.main.executableURL!.resolvingSymlinksInPath()
    .deletingLastPathComponent().appendingPathComponent("pulse-limits").path

// ---- the binary: one JSON document per run ------------------------------------------
func pulse(_ args: [String]) -> Data? {
    guard FileManager.default.isExecutableFile(atPath: pulseBin) else { return nil }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: pulseBin)
    p.arguments = args
    let pipe = Pipe()
    p.standardOutput = pipe
    p.standardError = FileHandle.nullDevice
    do { try p.run() } catch { return nil }
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    p.waitUntilExit()
    guard p.terminationStatus == 0, let text = String(data: data, encoding: .utf8), text.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("{") else { return nil }
    return data
}

final class Panel: NSPanel {
    override var canBecomeKey: Bool { true }     // so Escape reaches us without activating the app
}

final class App: NSObject, NSApplicationDelegate, WKNavigationDelegate {
    var panel: Panel!
    var web: WKWebView!
    var monitors: [Any] = []
    var signalSource: DispatchSourceSignal?
    var visible = false
    var hiddenAt = Date()
    var activityTimer: Timer?
    var activeProvider = "claude"          // whose activity the panel's live lane follows
    var usageTimer: Timer?
    var fetching = false
    var measuring = false
    var lastReading: (pct: Double, fetched: Int)?
    var lastEstimate: [String: Any]?
    var lastEstimateAt = Date.distantPast

    func applicationDidFinishLaunching(_: Notification) {
        try? String(getpid()).write(toFile: pidFile, atomically: true, encoding: .utf8)

        panel = Panel(contentRect: NSRect(x: 0, y: 0, width: width, height: height),
                      styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.level = .popUpMenu
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.canJoinAllSpaces, .transient, .ignoresCycle]

        web = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: height))
        web.navigationDelegate = self
        web.setValue(false, forKey: "drawsBackground")   // no white flash before the page paints
        web.wantsLayer = true
        web.layer?.cornerRadius = 16
        web.layer?.masksToBounds = true
        web.autoresizingMask = [.width, .height]
        panel.contentView = web

        signal(SIGUSR1, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
        source.setEventHandler { self.toggle() }
        source.resume()
        signalSource = source
        signal(SIGTERM) { _ in unlink(pidFile); exit(0) }

        Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { _ in
            if !self.visible, Date().timeIntervalSince(self.hiddenAt) > idleExit { self.quit() }
        }
        show()
    }

    func toggle() { visible ? hide() : show() }

    func show() {
        guard !visible,
              let text = try? String(contentsOfFile: urlFile, encoding: .utf8),
              let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines)) else { return }
        // Under the mouse, which is on the menu bar item that asked for us.
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { NSMouseInRect(mouse, $0.frame, false) } ?? NSScreen.main!
        let area = screen.visibleFrame
        let x = max(area.minX + 8, min(mouse.x - width / 2, area.maxX - width - 8))
        panel.setFrame(NSRect(x: x, y: area.maxY - 6 - height, width: width, height: height), display: false)
        panel.alphaValue = 0
        web.load(URLRequest(url: url))                  // fades in from didFinish
    }

    func webView(_: WKWebView, didFinish _: WKNavigation!) {
        guard !visible, web.url?.isFileURL == true else { return }   // the about:blank unload also lands here
        visible = true
        panel.makeKeyAndOrderFront(nil)
        NSAnimationContext.runAnimationGroup { ctx in ctx.duration = 0.12; self.panel.animator().alphaValue = 1 }
        monitors = [
            NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { _ in self.hide() }!,
            NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
                if event.keyCode == 53 { self.hide(); return nil }
                return event
            }!,
        ]
        pushActivity()
        activityTimer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { _ in self.pushActivity() }
        refreshUsage()
        usageTimer = Timer.scheduledTimer(withTimeInterval: 120, repeats: true) { _ in self.refreshUsage() }
        print("shown"); fflush(stdout)
    }

    func refreshUsage() {
        guard visible, !fetching else { return }
        fetching = true
        DispatchQueue.global(qos: .utility).async {
            defer { DispatchQueue.main.async { self.fetching = false } }
            guard let data = pulse(["payload"]), let json = String(data: data, encoding: .utf8) else { return }
            DispatchQueue.main.async {
                guard self.visible else { return }
                if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                    self.activeProvider = obj["provider"] as? String ?? "claude"
                    // Dead reckoning is calibrated on Claude Code's transcripts: only Claude's session gets it.
                    if (obj["provider"] as? String ?? "claude") == "claude",
                       let fetched = obj["fetched"] as? Int,
                       let windows = obj["windows"] as? [[String: Any]],
                       let session = windows.first(where: { ($0["label"] as? String) == "SESSION" }),
                       let pct = session["pct"] as? Double ?? (session["pct"] as? Int).map(Double.init) {
                        self.lastReading = (pct, fetched)
                    } else {
                        self.lastReading = nil; self.lastEstimate = nil
                    }
                    self.lastEstimateAt = Date.distantPast
                }
                self.web.evaluateJavaScript("window.pulse && window.pulse.usage(\(json))", completionHandler: nil)
                self.pushActivity()
                print("usage refreshed"); fflush(stdout)
            }
        }
    }

    func pushActivity() {
        guard visible, !measuring else { return }
        measuring = true
        let reading = lastReading
        let provider = activeProvider
        let estimateDue = Date().timeIntervalSince(lastEstimateAt) >= 10   // the estimate scans minutes of transcripts: every 10 s is plenty
        DispatchQueue.global(qos: .utility).async {
            defer { DispatchQueue.main.async { self.measuring = false } }
            guard let data = pulse(["activity", provider]),
                  var payload = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return }
            var estimate: [String: Any]? = nil
            if let r = reading, estimateDue,
               let e = pulse(["estimate", String(r.pct), String(r.fetched)]),
               let obj = (try? JSONSerialization.jsonObject(with: e)) as? [String: Any] { estimate = obj }
            DispatchQueue.main.async {
                guard self.visible else { return }
                if let e = estimate { self.lastEstimate = e; self.lastEstimateAt = Date() }
                if reading != nil, let e = self.lastEstimate { payload["estimate"] = e }
                guard let out = try? JSONSerialization.data(withJSONObject: payload), let json = String(data: out, encoding: .utf8) else { return }
                self.web.evaluateJavaScript("window.pulse && window.pulse.activity(\(json))", completionHandler: nil)
            }
        }
    }

    func hide() {
        guard visible else { return }
        visible = false
        hiddenAt = Date()
        activityTimer?.invalidate(); activityTimer = nil
        usageTimer?.invalidate(); usageTimer = nil
        monitors.forEach { NSEvent.removeMonitor($0) }
        monitors = []
        // The launcher reads this: a click on the menu bar item both hides us (the
        // global monitor fires first) and re-runs the launcher, which must not toggle us back.
        try? String(Date().timeIntervalSince1970).write(toFile: stampFile, atomically: true, encoding: .utf8)
        NSAnimationContext.runAnimationGroup({ ctx in ctx.duration = 0.1; self.panel.animator().alphaValue = 0 },
                                            completionHandler: {
            self.panel.orderOut(nil)
            self.web.load(URLRequest(url: URL(string: "about:blank")!))   // stop the animation, drop the page
            print("hidden"); fflush(stdout)
        })
    }

    func quit() {
        unlink(pidFile)
        exit(0)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)      // no Dock icon, no menu bar of its own
let delegate = App()
app.delegate = delegate
app.run()
