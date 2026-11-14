import AppKit
import CryptoKit
import SwiftUI

@MainActor final class RequiredModules:ObservableObject {
    @Published var busy=false
    @Published var status="Core components are bundled. Security modules are checked separately for Pro."
    private let fm=FileManager.default
    private var prefix:String {fm.isExecutableFile(atPath:"/opt/homebrew/bin/brew") ? "/opt/homebrew" : "/usr/local"}
    private func securityReport() async -> [String:Any]? {
        guard let url=URL(string:"http://127.0.0.1:7823/wardens/threat"),let(data,response)=try? await URLSession.shared.data(from:url),(response as? HTTPURLResponse)?.statusCode==200 else{return nil}
        return (try? JSONSerialization.jsonObject(with:data)) as? [String:Any]
    }
    func check() async {
        guard let report=await securityReport() else {status="Core installation must finish before security modules can be checked.";return}
        guard report["unlocked"] as? Bool == true else {status="Core components are ready. Connect a verified Pro account to install security modules.";return}
        let checks=[("Sentinel",fm.isExecutableFile(atPath:"/Library/Ossec/bin/wazuh-control")),("Antivirus",fm.isExecutableFile(atPath:prefix+"/bin/clamscan")),("Threat detection",fm.isExecutableFile(atPath:prefix+"/bin/suricata"))]
        status=checks.map{"\($0.0): \($0.1 ? "installed" : "required")"}.joined(separator:"\n")+"\nInstalled does not mean connected or actively monitoring; run the final health check."
    }
    func install() async {
        guard !busy else{return};busy=true;defer{busy=false}
        SetupEvents.record(.modulesStarted,step:3)
        do {
            guard let report=await securityReport(),report["unlocked"] as? Bool == true else {throw ModuleError.message("Connect this computer to a verified Pro account first.")}
            if !fm.isExecutableFile(atPath:"/Library/Ossec/bin/wazuh-control") {
                status="Installing Sentinel. Approve the macOS administrator prompt."
                var request=URLRequest(url:URL(string:"http://127.0.0.1:7823/wardens/threat/setup")!);request.httpMethod="POST"
                let(_,response)=try await URLSession.shared.data(for:request)
                guard (response as? HTTPURLResponse)?.statusCode==202 else{throw ModuleError.message("Sentinel enrollment could not start. Check the account service.")}
                var ready=false
                for _ in 0..<180 {
                    try await Task.sleep(nanoseconds:1_000_000_000)
                    let state=await securityReport();let level=(state?["status"] as? [String:Any])?["level"] as? String
                    if fm.isExecutableFile(atPath:"/Library/Ossec/bin/wazuh-control") && level != "setup" {ready=true;break}
                }
                guard ready else{throw ModuleError.message("Sentinel installation has not been confirmed. Check its administrator prompt and retry.")}
            }
            let brew=prefix+"/bin/brew"
            if !fm.fileExists(atPath:"/Library/Application Support/ThreatWarden/suricata.yaml") {guard #available(macOS 15.0,*) else{throw ModuleError.message("New security-module installation requires macOS 15 or later. Core Warden remains available.")}}
            if !fm.isExecutableFile(atPath:brew) {
                status="Downloading the signed package manager required by the security modules."
                try await downloadPackageManager()
                status="The system package installer is open. Complete its administrator approval, then choose Install required modules again."
                return
            }
            if URL(fileURLWithPath:prefix+"/bin/clamscan").resolvingSymlinksInPath().path.contains("/1.5.4_1/") {
                status="Updating an incompatible Antivirus package…"
                guard await run(brew,["upgrade","clamav"])==0 else{throw ModuleError.message("The Antivirus package needs a verified update before setup can continue.")}
            }
            var missing=[String]()
            if !fm.isExecutableFile(atPath:prefix+"/bin/clamscan"){missing.append("clamav")}
            if !fm.isExecutableFile(atPath:prefix+"/bin/suricata"){missing.append("suricata")}
            if !missing.isEmpty {
                status="Installing required Antivirus and Threat detection packages…"
                guard await run(brew,["install"]+missing)==0 else{throw ModuleError.message("A required security package could not be installed. Setup diagnostics recorded the failure.")}
            }
            if !fm.fileExists(atPath:"/Library/Application Support/ThreatWarden/suricata.yaml") {
                guard let scripts=Bundle.main.resourceURL?.appendingPathComponent("security-modules") else{throw ModuleError.message("This installer is missing its security setup components.")}
                let python=prefix+"/opt/suricata/libexec/bin/python"
                guard fm.isExecutableFile(atPath:python) else{throw ModuleError.message("A required security runtime is missing. Repair the security packages, then retry.")}
                let user=NSUserName();guard user.range(of:"^[A-Za-z0-9._-]+$",options:.regularExpression) != nil else{throw ModuleError.message("This account requires administrator-assisted module setup.")}
                let command=quote(python)+" "+quote(scripts.appendingPathComponent("install-macos.py").path)+" --user "+quote(user)+" && "+quote(python)+" "+quote(scripts.appendingPathComponent("configure-macos-agent.py").path)
                let apple=command.replacingOccurrences(of:"\\",with:"\\\\").replacingOccurrences(of:"\"",with:"\\\"")
                status="Configuring security modules. Approve the macOS administrator prompt."
                guard await run("/usr/bin/osascript",["-e","do shell script \""+apple+"\" with administrator privileges"])==0 else{throw ModuleError.message("Security setup was cancelled or did not finish. Existing configuration backups were retained.")}
            }
            await check();SetupEvents.record(.modulesFinished,step:3,result:true)
        } catch {status=(error as? ModuleError)?.description ?? "Required module setup could not finish. Check diagnostics and retry.";SetupEvents.record(.modulesFinished,step:3,result:false)}
    }
    private func quote(_ value:String)->String {"'"+value.replacingOccurrences(of:"'",with:"'\\''")+"'"}
    private func run(_ program:String,_ arguments:[String]) async -> Int32 {
        await Task.detached(priority:.utility) {
            do {let p=Process();p.executableURL=URL(fileURLWithPath:program);p.arguments=arguments;let inherited=ProcessInfo.processInfo.environment;p.environment=Dictionary(uniqueKeysWithValues:["HOME","USER","LOGNAME","PATH","TMPDIR","LANG","SHELL"].compactMap{key in inherited[key].map{(key,$0)}});p.standardOutput=FileHandle.nullDevice;p.standardError=FileHandle.nullDevice;try p.run();p.waitUntilExit();return p.terminationStatus}catch{return -1}
        }.value
    }
    private func downloadPackageManager() async throws {
        #if !arch(arm64)
        throw ModuleError.message("Automatic package-manager installation is available for Apple silicon. This Mac needs the supported platform package manager before security setup.")
        #else
        let(data,response)=try await URLSession.shared.data(from:URL(string:"https://api.github.com/repos/Homebrew/brew/releases/latest")!)
        guard (response as? HTTPURLResponse)?.statusCode==200,data.count<2_000_000,let release=try JSONSerialization.jsonObject(with:data) as? [String:Any],let assets=release["assets"] as? [[String:Any]],let asset=assets.first(where:{$0["name"] as? String == "Homebrew.pkg"}),let address=asset["browser_download_url"] as? String,let url=URL(string:address),url.scheme=="https",url.host=="github.com",url.path.hasPrefix("/Homebrew/brew/releases/download/"),let digest=asset["digest"] as? String,digest.hasPrefix("sha256:"),let size=asset["size"] as? Int,size>0,size<128_000_000 else{throw ModuleError.message("The verified package-manager download is unavailable.")}
        let(temp,download)=try await URLSession.shared.download(from:url)
        guard (download as? HTTPURLResponse)?.statusCode==200 else{throw ModuleError.message("The package-manager download failed.")}
        let handle=try FileHandle(forReadingFrom:temp);defer{try? handle.close()};var hash=SHA256();var count=0
        while let chunk=try handle.read(upToCount:65536),!chunk.isEmpty {count+=chunk.count;guard count<=size else{throw ModuleError.message("Package download exceeded its declared size.")};hash.update(data:chunk)}
        guard count==size,"sha256:"+hash.finalize().map({String(format:"%02x",$0)}).joined()==digest else{throw ModuleError.message("The package checksum did not match. It was not opened.")}
        let destination=fm.temporaryDirectory.appendingPathComponent("warden-package-"+UUID().uuidString).appendingPathExtension("pkg");try fm.moveItem(at:temp,to:destination)
        guard await run("/usr/sbin/pkgutil",["--check-signature",destination.path])==0 else{throw ModuleError.message("The package signature could not be verified. It was not opened.")}
        NSWorkspace.shared.open(destination)
        #endif
    }
}
private enum ModuleError:Error,CustomStringConvertible {case message(String);var description:String{switch self {case .message(let message):return message}}}
