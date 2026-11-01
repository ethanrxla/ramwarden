// RamWarden menu-bar app: a thin status item over the daemon's HTTP API.
// It holds no policy and takes no destructive action of its own; the only write
// it ever makes is resuming an app the daemon froze.
import AppKit
import Darwin

let base = URL(string: "http://127.0.0.1:7823")!

struct Snapshot {
    var rung = "unknown"
    var someAvg10 = 0.0
    var availableMB = 0.0
    var usedMB = 0.0
    var totalMB = 0.0
    var percent = 0.0
    var top: [(name: String, mb: Double)] = []
    var actions: [String] = []
    var suspended: [String] = []
}

func fetchJSON(_ path: String) async -> Any? {
    var req = URLRequest(url: base.appendingPathComponent(path))
    req.timeoutInterval = 5
    guard let (data, response) = try? await URLSession.shared.data(for: req),
          (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
    return try? JSONSerialization.jsonObject(with: data)
}

func loadSnapshot(previous: Snapshot?) async -> Snapshot? {
    guard let st = await fetchJSON("companion/state") as? [String:Any],
          st["ok"] as? Bool == true, let total = st["total_mb"] as? Double, total > 0 else { return nil }
    var s = previous ?? Snapshot()
    s.rung = st["rung"] as? String ?? "unknown"
    s.someAvg10 = st["psi_some"] as? Double ?? 0
    s.availableMB = st["available_mb"] as? Double ?? 0
    s.usedMB = st["used_mb"] as? Double ?? 0
    s.totalMB = total
    s.percent = st["percent"] as? Double ?? 0
    if let rows = st["processes"] as? [[String:Any]] {
        s.top = rows.map { ($0["name"] as? String ?? "?",$0["rss_mb"] as? Double ?? 0) }
    }
    if let names = st["suspended"] as? [String] { s.suspended = names }
    return s
}

func gb(_ mb: Double) -> String { String(format: "%.1f GB", mb / 1024) }

final class App: NSObject, NSApplicationDelegate, NSMenuDelegate {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    let menu = NSMenu()
    var snap: Snapshot?
    let companion = Companion()
    var refreshing = false
    var failedRefreshes = 0

    func applicationDidFinishLaunching(_ n: Notification) {
        item.autosaveName = "RamWarden"
        menu.delegate = self
        item.menu = menu
        item.button?.title = "RW …"
        rebuild()
        companion.restore()
        Task { await refresh() }
        Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { _ in
            Task { await self.refresh() }
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        companion.openChat()
        return true
    }

    func refresh() async {
        guard !refreshing else { return }; refreshing = true
        let s = await loadSnapshot(previous:snap)
        await MainActor.run {
            if let s = s { self.snap = s; self.failedRefreshes = 0 }
            else { self.failedRefreshes += 1 }
            if self.failedRefreshes >= 3 { self.snap = nil }
            self.updateTitle(); self.companion.model.connected = self.snap != nil
            self.refreshing = false
        }
    }

    func updateTitle() {
        guard let s = snap else { item.button?.title = "RW ⚠︎"; return }
        let dot: String
        switch s.rung {
        case "release", "hold": dot = s.someAvg10 > 2 ? "🟡" : "🟢"
        case "reclaim", "pageout": dot = "🟡"
        case "tabs": dot = "🟠"
        default: dot = "🔴"
        }
        item.button?.title = "\(dot) \(Int(s.percent))%"
    }

    func menuWillOpen(_ menu: NSMenu) {
        Task { await refresh(); await MainActor.run { self.rebuild() } }
        rebuild()
    }

    func add(_ title: String, _ action: Selector? = nil, key: String = "", enabled: Bool = true) {
        let m = NSMenuItem(title: title, action: action, keyEquivalent: key)
        m.target = self
        m.isEnabled = enabled && (action != nil)
        menu.addItem(m)
    }

    func header(_ title: String) {
        let m = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        m.attributedTitle = NSAttributedString(string: title, attributes: [
            .font: NSFont.systemFont(ofSize: 11, weight: .semibold),
            .foregroundColor: NSColor.secondaryLabelColor])
        menu.addItem(m)
    }

    func rebuild() {
        menu.removeAllItems()
        add(companion.visible ? "Hide ram companion" : "Show ram companion", #selector(toggleCompanion))
        add("Open assistant", #selector(openAssistant))
        add("Customize ram colors…", #selector(customizeRam))
        menu.addItem(.separator())
        guard let s = snap else {
            add("RamWarden daemon not reachable", nil)
            menu.addItem(.separator())
            add("Start daemon", #selector(startDaemon))
            add("Quit", #selector(quit), key: "q")
            return
        }
        add("Pressure: \(s.rung)  (some \(String(format: "%.1f", s.someAvg10))%)", nil)
        add("Memory: \(gb(s.usedMB)) of \(gb(s.totalMB)) used · \(gb(s.availableMB)) available", nil)

        menu.addItem(.separator())
        header("TOP MEMORY")
        for p in s.top { add("\(p.name) — \(Int(p.mb)) MB", nil) }

        if !s.suspended.isEmpty {
            menu.addItem(.separator())
            header("FROZEN BY RAMWARDEN")
            for n in s.suspended {
                let m = NSMenuItem(title: "Resume \(n)", action: #selector(resume(_:)), keyEquivalent: "")
                m.target = self; m.representedObject = n
                menu.addItem(m)
            }
        }

        menu.addItem(.separator())
        header("RECENT ACTIONS")
        if s.actions.isEmpty { add("None — nothing has needed doing", nil) }
        for a in s.actions { add(a, nil) }

        menu.addItem(.separator())
        add("Open Activity Monitor", #selector(openActivityMonitor))
        add("Open daemon log", #selector(openLog))
        add("Quit", #selector(quit), key: "q")
    }

    @objc func resume(_ sender: NSMenuItem) {
        guard let name = sender.representedObject as? String,
              let enc = name.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) else { return }
        var req = URLRequest(url: base.appendingPathComponent("resume/\(enc)"))
        req.httpMethod = "POST"
        URLSession.shared.dataTask(with: req) { _, _, _ in
            Task { await self.refresh() }
        }.resume()
    }

    @objc func startDaemon() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        p.arguments = ["kickstart", "gui/\(getuid())/com.ramwarden.daemon"]
        try? p.run()
    }

    @objc func openActivityMonitor() {
        NSWorkspace.shared.open(URL(fileURLWithPath: "/System/Applications/Utilities/Activity Monitor.app"))
    }

    @objc func openLog() {
        NSWorkspace.shared.open(FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/ramwarden.log"))
    }

    @objc func toggleCompanion() { companion.toggle(); rebuild() }
    @objc func openAssistant() { companion.openChat() }
    @objc func customizeRam() { companion.openColors() }

    @objc func quit() { NSApp.terminate(nil) }
}

// A per-user lock also covers login launch plus a manual reopen.
let lockPath = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Caches/com.ramwarden.menubar.lock").path
let instanceLock = open(lockPath, O_CREAT | O_RDWR, 0o600)
if instanceLock < 0 || flock(instanceLock, LOCK_EX | LOCK_NB) != 0 { exit(0) }
// macOS puts a new status item at the far left of the extras, which on a notched
// MacBook with a busy menu bar is behind the notch, where it is never drawn. Start
// it near the clock instead (points from the right edge); a ⌘-drag still moves it
// and AppKit saves that over this.
let positionKey = "NSStatusItem Preferred Position RamWarden"
if UserDefaults.standard.object(forKey: positionKey) == nil {
    UserDefaults.standard.set(300.0, forKey: positionKey)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)   // no Dock icon
let delegate = App()
app.delegate = delegate
app.run()
