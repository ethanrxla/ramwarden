import SwiftUI
import AppKit

@main
struct WardenSetupApp: App {
    var body: some Scene {
        WindowGroup("Set up your wardens") { SetupView().frame(minWidth: 640, minHeight: 520) }
            .windowStyle(.hiddenTitleBar)
    }
}
struct SetupView: View {
    @State private var step = 0
    @State private var allowance = 3
    @State private var health = "Not checked"
    @State private var checking = false
    @State private var copied = false
    @State private var installing = false
    @State private var installResult = ""
    private let titles = ["Welcome", "Your account", "Install or update", "Check your warden", "Bring your other PCs"]
    private var site: String { ProcessInfo.processInfo.environment["RAMWARDEN_SETUP_SITE"] == "http://localhost:8000" ? "http://localhost:8000" : "https://ramwarden.com" }
    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            Text("A little setup. A lot more breathing room.").font(.largeTitle.bold())
            HStack { ForEach(titles.indices, id: \.self) { i in
                Button("\(i + 1)") { step = i }.buttonStyle(.borderedProminent).tint(step == i ? .red : .gray)
                if i < titles.count - 1 { Rectangle().frame(height: 1).foregroundStyle(.secondary) }
            }}
            Text(titles[step]).font(.title2.bold())
            Group {
                switch step {
                case 0:
                    Text("Your RAM companion keeps a crowded computer comfortable. ThreatWarden adds security monitoring with Pro. We’ll connect your account, update safely, and check what is actually running.")
                    Text("Existing settings, credentials, and history should stay with your installation. Do not uninstall or delete your configuration to update.").foregroundStyle(.secondary)
                case 1:
                    Text("Sign in with your email, wallet, or GitHub when available. Use the same Warden account on all your computers. Approve each computer’s own name and code.")
                    Button("Open my account") { open("/account.html") }
                    Text("GitHub sign-in does not grant remote access. Tailscale remains optional and separately authorized.").font(.callout).foregroundStyle(.secondary)
                case 2:
                    Text("Everything needed for this Mac is bundled here. Install the daemon, menu bar companion, and browser extension without downloading another installer. Existing settings and account credentials stay in place.")
                    Button(installing ? "Installing…" : "Install on this Mac") { Task { await install() } }.disabled(installing)
                    if !installResult.isEmpty { Text(installResult).font(.callout).textSelection(.enabled) }
                    Button("Show browser extension folder") { NSWorkspace.shared.open(FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/RamWarden/extension")) }
                    Text("After installation, enable the browser extension in each profile you want your warden to manage. The browser page explains the steps.").foregroundStyle(.secondary)
                case 3:
                    Text("Check whether this Mac’s existing daemon is answering. This does not verify your subscription, browser extension, or every security engine.")
                    HStack { Button(checking ? "Checking…" : "Check this Mac") { Task { await check() } }.disabled(checking); Text(health).accessibilityLabel("Daemon status: \(health)") }
                    Text("Open the companion to verify its connection. In Customize your ram, choose its name, colors, eyes, and size when supported by your installed version.").foregroundStyle(.secondary)
                default:
                    Text("Give your own agent this request. It contains no keys, pass files, or personal install links. Your account’s remaining allowance is always checked by the server.")
                    Picker("Plan guidance", selection: $allowance) { Text("Lite · 1 PC").tag(1); Text("Plus · 3 PCs").tag(3); Text("Pro · 5 PCs").tag(5) }.pickerStyle(.segmented)
                    Text("Choosing a plan here only changes the request. It does not purchase or unlock anything.").font(.caption).foregroundStyle(.secondary)
                    Button(copied ? "Copied" : "Copy agent setup request") { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(prompt, forType: .string); copied = true }
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
            Spacer()
            HStack { Button("Back") { step -= 1 }.disabled(step == 0); Spacer(); Text("Step \(step + 1) of \(titles.count)").font(.caption).foregroundStyle(.secondary); Button(step == 4 ? "Done" : "Continue") { if step == 4 { NSApp.terminate(nil) } else { step += 1 } }.buttonStyle(.borderedProminent).tint(.red) }
        }.padding(32)
    }
    private func open(_ path: String) { if let url = URL(string: site + path) { NSWorkspace.shared.open(url) } }
    private var prompt: String {
        "Help me set up RamWarden on up to \(allowance) of my chosen computers within my account’s remaining device allowance. Follow https://github.com/ethanrxla/ramwarden/blob/main/agent/skills/ramwarden/SKILL.md. Check each OS and existing installation first; update in place and preserve settings, credentials, and history. Use only remote access I already authorized. Sign each computer in separately using its own approval code; never copy tokens, fingerprints, signed passes, or reuse an install link. Ask me before granting remote access or changing network/security settings. Verify daemon health, companion, and browser extension per computer. Do not buy anything. Report actual results."
    }
    @MainActor private func install() async {
        installing = true; defer { installing = false }
        guard let resources = Bundle.main.resourceURL,
              FileManager.default.fileExists(atPath: resources.appendingPathComponent("install-payload.sh").path) else { installResult = "This build has no installation payload. Use the complete disk image."; return }
        let home = FileManager.default.homeDirectoryForCurrentUser
        for item in ["ramwarden-daemon", "ramwarden-usage-sync", "config.toml", "RamWarden.app", "extension"] {
            guard FileManager.default.fileExists(atPath: resources.appendingPathComponent("payload/" + item).path) else { installResult = "This disk image is incomplete; nothing was installed."; return }
        }
        do {
            let agents = home.appendingPathComponent("Library/LaunchAgents")
            try FileManager.default.createDirectory(at: agents, withIntermediateDirectories: true)
            let definitions: [(String, [String])] = [
                ("com.ramwarden.daemon", [home.appendingPathComponent(".local/bin/ramwarden-daemon").path]),
                ("com.ramwarden.menubar", [home.appendingPathComponent("Applications/RamWarden.app/Contents/MacOS/RamWarden").path]),
                ("com.ramwarden.usage", [home.appendingPathComponent(".local/bin/ramwarden-usage-sync").path, "--watch"])
            ]
            for (label, args) in definitions {
                let file = agents.appendingPathComponent(label + ".plist")
                // Stage definitions before payload install; installation only starts on this button.
                let plist: [String: Any] = ["EnvironmentVariables": ["RAMWARDEN_CONFIG":home.appendingPathComponent(".config/ramwarden/config.toml").path,"PATH":"/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"], "Label": label, "ProgramArguments": args, "RunAtLoad": true, "KeepAlive": true, "ThrottleInterval": 60, "ProcessType": "Background", "StandardOutPath": home.appendingPathComponent("Library/Logs/" + label + ".log").path, "StandardErrorPath": home.appendingPathComponent("Library/Logs/" + label + ".err.log").path]
                let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
                try data.write(to: file, options: .atomic)
            }
            let script = resources.appendingPathComponent("install-payload.sh").path
            let result: String = await Task.detached {
                do {
                    let process = Process(); process.executableURL = URL(fileURLWithPath: "/bin/bash"); process.arguments = [script]
                    let pipe = Pipe(); process.standardOutput = pipe; process.standardError = pipe
                    try process.run()
                    let data = pipe.fileHandleForReading.readDataToEndOfFile(); process.waitUntilExit()
                    let text = String(data: data, encoding: .utf8) ?? ""
                    return process.terminationStatus == 0 ? text : "Installation stopped. " + text
                } catch { return "Installation could not start: " + error.localizedDescription }
            }.value
            installResult = result
        } catch { installResult = "Installation stopped: " + error.localizedDescription }
    }
    @MainActor private func check() async {
        checking = true; defer { checking = false }
        guard let url = URL(string: "http://127.0.0.1:7823/health") else { return }
        do {
            var request = URLRequest(url: url); request.timeoutInterval = 3
            let (data, response) = try await URLSession.shared.data(for: request)
            let value = try JSONSerialization.jsonObject(with: data) as? [String: Any]
            health = (response as? HTTPURLResponse)?.statusCode == 200 && value?["ok"] as? Bool == true ? "Daemon is answering" : "Daemon returned an unexpected status"
        } catch { health = "Not connected yet" }
    }
}
