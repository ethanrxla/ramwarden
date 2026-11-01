// The wardens in the menu bar: a RAM pane (the memory menu in main.swift) and
// a ThreatWarden pane, switched by a segmented control at the top of the menu.
//
// This mirrors crates/ramwarden-ui/src/wardens.rs, which the Linux and Windows
// windows share: the same level words and colours, the same "one button"
// rule (Get a pass when locked, Turn on when unlocked but not installed), and
// the same engine labels for alerts. Contract: docs/specs/wardens.md.
//
// ThreatWarden shows what Wazuh, ClamAV, Suricata and the network view report,
// in RamWarden's own menu. None of their dashboards is opened.
import AppKit

enum Pane: Int { case ram = 0, threat = 1 }

/// Where "Get a pass" goes and what it costs, until the daemon names its own.
let upgradeFallbackURL = URL(string: "https://ramwarden.app/#pricing")!
let threatFallbackPrice = "In RamWarden Pro, from $5 a month"

struct ThreatEngine { var name: String; var state: String; var detail: String }

struct ThreatAlert {
    var tag: String
    var level: String          // the pane level whose colour the tag uses
    var engine: String
    var when: String
    var title: String
    var explanation: String
    var advice: String
}

struct ThreatPane {
    var name = "ThreatWarden"
    var level = "off"
    var headline = ""
    var detail = ""
    var summary: String?
    var upgrade: (url: URL, price: String)?
    var turnOn = false
    var engines: [ThreatEngine] = []
    var alerts: [ThreatAlert] = []
    var agents: [String] = []
    var problems: [String] = []
    var iconPath: String? = "wardens/threat/icon.svg"

    static func unavailable(_ headline: String, _ detail: String) -> ThreatPane {
        var p = ThreatPane(); p.headline = headline; p.detail = detail; return p
    }
}

func levelLabel(_ level: String) -> String {
    switch level {
    case "clear": return "All clear"
    case "watch": return "Worth a look"
    case "alert": return "Act now"
    case "off": return "Off"
    case "locked": return "Locked"
    case "setup": return "Setting up"
    default: return "Unknown"
    }
}

/// The process legend's greens, ambers and reds; brand crimson for "locked".
func levelColour(_ level: String) -> NSColor {
    func rgb(_ v: Int) -> NSColor {
        NSColor(srgbRed: CGFloat((v >> 16) & 0xff) / 255, green: CGFloat((v >> 8) & 0xff) / 255,
                blue: CGFloat(v & 0xff) / 255, alpha: 1)
    }
    switch level {
    case "clear": return rgb(0x4ade80)
    case "watch": return rgb(0xfacc15)
    case "alert": return rgb(0xf87171)
    case "setup": return rgb(0x60a5fa)
    case "locked": return rgb(0xf24455)
    default: return rgb(0x9ca3af)
    }
}

func engineLevel(_ state: String) -> String {
    switch state {
    case "on": return "clear"
    case "error": return "watch"
    case "planned": return "setup"
    default: return "off"
    }
}

/// Which engine raised an alert, by its Wazuh rule groups.
func engineOf(_ groups: [String]) -> String {
    let g = Set(groups.map { $0.lowercased() })
    if !g.isDisjoint(with: ["clamav", "freshclam", "virus"]) { return "ClamAV" }
    if !g.isDisjoint(with: ["suricata", "ids"]) { return "Suricata" }
    if !g.isDisjoint(with: ["syscheck", "fim"]) { return "File integrity" }
    if g.contains("rootcheck") { return "Rootkit check" }
    if g.contains("sca") { return "Config check" }
    if g.contains("vulnerability-detector") { return "Vulnerable software" }
    return "Wazuh"
}

/// "2026-11-01T13:05:44Z" -> "11-01 13:05".
func shortTime(_ t: String) -> String {
    let c = Array(t)
    guard c.count >= 16, c[10] == "T" || c[10] == " " else { return t }
    return String(c[5..<10]) + " " + String(c[11..<16])
}

private let engineOrder = [("wazuh", "Wazuh agent"), ("clamav", "ClamAV malware scans"),
                           ("suricata", "Suricata network IDS"), ("network", "Network view")]

/// `/wardens/threat` as the pane shows it. `nil` JSON means the daemon answered
/// /psi but not /wardens: a daemon from before the wardens.
func threatPane(from json: Any?) -> ThreatPane {
    guard let w = json as? [String: Any] else {
        return .unavailable("This RamWarden is older than ThreatWarden",
                            "Update RamWarden to get the ThreatWarden pane.")
    }
    let status = w["status"] as? [String: Any] ?? [:]
    let unlocked = w["unlocked"] as? Bool ?? false
    let enabled = w["enabled"] as? Bool ?? false
    let report = unlocked ? w["report"] as? [String: Any] : nil
    let local = report?["local"] as? [String: Any] ?? [:]
    let installed = local["installed"] as? Bool ?? false

    var p = ThreatPane()
    p.name = w["name"] as? String ?? "ThreatWarden"
    p.level = unlocked ? (status["level"] as? String ?? "off") : "locked"
    p.headline = status["headline"] as? String ?? ""
    p.detail = status["detail"] as? String ?? ""
    if p.detail.isEmpty && !unlocked { p.detail = w["role"] as? String ?? "" }
    p.iconPath = (w["icon"] as? String).map { String($0.drop(while: { $0 == "/" })) } ?? p.iconPath
    if !unlocked {
        let url = (w["upgrade_url"] as? String).flatMap { URL(string: $0) } ?? upgradeFallbackURL
        p.upgrade = (url, w["price"] as? String ?? threatFallbackPrice)
    } else if enabled && p.level == "off" && !installed {
        p.turnOn = true
    }

    guard let r = report else {
        if !unlocked { p.engines = engineOrder.map { ThreatEngine(name: $0.1, state: "locked", detail: "") } }
        return p
    }
    if let s = r["summary"] as? String, !s.isEmpty { p.summary = s }
    let alerts = r["alerts"] as? [[String: Any]] ?? []
    p.alerts = alerts.prefix(8).map { (a: [String: Any]) -> ThreatAlert in
        let lvl = a["level"] as? Int ?? 0
        let verdict = a["verdict"] as? String
        let (tag, colour): (String, String) = {
            switch verdict ?? "" {
            case "threat": return ("threat", "alert")
            case "suspicious": return ("suspicious", "watch")
            case "benign": return ("benign", "clear")
            default: return ("level \(lvl)", lvl >= 12 ? "alert" : "watch")
            }
        }()
        return ThreatAlert(tag: tag, level: colour, engine: engineOf(a["groups"] as? [String] ?? []),
                           when: shortTime(a["timestamp"] as? String ?? ""),
                           title: a["description"] as? String ?? "",
                           explanation: a["explanation"] as? String ?? "",
                           advice: a["advice"] as? String ?? "")
    }
    if let engines = r["engines"] as? [[String: Any]], !engines.isEmpty {
        p.engines = engines.map {
            ThreatEngine(name: $0["name"] as? String ?? "?", state: $0["state"] as? String ?? "off",
                         detail: $0["detail"] as? String ?? "")
        }
    } else {
        // An older daemon: Wazuh from agent health, the rest from their alerts.
        let manager = (r["manager"] as? [String: Any])?["status"] as? String
        let localStatus = local["status"] as? String
        let wazuh: ThreatEngine
        if !installed && manager == nil {
            wazuh = ThreatEngine(name: "Wazuh agent", state: "off", detail: "Not installed")
        } else if localStatus == "connected" || manager == "active" {
            wazuh = ThreatEngine(name: "Wazuh agent", state: "on", detail: "Connected")
        } else {
            let s = (manager ?? localStatus)?.replacingOccurrences(of: "_", with: " ") ?? "connection unknown"
            wazuh = ThreatEngine(name: "Wazuh agent", state: "error", detail: "Installed, \(s)")
        }
        let raised = alerts.map { engineOf($0["groups"] as? [String] ?? []).lowercased() }
        p.engines = [wazuh] + engineOrder.dropFirst().map { (e: (String, String)) -> ThreatEngine in
            let (id, name) = e
            let n = raised.filter { $0 == id }.count
            return n > 0 ? ThreatEngine(name: name, state: "on", detail: "\(n) alert\(n == 1 ? "" : "s") recently")
                         : ThreatEngine(name: name, state: "planned", detail: "Arrives in a later update")
        }
    }
    let me = r["agent_name"] as? String ?? ""
    p.agents = (r["agents"] as? [[String: Any]] ?? []).compactMap { (a: [String: Any]) -> String? in
        guard let n = a["name"] as? String, n != me else { return nil }
        return "\(n): \((a["status"] as? String ?? "?").replacingOccurrences(of: "_", with: " "))"
    }
    p.problems = r["problems"] as? [String] ?? []
    return p
}

// MARK: - The 8-bit icon

/// A warden icon is a pixel sprite: an SVG of axis-aligned <rect>s on a 48x48
/// grid (scripts/warden-sprites.py). Drawing the rects ourselves keeps the
/// pixels square at any size and needs no SVG support from the OS.
func spriteImage(svg: String, size: CGFloat) -> NSImage? {
    func attr(_ tag: String, _ name: String) -> String? {
        guard let r = tag.range(of: " \(name)=\"") else { return nil }
        let rest = tag[r.upperBound...]
        guard let end = rest.firstIndex(of: "\"") else { return nil }
        return String(rest[..<end])
    }
    guard let svgStart = svg.range(of: "<svg"),
          let svgEnd = svg[svgStart.lowerBound...].firstIndex(of: ">"),
          let box = attr(String(svg[svgStart.lowerBound..<svgEnd]), "viewBox") else { return nil }
    let v = box.split(separator: " ").compactMap { Double($0) }
    guard v.count == 4, v[2] > 0, v[3] > 0 else { return nil }
    let (gw, gh) = (v[2], v[3])

    var rects: [(NSRect, NSColor)] = []
    for piece in svg.components(separatedBy: "<rect").dropFirst() {
        guard let end = piece.firstIndex(of: ">") else { continue }
        let tag = " " + String(piece[..<end])
        guard let x = attr(tag, "x").flatMap({ Double($0) }), let y = attr(tag, "y").flatMap({ Double($0) }),
              let w = attr(tag, "width").flatMap({ Double($0) }), let h = attr(tag, "height").flatMap({ Double($0) }),
              let fill = attr(tag, "fill"), fill.hasPrefix("#"), fill.count == 7,
              let rgb = Int(fill.dropFirst(), radix: 16) else { continue }
        let colour = NSColor(srgbRed: CGFloat((rgb >> 16) & 0xff) / 255, green: CGFloat((rgb >> 8) & 0xff) / 255,
                             blue: CGFloat(rgb & 0xff) / 255, alpha: 1)
        rects.append((NSRect(x: x, y: y, width: w, height: h), colour))
    }
    guard !rects.isEmpty else { return nil }
    // Flipped, so the SVG's y-down coordinates draw as written.
    return NSImage(size: NSSize(width: size, height: size), flipped: true) { bounds in
        let (sx, sy) = (bounds.width / CGFloat(gw), bounds.height / CGFloat(gh))
        NSGraphicsContext.current?.shouldAntialias = false
        for (r, c) in rects {
            c.setFill()
            NSRect(x: r.minX * sx, y: r.minY * sy, width: r.width * sx, height: r.height * sy).fill()
        }
        return true
    }
}

func fetchText(_ path: String) async -> String? {
    var req = URLRequest(url: base.appendingPathComponent(path))
    req.timeoutInterval = 2
    guard let (data, resp) = try? await URLSession.shared.data(for: req),
          (resp as? HTTPURLResponse)?.statusCode == 200 else { return nil }
    return String(data: data, encoding: .utf8)
}

// MARK: - The menu

/// Menu titles do not wrap, so long text is clipped and kept whole in the tooltip.
func clip(_ s: String, _ n: Int = 64) -> String {
    s.count <= n ? s : String(s.prefix(n - 1)) + "…"
}

extension App {
    /// The RAM / ThreatWarden switch, as the first item of the menu.
    func paneSwitcher() -> NSMenuItem {
        let seg = NSSegmentedControl(labels: ["RAM", "ThreatWarden"], trackingMode: .selectOne,
                                     target: self, action: #selector(switchPane(_:)))
        seg.segmentDistribution = .fillEqually
        seg.selectedSegment = pane.rawValue
        if let icon = threatIcon16 {
            seg.setImage(icon, forSegment: Pane.threat.rawValue)
            seg.setImageScaling(.scaleProportionallyDown, forSegment: Pane.threat.rawValue)
        }
        seg.frame = NSRect(x: 12, y: 4, width: 256, height: 24)
        let holder = NSView(frame: NSRect(x: 0, y: 0, width: 280, height: 32))
        holder.addSubview(seg)
        let item = NSMenuItem()
        item.view = holder
        return item
    }

    @objc func switchPane(_ sender: NSSegmentedControl) {
        pane = Pane(rawValue: sender.selectedSegment) ?? .ram
        // Rebuilding removes the control that is calling us; do it after.
        DispatchQueue.main.async { self.rebuild() }
    }

    func line(_ title: String, colour: NSColor? = nil, small: Bool = false, tip: String? = nil) {
        let m = NSMenuItem(title: clip(title), action: nil, keyEquivalent: "")
        var attrs: [NSAttributedString.Key: Any] = [:]
        if let colour = colour { attrs[.foregroundColor] = colour }
        if small { attrs[.font] = NSFont.menuFont(ofSize: 11) }
        if !attrs.isEmpty { m.attributedTitle = NSAttributedString(string: clip(title), attributes: attrs) }
        m.toolTip = tip ?? (title.count > 64 ? title : nil)
        m.isEnabled = false
        menu.addItem(m)
    }

    /// A line starting with a coloured dot, the rest in the normal colour.
    func dotted(_ level: String, _ text: String, _ detail: String = "") {
        let s = NSMutableAttributedString(string: "● ", attributes: [.foregroundColor: levelColour(level)])
        s.append(NSAttributedString(string: clip(text), attributes: [.foregroundColor: NSColor.labelColor]))
        if !detail.isEmpty {
            s.append(NSAttributedString(string: "  " + clip(detail, 48),
                                        attributes: [.foregroundColor: NSColor.secondaryLabelColor]))
        }
        let m = NSMenuItem(title: text, action: nil, keyEquivalent: "")
        m.attributedTitle = s
        m.toolTip = detail.isEmpty ? nil : detail
        m.isEnabled = false
        menu.addItem(m)
    }

    func buildThreatPane() {
        guard let p = threat else { add("Loading ThreatWarden…", nil); return }
        if let note = threatSetupNote { line(note, small: true, tip: note) }

        let title = NSMutableAttributedString(string: p.name + "  ", attributes: [
            .font: NSFont.boldSystemFont(ofSize: 13), .foregroundColor: NSColor.labelColor])
        title.append(NSAttributedString(string: "● " + levelLabel(p.level), attributes: [
            .font: NSFont.systemFont(ofSize: 12, weight: .semibold), .foregroundColor: levelColour(p.level)]))
        let head = NSMenuItem(title: p.name, action: nil, keyEquivalent: "")
        head.attributedTitle = title
        head.image = threatIcon32
        head.isEnabled = false
        menu.addItem(head)
        if !p.headline.isEmpty { line(p.headline) }
        if !p.detail.isEmpty { line(p.detail, colour: .secondaryLabelColor, small: true) }
        if let s = p.summary { line(s, small: true) }

        if let up = p.upgrade {
            menu.addItem(.separator())
            let m = NSMenuItem(title: "Get RamWarden Pro…", action: #selector(openUpgrade(_:)),
                               keyEquivalent: "")
            m.target = self
            m.representedObject = up.url
            m.toolTip = up.url.absoluteString
            menu.addItem(m)
            line("\(up.price). Pay with Phantom, MetaMask or card. Your RAM warden stays free.",
                 colour: .secondaryLabelColor, small: true)
        } else if p.turnOn {
            menu.addItem(.separator())
            let m = NSMenuItem(title: "Turn on ThreatWarden", action: #selector(turnOnThreat), keyEquivalent: "")
            m.target = self
            m.toolTip = "Installs Wazuh's own agent and connects it. macOS asks for your password once."
            m.isEnabled = !threatSetupPending
            menu.addItem(m)
        }

        if !p.engines.isEmpty {
            menu.addItem(.separator())
            header(p.upgrade != nil ? "WHAT PRO TURNS ON" : "ENGINES")
            for e in p.engines { dotted(engineLevel(e.state), e.name, e.detail) }
        }

        if p.upgrade == nil {
            menu.addItem(.separator())
            header("RECENT ALERTS")
            if p.alerts.isEmpty { line("Nothing at or above your alert level", colour: .secondaryLabelColor) }
            for a in p.alerts {
                dotted(a.level, a.title, "\(a.tag) · \(a.engine) · \(a.when)")
                if !a.explanation.isEmpty { line("   " + a.explanation, colour: .secondaryLabelColor, small: true, tip: a.explanation) }
                if !a.advice.isEmpty { line("   → " + a.advice, small: true, tip: a.advice) }
            }
        }
        if !p.agents.isEmpty {
            menu.addItem(.separator())
            header("YOUR OTHER COMPUTERS")
            for a in p.agents { line(a, colour: .secondaryLabelColor) }
        }
        for problem in p.problems { line("⚠︎ " + problem, colour: levelColour("watch"), small: true, tip: problem) }
        line("Watches and explains only. Never deletes, quarantines or blocks on its own.",
             colour: .tertiaryLabelColor, small: true)
    }

    func refreshThreat() async {
        let p = threatPane(from: await fetchJSON("wardens/threat"))
        var big: NSImage?, small: NSImage?
        if threatIcon32 == nil, let path = p.iconPath, let svg = await fetchText(path) {
            big = spriteImage(svg: svg, size: 32)
            small = spriteImage(svg: svg, size: 16)
        }
        let icons = (big, small)
        await MainActor.run {
            self.threat = p
            if let big = icons.0 { self.threatIcon32 = big; self.threatIcon16 = icons.1 }
        }
    }

    @objc func openUpgrade(_ sender: NSMenuItem) {
        if let url = sender.representedObject as? URL { NSWorkspace.shared.open(url) }
    }

    @objc func turnOnThreat() {
        guard !threatSetupPending else { return }
        threatSetupPending = true
        threatSetupNote = "Starting setup…"
        rebuild()
        var req = URLRequest(url: base.appendingPathComponent("wardens/threat/setup"))
        req.timeoutInterval = 20
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = "{}".data(using: .utf8)
        URLSession.shared.dataTask(with: req) { data, response, error in
            let note = setupMessage(data: data, status: (response as? HTTPURLResponse)?.statusCode, error: error)
            Task {
                await self.refreshThreat()
                await MainActor.run {
                    self.threatSetupPending = false
                    self.threatSetupNote = note
                    self.rebuild()
                }
            }
        }.resume()
    }
}

func setupMessage(data: Data?, status: Int?, error: Error?) -> String {
    if let error = error { return "Setup did not start: \(error.localizedDescription)" }
    let json = data.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]
    if let status = status, (200..<300).contains(status), json?["started"] as? Bool == true {
        return "ThreatWarden is setting up. Approve the macOS prompt when it appears."
    }
    let reason = json?["error"] as? String ?? status.map { "daemon returned HTTP \($0)" } ?? "no response"
    return "Setup did not start: \(reason)"
}
