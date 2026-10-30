// RamWarden menu-bar app: a thin status item over the daemon's HTTP API.
// It holds no policy and takes no destructive action of its own; the only write
// it ever makes is resuming an app the daemon froze.
import AppKit

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
    req.timeoutInterval = 2
    guard let (data, _) = try? await URLSession.shared.data(for: req) else { return nil }
    return try? JSONSerialization.jsonObject(with: data)
}

func loadSnapshot() async -> Snapshot? {
    guard let psi = await fetchJSON("psi") as? [String: Any] else { return nil }
    var s = Snapshot()
    s.rung = psi["rung"] as? String ?? "unknown"
    s.availableMB = psi["available_mb"] as? Double ?? 0
    s.someAvg10 = (psi["some"] as? [String: Any])?["avg10"] as? Double ?? 0

    if let st = await fetchJSON("stats") as? [String: Any] {
        s.usedMB = st["used_mb"] as? Double ?? 0
        s.totalMB = st["total_mb"] as? Double ?? 0
        s.percent = st["percent"] as? Double ?? 0
        let procs = st["processes"] as? [[String: Any]] ?? []
        s.top = procs.prefix(6).map { ($0["name"] as? String ?? "?", $0["rss_mb"] as? Double ?? 0) }
    }
    if let a = await fetchJSON("actions") as? [String: Any],
       let list = a["actions"] as? [[String: Any]] {
        s.actions = list.prefix(5).map {
            let at = String(($0["at"] as? String ?? "").suffix(8).prefix(5))
            let ok = ($0["succeeded"] as? Bool ?? false) ? "✓" : "✗"
            return "\(at)  \($0["action"] as? String ?? "?")  \($0["target"] as? String ?? "")  \(ok)"
        }
    }
    if let sus = await fetchJSON("suspended") as? [[String: Any]] {
        s.suspended = sus.compactMap { $0["name"] as? String }
    }
    return s
}

func gb(_ mb: Double) -> String { String(format: "%.1f GB", mb / 1024) }

final class App: NSObject, NSApplicationDelegate, NSMenuDelegate {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    let menu = NSMenu()
    var snap: Snapshot?

    func applicationDidFinishLaunching(_ n: Notification) {
        menu.delegate = self
        item.menu = menu
        item.button?.title = "RW …"
        rebuild()
        Task { await refresh() }
        Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { _ in
            Task { await self.refresh() }
        }
    }

    func refresh() async {
        let s = await loadSnapshot()
        await MainActor.run { self.snap = s; self.updateTitle() }
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

    @objc func quit() { NSApp.terminate(nil) }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)   // no Dock icon
let delegate = App()
app.delegate = delegate
app.run()
