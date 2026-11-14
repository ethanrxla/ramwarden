import Foundation

// Fixed-schema diagnostics: callers cannot pass URLs, account details or command output.
enum SetupEvents {
    enum Event:String {case modulesStarted,modulesFinished,opened,stepEntered,licenseAccepted,connectionStarted,connectionFinished,installationStarted,installationFinished,healthChecked,sharingChoiceSaved,extensionOpened,pairingOpened,pairingOperationStarted,pairingOperationFinished,completed,closed}
    private static let queue=DispatchQueue(label:"com.ramwarden.setup-events",qos:.utility)
    private static let flow=UUID().uuidString
    static var path:URL {FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/RamWarden/setup-events.jsonl")}
    static func record(_ event:Event,step:Int,result:Bool?=nil,duration:TimeInterval?=nil){
        let version=Bundle.main.object(forInfoDictionaryKey:"CFBundleVersion") as? String ?? "unknown"
        queue.async {
            do {
                let manager=FileManager.default;let url=path
                try manager.createDirectory(at:url.deletingLastPathComponent(),withIntermediateDirectories:true,attributes:[.posixPermissions:0o700])
                if let size=(try? manager.attributesOfItem(atPath:url.path)[.size]) as? NSNumber,size.intValue>1_000_000 {
                    let old=url.appendingPathExtension("old");try? manager.removeItem(at:old);try manager.moveItem(at:url,to:old)
                }
                var fields:[String:Any]=["schema":1,"at":ISO8601DateFormatter().string(from:Date()),"flow":flow,"event":event.rawValue,"step":step,"version":version]
                if let result=result {fields["success"]=result};if let duration=duration {fields["duration_ms"]=Int(max(0,duration)*1000)}
                let data=try JSONSerialization.data(withJSONObject:fields,options:[.sortedKeys])+Data([10])
                if !manager.fileExists(atPath:url.path) {manager.createFile(atPath:url.path,contents:nil,attributes:[.posixPermissions:0o600])}
                let file=try FileHandle(forWritingTo:url);defer{try? file.close()};try file.seekToEnd();try file.write(contentsOf:data)
            } catch { /* Diagnostics must not block setup. */ }
        }
    }
}
