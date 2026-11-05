import AppKit
import SwiftUI

@MainActor final class UpdateController:ObservableObject {
    @Published var text="Check for a newer version of RamWarden."
    @Published var busy=false
    @Published var available=false
    private var window:NSWindow?
    func show(){
        if let window=window {window.makeKeyAndOrderFront(nil);NSApp.activate(ignoringOtherApps:true);return}
        let window=NSWindow(contentRect:NSRect(x:0,y:0,width:420,height:240),styleMask:[.titled,.closable],backing:.buffered,defer:false)
        window.title="RamWarden updates";window.isReleasedWhenClosed=false
        window.contentView=NSHostingView(rootView:UpdateView(model:self));window.center();self.window=window;window.makeKeyAndOrderFront(nil);NSApp.activate(ignoringOtherApps:true)
    }
    func check(){run("check")}
    func install(){run("update")}
    private func run(_ command:String){
        guard !busy else{return};busy=true
        let bundled=Bundle.main.resourceURL?.appendingPathComponent("payload/ramwarden-launcher").path
        let helper=bundled ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".local/bin/ramwarden-launcher").path
        Task {
            let result:(Int32,String)=await Task.detached {
                do {
                    let process=Process();process.executableURL=URL(fileURLWithPath:helper);process.arguments=[command]
                    let pipe=Pipe();process.standardOutput=pipe;process.standardError=pipe
                    try process.run();let bytes=pipe.fileHandleForReading.readDataToEndOfFile();process.waitUntilExit()
                    return (process.terminationStatus,String(data:bytes,encoding:.utf8) ?? "")
                } catch {return (1,"The update helper is missing. Install the complete RamWarden release.")}
            }.value
            busy=false
            if command == "update" {text=result.0 == 0 ? "Verified installer opened. Quit RamWarden, replace the app in Applications, then open it to finish updating." : result.1;return}
            guard result.0 == 0,let data=result.1.data(using:.utf8),let status=try? JSONSerialization.jsonObject(with:data) as? [String:Any] else {text=result.1;return}
            available=status["available_version"] as? String != nil
            if let version=status["available_version"] as? String {text="RamWarden \(version) is available.\n"+(status["notes"] as? String ?? "");show()}
            else if let error=status["error"] as? String {text=error}
            else {text="You have the latest published version."}
        }
    }
}
struct UpdateView:View {
    @ObservedObject var model:UpdateController
    var body:some View {VStack(alignment:.leading,spacing:16){Text("RamWarden updates").font(.title2.bold());ScrollView{Text(model.text).frame(maxWidth:.infinity,alignment:.leading)};HStack{Button("Check for updates"){model.check()}.disabled(model.busy);if model.available {Button("Download and open installer"){model.install()}.disabled(model.busy)}}}.padding(24).frame(width:420,height:240)}
}
