import SwiftUI
import AppKit
import CryptoKit

struct SetupView: View {
    static var needsSetup:Bool {
        let d=UserDefaults.standard
        guard let url=Bundle.main.url(forResource:"LICENSE",withExtension:"txt"),let data=try? Data(contentsOf:url) else{return true}
        let hash=SHA256.hash(data:data).map{String(format:"%02x",$0)}.joined()
        return d.string(forKey:"setup.installedVersion") != Bundle.main.object(forInfoDictionaryKey:"CFBundleVersion") as? String || d.string(forKey:"setup.licenseHash") != hash
    }
    var done: (() -> Void)?
    @State private var step = 0
    @State private var licenseAccepted=false
    @State private var reachedLicenseEnd=false
    @State private var allowance = 3
    @State private var health = "Not checked"
    @State private var checking = false
    @State private var copied = false
    @State private var installing = false
    @State private var installationComplete = false
    @State private var installResult = ""
    @State private var connecting = false
    @State private var connectStatus = ""
    @State private var connectURL: URL?
    @State private var loginProcess: Process?
    private let titles = ["Welcome", "License agreement", "Your account", "Install or update", "Check your warden", "Bring your other PCs"]
    private var site: String {
        let configured = ProcessInfo.processInfo.environment["RAMWARDEN_SETUP_SITE"] ?? (Bundle.main.object(forInfoDictionaryKey: "WardenSiteURL") as? String) ?? "https://ramwarden.com"
        return configured == "http://localhost:8000" ? configured : "https://ramwarden.com"
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            Text("A little setup. A lot more breathing room.").font(.largeTitle.bold())
            HStack { ForEach(titles.indices, id: \.self) { i in
                Button("\(i + 1)") { step = i }.buttonStyle(.borderedProminent).tint(step == i ? .red : .gray).disabled(i > 1 && !licenseAccepted)
                if i < titles.count - 1 { Rectangle().frame(height: 1).foregroundStyle(.secondary) }
            }}
            Text(titles[step]).font(.title2.bold())
            Group {
                switch step {
                case 0:
                    Text("Your RAM companion keeps a crowded computer comfortable. ThreatWarden adds security monitoring with Pro. We’ll connect your account, update safely, and check what is actually running.")
                    Text("Existing settings, credentials, and history should stay with your installation. Do not uninstall or delete your configuration to update.").foregroundStyle(.secondary)
                case 1:
                    Text("Read the license to the end, then choose whether to accept it.").fixedSize(horizontal:false,vertical:true)
                    GeometryReader { viewport in
                        ScrollView {
                            VStack(alignment:.leading,spacing:12){
                                Text(licenseText).font(.system(size:12,design:.monospaced)).textSelection(.enabled).fixedSize(horizontal:false,vertical:true)
                                Color.clear.frame(height:1).background(GeometryReader { marker in Color.clear.preference(key:LicenseEndPreference.self,value:marker.frame(in:.named("licenseViewport")).maxY) })
                            }.padding(12)
                        }.coordinateSpace(name:"licenseViewport")
                        .onPreferenceChange(LicenseEndPreference.self){bottom in if bottom >= 0 && bottom <= viewport.size.height+2 {reachedLicenseEnd=true}}
                    }.frame(height:210).background(Color.primary.opacity(0.04),in:RoundedRectangle(cornerRadius:8))
                    Toggle("I have read and agree to the license agreement",isOn:$licenseAccepted).disabled(!reachedLicenseEnd || licenseText.isEmpty)
                        .onChange(of:licenseAccepted){accepted in
                            if accepted {UserDefaults.standard.set(licenseHash,forKey:"setup.licenseHash");UserDefaults.standard.set(Date().timeIntervalSince1970,forKey:"setup.licenseAcceptedAt")}
                            else {UserDefaults.standard.removeObject(forKey:"setup.licenseHash")}
                        }
                    if !reachedLicenseEnd {Text("Scroll to the bottom to enable acceptance.").font(.caption).foregroundStyle(.secondary)}
                case 2:
                    Text("Sign in with your email, wallet, or GitHub when available. Use the same Warden account on all your computers. Approve each computer’s own name and code.")
                    Button("Open my account") { open("/account.html") }
                    Button(connecting ? "Waiting for account approval…" : "Connect this Mac") { Task { await connect() } }.disabled(connecting)
                    if let url = connectURL { Button("Approve this computer in browser") { NSWorkspace.shared.open(url) } }
                    if !connectStatus.isEmpty { Text(connectStatus).font(.callout).textSelection(.enabled) }
                    Text("Usage sharing is off until you enable it in account settings. It sends numeric activity totals, not tab URLs, chats, file contents, or process names.").font(.callout).foregroundStyle(.secondary)
                    Button("Review usage-sharing settings"){open("/account.html")}
                    Text("GitHub sign-in does not grant remote access. Tailscale remains optional and separately authorized.").font(.callout).foregroundStyle(.secondary)
                case 3:
                    Text("Everything needed for this Mac is bundled here. Install the daemon, menu bar companion, and browser extension without downloading another installer. Existing settings and account credentials stay in place.")
                    Button(installing ? "Installing…" : "Install on this Mac") { Task { await install() } }.disabled(installing)
                    if !installResult.isEmpty { Text(installResult).font(.callout).textSelection(.enabled) }
                    Button("Show browser extension folder") { NSWorkspace.shared.open(FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/RamWarden/extension")) }
                    Text("After installation, enable the browser extension in each profile you want your warden to manage. The browser page explains the steps.").foregroundStyle(.secondary)
                case 4:
                    Text("Check whether this Mac’s existing daemon is answering. This does not verify your subscription, browser extension, or every security engine.")
                    HStack { Button(checking ? "Checking…" : "Check this Mac") { Task { await check() } }.disabled(checking); Text(health).accessibilityLabel("Daemon status: \(health)") }
                    Text("Open the companion to verify its connection. In Customize your ram, choose its name, colors, eyes, and size when supported by your installed version.").foregroundStyle(.secondary)
                default:
                    Text("Give your own agent this request. It contains no keys, pass files, or personal install links. Your account’s remaining allowance is always checked by the server.")
                    Picker("Plan guidance", selection: $allowance) { Text("Lite · 1 PC").tag(1); Text("Plus · 3 PCs").tag(3); Text("Pro · 5 PCs").tag(5) }.pickerStyle(.segmented)
                    Text("Choosing a plan here only changes the request. It does not purchase or unlock anything.").font(.caption).foregroundStyle(.secondary)
                    Button(copied ? "Copied" : "Copy setup request + agent skill") { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(prompt + "\n\n" + bundledSkill, forType: .string); copied = true }
                }
            }.frame(maxWidth: .infinity, alignment: .leading).fixedSize(horizontal:false,vertical:true)
            Spacer()
            HStack { Button("Back") { step -= 1 }.disabled(step == 0); Spacer(); Text((site == "http://localhost:8000" ? "Local preview · " : "") + "Step \(step + 1) of \(titles.count)").font(.caption).foregroundStyle(.secondary); Button(step == 5 ? "Done" : "Continue") { if step == 5 { if let done=done {done()} else {NSApp.terminate(nil)} } else { step += 1 } }.buttonStyle(.borderedProminent).tint(.red).disabled(installing || (step == 5 && !installationComplete) || (step == 1 && !licenseAccepted)) }
        }.padding(32).onAppear{licenseAccepted=UserDefaults.standard.string(forKey:"setup.licenseHash") == licenseHash && !licenseText.isEmpty;reachedLicenseEnd=licenseAccepted}.onDisappear { loginProcess?.terminate() }
    }
    private var licenseText:String {
        guard let url=Bundle.main.url(forResource:"LICENSE",withExtension:"txt") else{return ""}
        return (try? String(contentsOf:url,encoding:.utf8)) ?? ""
    }
    private var licenseHash:String {SHA256.hash(data:Data(licenseText.utf8)).map{String(format:"%02x",$0)}.joined()}
    private func open(_ path: String) { if let url = URL(string: site + path) { NSWorkspace.shared.open(url) } }
    private var bundledSkill: String {
        guard let url = Bundle.main.resourceURL?.appendingPathComponent("agent-skill/SKILL.md") else { return "" }
        return (try? String(contentsOf: url, encoding: .utf8)) ?? ""
    }
    private var prompt: String {
        "Help me set up RamWarden on up to \(allowance) of my chosen computers within my account’s remaining device allowance. Follow https://github.com/ethanrxla/ramwarden/blob/main/agent/skills/ramwarden/SKILL.md. Check each OS and existing installation first; update in place and preserve settings, credentials, and history. Use only remote access I already authorized. Sign each computer in separately using its own approval code; never copy tokens, fingerprints, signed passes, or reuse an install link. Ask me before granting remote access or changing network/security settings. Verify daemon health, companion, and browser extension per computer. Do not buy anything. Report actual results."
    }
    @MainActor private func connect() async {
        guard let helper = Bundle.main.resourceURL?.appendingPathComponent("payload/ramwarden-usage-sync"), FileManager.default.isExecutableFile(atPath: helper.path) else { connectStatus = "Open the complete disk image to connect this Mac."; return }
        connecting = true; connectURL = nil; connectStatus = "Starting this computer’s sign-in…"
        defer { connecting = false; loginProcess = nil }
        let process = Process(); process.executableURL = helper; process.arguments = ["login"]
        var environment = ProcessInfo.processInfo.environment
        environment["RAMWARDEN_ACCOUNT_API"] = site == "http://localhost:8000" ? site : "https://api.ramwarden.com"
        process.environment = environment
        let pipe = Pipe(); process.standardOutput = pipe; process.standardError = pipe; loginProcess = process
        do { try process.run() } catch { connectStatus = "Sign-in could not start: " + error.localizedDescription; return }
        let result: String = await Task.detached {
            var output = ""
            while true {
                let data = pipe.fileHandleForReading.availableData
                if data.isEmpty { break }
                output += String(data: data, encoding: .utf8) ?? ""
                for line in output.split(separator: "\n") {
                    if let url = URL(string: String(line)), url.path == "/link.html", (url.scheme == "https" && url.host == "ramwarden.com") || (url.scheme == "http" && url.host == "localhost" && url.port == 8000) {
                        await MainActor.run { connectURL = url; connectStatus = "Approve this Mac’s name and code in your browser. This does not buy a plan." }
                    }
                }
                if output.utf8.count > 8192 { output = String(output.suffix(4096)) }
            }
            process.waitUntilExit()
            return process.terminationStatus == 0 ? "This Mac is connected to your account." : "Sign-in did not finish. Try again when the account service is reachable."
        }.value
        connectStatus = result; connectURL = nil
    }
    @MainActor private func install() async {
        guard licenseAccepted && reachedLicenseEnd && !licenseText.isEmpty else {installResult="Read and accept the bundled license before installation.";return}
        installing = true; defer { installing = false }
        guard let resources = Bundle.main.resourceURL,
              FileManager.default.fileExists(atPath: resources.appendingPathComponent("install-payload.sh").path) else { installResult = "This build has no installation payload. Use the complete disk image."; return }
        let home = FileManager.default.homeDirectoryForCurrentUser
        let unified=Bundle.main.bundleIdentifier == "com.ramwarden.menubar"
        if unified && !(Bundle.main.bundlePath.hasPrefix("/Applications/") || Bundle.main.bundlePath.hasPrefix(home.appendingPathComponent("Applications").path+"/")) {installResult="Drag RamWarden into Applications first, then open it there to finish setup.";return}
        for item in ["ramwarden-daemon", "ramwarden-usage-sync", "ramwarden-launcher", "config.toml", "extension"] {
            guard FileManager.default.fileExists(atPath: resources.appendingPathComponent("payload/" + item).path) else { installResult = "This disk image is incomplete; nothing was installed."; return }
        }
        do {
            let agents = home.appendingPathComponent("Library/LaunchAgents")
            try FileManager.default.createDirectory(at: agents, withIntermediateDirectories: true)
            let definitionsDirectory=FileManager.default.temporaryDirectory.appendingPathComponent("warden-definitions-"+UUID().uuidString)
            try FileManager.default.createDirectory(at:definitionsDirectory,withIntermediateDirectories:true,attributes:[.posixPermissions:0o700])
            defer {try? FileManager.default.removeItem(at:definitionsDirectory)}
            let definitions: [(String, [String])] = [
                ("com.ramwarden.daemon", [home.appendingPathComponent(".local/bin/ramwarden-daemon").path]),
                ("com.ramwarden.menubar", [unified ? Bundle.main.executablePath! : home.appendingPathComponent("Applications/RamWarden.app/Contents/MacOS/RamWarden").path]),
                ("com.ramwarden.usage", [home.appendingPathComponent(".local/bin/ramwarden-usage-sync").path, "--watch"])
            ]
            for (label, args) in definitions {
                let file = agents.appendingPathComponent(label + ".plist")
                // Stage definitions before payload install; installation only starts on this button.
                var plist: [String: Any] = ["EnvironmentVariables": ["RAMWARDEN_CONFIG":home.appendingPathComponent(".config/ramwarden/config.toml").path,"PATH":"/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"], "Label": label, "ProgramArguments": args, "RunAtLoad": true, "KeepAlive": ["SuccessfulExit": false], "ThrottleInterval": 60, "ProcessType": label == "com.ramwarden.menubar" ? "Interactive" : "Background", "StandardOutPath": home.appendingPathComponent("Library/Logs/" + label + ".log").path, "StandardErrorPath": home.appendingPathComponent("Library/Logs/" + label + ".err.log").path]
                if let old=try? Data(contentsOf:file), let previous=try? PropertyListSerialization.propertyList(from:old,format:nil) as? [String:Any], let environment=previous["EnvironmentVariables"] as? [String:String] {var merged=environment; for (key,value) in plist["EnvironmentVariables"] as? [String:String] ?? [:] {merged[key]=value};plist["EnvironmentVariables"]=merged}
                if label == "com.ramwarden.daemon" {plist.removeValue(forKey:"ProcessType")}
                let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
                let staged=definitionsDirectory.appendingPathComponent(label+".plist")
                try data.write(to:staged,options:.atomic)
                try FileManager.default.setAttributes([.posixPermissions:0o600],ofItemAtPath:staged.path)
            }
            let script = resources.appendingPathComponent("install-payload.sh").path
            let result: String = await Task.detached {
                do {
                    let process = Process(); process.executableURL = URL(fileURLWithPath: "/bin/bash"); process.arguments = (unified ? [script,"--app",Bundle.main.bundlePath] : [script])+["--definitions",definitionsDirectory.path]
                    let pipe = Pipe(); process.standardOutput = pipe; process.standardError = pipe
                    try process.run()
                    let data = pipe.fileHandleForReading.readDataToEndOfFile(); process.waitUntilExit()
                    let text = String(data: data, encoding: .utf8) ?? ""
                    return process.terminationStatus == 0 ? text : "Installation stopped. " + text
                } catch { return "Installation could not start: " + error.localizedDescription }
            }.value
            installResult = result
            if result.contains("Installed. Your settings") {installationComplete=true;UserDefaults.standard.set(Bundle.main.object(forInfoDictionaryKey:"CFBundleVersion") as? String,forKey:"setup.installedVersion")}
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

private struct LicenseEndPreference:PreferenceKey {
    static var defaultValue:CGFloat {CGFloat.greatestFiniteMagnitude}
    static func reduce(value:inout CGFloat,nextValue:()->CGFloat){value=nextValue()}
}
