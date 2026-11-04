import AppKit
import SwiftUI
import Combine

class WardenProfile {
    func thinkingLine(at date:Date)->String { let index=Int(date.timeIntervalSince1970 / 3);return thinkingLines[index % thinkingLines.count] }
    let id:String; let name:String; let placeholder:String; let thinkingLines:[String]
    init(id:String,name:String,placeholder:String,thinkingLines:[String] = ["Discombobulating…","Babooning…","Larping…","Mogging…","67ing…","Deez nutzing…"]) {self.id=id;self.name=name;self.placeholder=placeholder;self.thinkingLines=thinkingLines}
}
final class RamWardenProfile: WardenProfile {
    init(){super.init(id:"ram",name:"Warden",placeholder:"Ask your ram…")}
}
final class ThreatWardenProfile: WardenProfile {
    init(){super.init(id:"threat",name:"ThreatWarden",placeholder:"Ask about your protection…",thinkingLines:["Reviewing telemetry…","Assessing the signals…","Waiting for analysis…"])}
}
// Shared identity, conversation transport, glass panel and speech-bubble lifecycle.
// Memory and security companions specialize their observations, not each other's actions.
@MainActor class WardenCompanion: NSObject {
    let model:CompanionModel
    var assistantPanel:ChatPanel?
    var statusBubble:BubblePanel?
    private var chatObserver:AnyCancellable?
    private var dismissBubble:DispatchWorkItem?
    override init(){model=CompanionModel(profile:RamWardenProfile());super.init();observeChat()}
    init(profile:WardenProfile){model=CompanionModel(profile:profile);super.init();observeChat()}
    private func observeChat(){
        chatObserver=model.$turns.sink{[weak self] _ in DispatchQueue.main.async{self?.resizeAssistant()}}
    }
    func showAssistant(anchor:NSWindow?,settings:@escaping ()->Void) {
        statusBubble?.orderOut(nil)
        if assistantPanel == nil {
            let p=ChatPanel(contentRect:NSRect(x:0,y:0,width:304,height:240),styleMask:[.borderless],backing:.buffered,defer:false)
            p.isOpaque=false;p.backgroundColor = .clear;p.hasShadow=true;p.level = .floating;p.hidesOnDeactivate=false
            p.isFloatingPanel=true;p.becomesKeyOnlyIfNeeded=false;p.collectionBehavior=[.canJoinAllSpaces,.fullScreenAuxiliary]
            p.contentView=NSHostingView(rootView:AssistantView(model:model,dismiss:{[weak self] in self?.closeAssistant()},settings:settings))
            assistantPanel=p
        }
        resizeAssistant()
        if let p=assistantPanel,let anchor=anchor {
            p.setFrame(PetSizing.attachedFrame(pet:anchor.frame,size:p.frame.size,bounds:anchor.screen?.visibleFrame ?? NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1440,height:900),preferAbove:false),display:true)
        } else {assistantPanel?.center()}
        NSApp.activate(ignoringOtherApps:true);assistantPanel?.makeKeyAndOrderFront(nil)
        DispatchQueue.main.async { NotificationCenter.default.post(name:Notification.Name("WardenChatOpened"),object:self.model.profile.id) }
    }
    private func resizeAssistant(){guard let p=assistantPanel else{return};let size=NSSize(width:304,height:model.chatHeight);if p.frame.size != size {p.setContentSize(size)}}
    func showStatusBubble(_ text:String,anchor:NSWindow?,open:@escaping ()->Void) {
        guard let anchor=anchor,assistantPanel?.isVisible != true else{return}
        dismissBubble?.cancel()
        let p=statusBubble ?? BubblePanel(contentRect:.zero,styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        p.isOpaque=false;p.backgroundColor = .clear;p.hasShadow=true;p.level = .floating;p.hidesOnDeactivate=false;p.collectionBehavior=[.canJoinAllSpaces,.fullScreenAuxiliary]
        p.contentView=NSHostingView(rootView:Button(action:open){Text(String(text.prefix(240))).font(.system(size:12)).multilineTextAlignment(.leading).fixedSize(horizontal:false,vertical:true).padding(12).frame(width:260).modifier(ChatGlass())}.buttonStyle(.plain))
        p.contentView?.layoutSubtreeIfNeeded();let size=p.contentView?.fittingSize ?? NSSize(width:260,height:80)
        let bounds=anchor.screen?.visibleFrame ?? NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1440,height:900)
        p.setFrame(PetSizing.attachedFrame(pet:anchor.frame,size:size,bounds:bounds,preferAbove:true),display:true)
        statusBubble=p;p.orderFrontRegardless()
        let work=DispatchWorkItem{[weak self] in self?.statusBubble?.orderOut(nil)};dismissBubble=work;DispatchQueue.main.asyncAfter(deadline:.now()+12,execute:work)
    }
    func closeAssistant(){model.cancelConversation();CompanionSpeech.shared.stop(model.profile.id);assistantPanel?.orderOut(nil)}
    func didMove(anchor:NSWindow?) {
        statusBubble?.orderOut(nil)
        guard let anchor=anchor,let chat=assistantPanel,chat.isVisible,let bounds=anchor.screen?.visibleFrame else{return}
        chat.setFrame(PetSizing.attachedFrame(pet:anchor.frame,size:chat.frame.size,bounds:bounds,preferAbove:false),display:true)
    }
    func hideSharedPanels(){GoatAlerts.shared.stop();model.companionVisible=false;model.cancelConversation();dismissBubble?.cancel();assistantPanel?.orderOut(nil);statusBubble?.orderOut(nil);CompanionSpeech.shared.stop(model.profile.id)}
}

// Short event notifications; no polling sound loop or network audio request.
final class GoatAlerts {
    static let shared = GoatAlerts()
    private var policy = BaaPolicy()
    private var sound: NSSound?
    private var criticalSamples = 0
    func notify(warden:String,event:String,critical:Bool) {
        guard policy.accept(warden:warden,event:event,muted:UserDefaults.standard.bool(forKey:"\(warden).baaMuted"),now:Date().timeIntervalSince1970),
              let url=Bundle.main.url(forResource:critical ? "baa-critical" : "baa",withExtension:"wav") else{return}
        sound=NSSound(contentsOf:url,byReference:false);sound?.volume=0.55;sound?.play()
    }
    func memory(percent:Double,visible:Bool) {
        guard visible else {criticalSamples=0;return}
        if percent < 90 {criticalSamples=0;policy.clear("ram");return}
        criticalSamples += 1
        if criticalSamples == 10 {notify(warden:"ram",event:"critical-memory",critical:percent >= 95)}
    }
    func stop(){sound?.stop()}
    func mute(_ warden:String) { if UserDefaults.standard.bool(forKey:"\(warden).baaMuted") {sound?.stop()} }
}
struct GoatAlertControl: View {
    let warden:String
    @AppStorage private var muted:Bool
    init(warden:String){self.warden=warden;self._muted=AppStorage(wrappedValue:false,"\(warden).baaMuted")}
    var body:some View {
        Toggle("Mute baa notifications",isOn:$muted).onChange(of:muted){_ in GoatAlerts.shared.mute(warden)}
    }
}

struct BaaPolicy {
    private var lastSound = -Double.infinity
    private var events:[String:String] = [:]
    mutating func clear(_ warden:String){events.removeValue(forKey:warden)}
    mutating func accept(warden:String,event:String,muted:Bool,now:Double)->Bool {
        guard events[warden] != event else{return false}
        events[warden]=event
        guard !muted,now-lastSound >= 900 else{return false}
        lastSound=now;return true
    }
}

// Bounded local recall. The shared cross-app hub is a separate explicit integration.
enum CompanionRecall {
    static let storage=DispatchQueue(label:"com.ramwarden.companion-memory",qos:.utility)
    static func saveLater(_ turns:[ChatTurn],warden:String){storage.async{save(turns,warden:warden)}}
    struct Entry:Codable {let role:String;let content:String;let at:Double}
    static func url(_ warden:String)->URL {
        FileManager.default.urls(for:.applicationSupportDirectory,in:.userDomainMask)[0].appendingPathComponent("RamWarden/CompanionMemory/\(warden).json")
    }
    static func load(_ warden:String)->[ChatTurn] {
        guard let data=try? Data(contentsOf:url(warden)),data.count <= 1_000_000,
              let entries=try? JSONDecoder().decode([Entry].self,from:data) else{return []}
        let cutoff=Date().timeIntervalSince1970-30*86400
        let turns=entries.filter{$0.at>cutoff && ["user","assistant"].contains($0.role)}.suffix(200).map{ChatTurn(role:$0.role,content:$0.content,at:$0.at)}
        save(turns,warden:warden);return turns
    }
    static func save(_ turns:[ChatTurn],warden:String) {
        let target=url(warden);let folder=target.deletingLastPathComponent()
        let cutoff=Date().timeIntervalSince1970-30*86400
        let entries=turns.filter{$0.at>cutoff}.suffix(200).map{Entry(role:$0.role,content:boundedChatText($0.content),at:$0.at)}
        guard let data=try? JSONEncoder().encode(entries) else{return}
        do {try FileManager.default.createDirectory(at:folder,withIntermediateDirectories:true,attributes:[.posixPermissions:0o700]);try data.write(to:target,options:.atomic);try FileManager.default.setAttributes([.posixPermissions:0o600],ofItemAtPath:target.path)} catch { /* Chat remains usable if local recall cannot be saved. */ }
    }
    static func context(_ turns:[ChatTurn],query:String)->[[String:String]] {
        let words=Set(query.lowercased().split{!$0.isLetter && !$0.isNumber}.filter{$0.count>3}.map(String.init))
        var scored:[(index:Int,turn:ChatTurn,score:Int)] = []
        for (index,turn) in turns.dropLast(4).enumerated() {
            let text=turn.content.lowercased()
            let score=words.filter{text.contains($0)}.count
            if score>0 {scored.append((index,turn,score))}
        }
        scored.sort {a,b in a.score == b.score ? a.index > b.index : a.score > b.score}
        let selected=Array(scored.prefix(3)).sorted{$0.index<$1.index}
        let old=selected.map{$0.turn}
        return (old+turns.suffix(4)).map{["role":$0.role,"content":boundedChatText($0.content,bytes:2400)]}
    }
}
