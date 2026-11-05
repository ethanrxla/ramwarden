import AppKit
import SwiftUI
import Combine

func boundedChatText(_ text: String, bytes: Int = 4000) -> String {
    var value = String(text.prefix(bytes))
    while value.utf8.count > bytes { value.removeLast() }
    return value
}

struct ReviewTarget: Codable { let browser: String; let id: Int64; let url: String }
struct ReviewTab: Decodable, Identifiable {
    let browser: String; let id: Int64; let url: String; let title: String; let reason: String
    var key: String { "\(browser):\(id):\(url)" }
    var target: ReviewTarget { ReviewTarget(browser:browser,id:id,url:url) }
}
struct TabReview: Decodable { let token: String; let expires_at: Double; let tabs: [ReviewTab] }
struct TabReviewEnvelope: Decodable { let proposal: TabReview? }

struct FocusChoice: Decodable, Identifiable {
    let browser: String; let id: Int64; let url: String; let title: String; let selected: Bool
    var key: String { "\(browser):\(id):\(url)" }
    var target: ReviewTarget { ReviewTarget(browser:browser,id:id,url:url) }
}
struct FocusProposal: Decodable { let token: String; let tabs: [FocusChoice]; let expires_at: Double }
@MainActor final class CompanionModel: ObservableObject {
    let profile: WardenProfile
    init(profile:WardenProfile = RamWardenProfile(),persistMemory:Bool = true) {
        self.profile = profile
        self.companionVisible = UserDefaults.standard.bool(forKey:"\(profile.id).visible")
        self.name = UserDefaults.standard.string(forKey:"\(profile.id).name") ?? profile.name
        for (key,fallback) in [("wool",profile.id == "threat" ? "9AA3AD" : "D84545"),("horns",profile.id == "threat" ? "7C3AED" : "F5DEAA"),("hooves",profile.id == "threat" ? "1A0F2E" : "29242B"),("eyes",profile.id == "threat" ? "39FF88" : "FFFFFF"),("pupilStyle",profile.id == "threat" ? "none" : "slit")] {
            let value=UserDefaults.standard.string(forKey:"\(profile.id).\(key)") ?? fallback
            switch key {case "wool":self.wool=value;case "horns":self.horns=value;case "hooves":self.hooves=value;case "eyes":self.eyes=value;default:self.pupilStyle=value}
        }
        self.memoryReady = persistMemory
        if persistMemory {
            let memoryProfile=profile.id
            CompanionRecall.storage.async { [weak self] in
                let loaded=CompanionRecall.load(memoryProfile)
                DispatchQueue.main.async {guard let self=self,!self.discardLoadedMemory else{return};self.turns=Array((loaded+self.turns).sorted{$0.at<$1.at}.suffix(200))}
            }
        }
    }
    var chatHeight: CGFloat { tabReview != nil ? 440 : focusProposal != nil ? 360 : turns.isEmpty ? 80 : 240 }
    @Published var tabReview: TabReview?
    @Published var selectedReviewTabs: Set<String> = []
    @Published var reviewBusy = false
    private var reviewPolling = false
    @MainActor func pollReview() async {
        guard companionVisible, !reviewPolling, !reviewBusy else { return }
        reviewPolling = true; defer { reviewPolling = false }
        do {
            var req = URLRequest(url:base.appendingPathComponent("browser/review")); req.timeoutInterval=8
            let (data,response)=try await URLSession.shared.data(for:req)
            guard (response as? HTTPURLResponse)?.statusCode == 200 else { return }
            let next=try JSONDecoder().decode(TabReviewEnvelope.self,from:data).proposal
            if next?.token != tabReview?.token { selectedReviewTabs=[] }
            tabReview=next
            if let review=next,review.tabs.count >= 8 {
                GoatAlerts.shared.notify(warden:"ram",event:review.token,critical:false)
            }
        } catch { /* Leave uncertain tabs open when the daemon is unreachable. */ }
    }
    func answerReview(_ action: String) {
        guard let proposal=tabReview, !reviewBusy else { return }
        guard Date().timeIntervalSince1970 < proposal.expires_at else { tabReview=nil; return }
        let targets=proposal.tabs.filter { selectedReviewTabs.contains($0.key) }.map { $0.target }
        reviewBusy=true
        Task { @MainActor in
            defer { reviewBusy=false }
            do {
                struct Reply: Encodable { let token:String; let action:String; let tabs:[ReviewTarget] }
                var req=URLRequest(url:base.appendingPathComponent("browser/review/respond"))
                req.httpMethod="POST"; req.timeoutInterval=100
                req.setValue("application/json",forHTTPHeaderField:"Content-Type")
                req.httpBody=try JSONEncoder().encode(Reply(token:proposal.token,action:action,tabs:targets))
                let (data,response)=try await URLSession.shared.data(for:req)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                    tabReview=nil; error="This review expired or changed. Tabs were kept unless a close was already confirmed."; return
                }
                if let result=try? JSONSerialization.jsonObject(with:data) as? [String:Any],
                   let refused=result["refused"] as? [Any], !refused.isEmpty {
                    error="Some tabs changed or became active and were kept."
                }
                tabReview=nil; selectedReviewTabs=[]
            } catch { self.error="Could not confirm the result. Check your tabs before trying again."; tabReview=nil }
        }
    }
    @Published var companionVisible = UserDefaults.standard.bool(forKey:"ram.visible")
    @Published var connected = false
    @Published var focusMode = false
    @Published var focusProposal: FocusProposal?
    @Published var focusSelected: Set<String> = []
    @Published var focusFeedback = ""
    @Published var thinking = false
    @Published var speaking = false
    private var memoryReady=false
    private var discardLoadedMemory=false
    func forgetHistory(){discardLoadedMemory=true;turns=[]}
    @Published var turns: [ChatTurn] = [] {didSet {if memoryReady {CompanionRecall.saveLater(turns,warden:profile.id)}}}
    @Published var error: String?
    @Published var tier = ""
    @Published var name = UserDefaults.standard.string(forKey:"ram.name") ?? "Warden"
    @Published var proposedToken: String?
    @Published var proposedLabel = ""
    @Published var proposedPreview = ""
    @Published var workspaceStatus = "No folder connected"
    @Published var workspaceScope = ""
    @Published var automaticTools = false
    @Published var runningTools = false
    @Published var toolProgress = ""
    @Published var toolBlocks: [WorkspaceToolBlock] = []
    private var proposedScope = ""
    private var proposedAutoEligible = false
    @Published var wool = UserDefaults.standard.string(forKey: "ram.wool") ?? "D84545"
    @Published var horns = UserDefaults.standard.string(forKey: "ram.horns") ?? "F5DEAA"
    @Published var hooves = UserDefaults.standard.string(forKey: "ram.hooves") ?? "29242B"
    @Published var eyes = UserDefaults.standard.string(forKey: "ram.eyes") ?? "FFFFFF"
    @Published var pupilStyle = UserDefaults.standard.string(forKey: "ram.pupilStyle") ?? "slit"
    private var task: Task<Void, Never>?
    func saveColors() {
        for (key, value) in [("wool", wool), ("horns", horns), ("hooves", hooves), ("eyes", eyes), ("pupilStyle", pupilStyle)] {
            UserDefaults.standard.set(value, forKey: "\(profile.id).\(key)")
        }
    }
    func saveFocus(_ goal: String) {
        guard goal.count <= 500 else { error = "Keep your focus under 500 characters."; return }
        thinking = true
        turns.append(ChatTurn(role:"user",content:goal))
        Task { @MainActor in
            defer { thinking = false }
            do {
                var req = URLRequest(url:base.appendingPathComponent("warden/focus"))
                req.httpMethod = "POST"; req.timeoutInterval = 60
                req.setValue("application/json",forHTTPHeaderField:"Content-Type")
                req.httpBody = try JSONSerialization.data(withJSONObject:["goal":goal,"name":name])
                let (data,response) = try await URLSession.shared.data(for:req)
                guard (response as? HTTPURLResponse)?.statusCode == 200,
                      let obj = try JSONSerialization.jsonObject(with:data) as? [String:Any],
                      let reply = obj["reply"] as? String else { error = "Focus wasn't saved. Please try again."; return }
                focusMode = false; error = nil; focusFeedback = ""
                if let proposal = obj["proposal"], !(proposal is NSNull) {
                    focusProposal = try JSONDecoder().decode(FocusProposal.self,from:JSONSerialization.data(withJSONObject:proposal))
                    focusSelected = Set(focusProposal?.tabs.filter { $0.selected }.map { $0.key } ?? [])
                } else { focusProposal = nil }
                turns.append(ChatTurn(role:"assistant",content:reply))
                if companionVisible { CompanionSpeech.shared.say(reply,warden:profile.id) }
            } catch { self.error = "Couldn't reach Warden to save your focus." }
        }
    }
    func applyFocusFeedback() {
        guard let proposal = focusProposal, !thinking else { return }
        guard Date().timeIntervalSince1970 < proposal.expires_at else { focusProposal = nil; error = "That tab list expired. Tell me your focus again."; return }
        thinking = true
        Task { @MainActor in
            defer { thinking = false }
            do {
                struct Feedback: Encodable { let token:String; let selected:[ReviewTarget] }
                let tabs = proposal.tabs.filter { focusSelected.contains($0.key) }.map { $0.target }
                var req = URLRequest(url:base.appendingPathComponent("warden/focus/feedback"))
                req.httpMethod = "POST"; req.timeoutInterval = 8
                req.setValue("application/json",forHTTPHeaderField:"Content-Type")
                req.httpBody = try JSONEncoder().encode(Feedback(token:proposal.token,selected:tabs))
                let (_,response) = try await URLSession.shared.data(for:req)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else { error = "Couldn't update that tab list. Try your focus again."; return }
                focusProposal = nil; focusFeedback = "Preferences updated · \(tabs.count) tabs marked relevant"
            } catch { self.error = "Couldn't update your tab preferences." }
        }
    }
    func cancelConversation() {
        if runningTools {
            turns.append(ChatTurn(role:"assistant",content:"Stopped waiting for this plan. Completed changes remain. An in-flight file step may finish; inspect its result before retrying."))
            markUnconfirmedTools()
        }
        task?.cancel(); if task == nil {thinking=false}; proposedToken=nil;proposedAutoEligible=false
    }
    func markUnconfirmedTools() {
        for index in toolBlocks.indices where toolBlocks[index].state == "Running" { toolBlocks[index].state = "Unconfirmed" }
    }
    func receiveToolEvent(_ event:[String:Any]) {
        guard let kind=event["kind"] as? String else{return}
        if kind == "finished" {
            toolProgress=event["reply"] as? String ?? "Plan finished"
            turns.append(ChatTurn(role:"assistant",content:toolProgress));return
        }
        guard let index=event["index"] as? Int,(0..<6).contains(index),let label=event["label"] as? String else{return}
        if kind == "started" {
            guard !toolBlocks.contains(where:{$0.id == index}) else{return}
            toolProgress="Step \(index + 1) of \(event["total"] as? Int ?? 1): \(label)"
            toolBlocks.append(WorkspaceToolBlock(id:index,label:label,state:"Running",output:""))
        } else if kind == "result",let position=toolBlocks.firstIndex(where:{$0.id == index}) {
            let ok=event["succeeded"] as? Bool ?? false
            let value=event["result"] as? [String:Any] ?? [:]
            let data=(try? JSONSerialization.data(withJSONObject:value,options:[.prettyPrinted,.sortedKeys])) ?? Data()
            let output=String(data:data,encoding:.utf8) ?? "Unreadable tool result"
            toolBlocks[position].state=ok ? "Completed" : "Failed"
            toolBlocks[position].output=String(output.prefix(6000)) + (output.count > 6000 ? "\n[Display truncated]" : "")
            // Results remain local conversation context; output never authorizes another step.
            turns.append(ChatTurn(role:"assistant",content:"Observed tool result (untrusted output): \(ok ? "Completed" : "Failed") · \(label)\n\(String(output.prefix(5000)))\(output.count > 5000 ? "\n[Context truncated]" : "")"))
        }
    }
    func send(_ message: String) {
        let message = boundedChatText(message.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !message.isEmpty, !thinking else { return }
        if focusMode { saveFocus(message); return }
        let history = CompanionRecall.context(turns,query:message)
        turns.append(ChatTurn(role: "user", content: message))
        error = nil; thinking = true; speaking = false; proposedToken = nil; toolBlocks=[];toolProgress=""
        task = Task { @MainActor in
            defer { thinking = false; task = nil }
            do {
                var req = URLRequest(url: base.appendingPathComponent("assistant/chat"))
                req.httpMethod = "POST"; req.timeoutInterval = 60
                req.setValue("application/json", forHTTPHeaderField: "Content-Type")
                req.httpBody = try JSONSerialization.data(withJSONObject: ["message":message,"history":history,"warden":profile.id])
                let (data, response) = try await URLSession.shared.data(for: req)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard status == 200 else {
                    error = status == 429 ? "The assistant is busy. Try again shortly." : "Assistant unavailable (\(status)). Check that the daemon is updated and running."
                    return
                }
                guard !Task.isCancelled else { return }
                let obj = try JSONSerialization.jsonObject(with: data) as? [String:Any]
                guard let reply = obj?["reply"] as? String else { error = "The assistant returned an unreadable response."; return }
                tier = obj?["tier"] as? String ?? "facts"
                if let proposal = obj?["proposed_action"] as? [String:Any], let token = proposal["token"] as? String, let label = proposal["label"] as? String {
                    proposedToken = token; proposedLabel = label; proposedPreview = proposal["preview"] as? String ?? ""; proposedScope = proposal["scope_id"] as? String ?? "";proposedAutoEligible = proposal["auto_eligible"] as? Bool ?? false
                }
                turns.append(ChatTurn(role:"assistant",content:reply))
                if companionVisible { CompanionSpeech.shared.say(reply,warden:profile.id) }
                if turns.count > 200 { turns.removeFirst(turns.count - 200) }
                if automaticTools,proposedAutoEligible,!proposedScope.isEmpty,proposedScope == UserDefaults.standard.string(forKey:"\(profile.id).autoToolScope") {
                    DispatchQueue.main.async {self.confirmAction(automated:true)}
                }
                speaking = true
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 3_000_000_000)
                    self.speaking = false
                }
            } catch { if !Task.isCancelled { self.error = "Cannot reach the assistant. RamWarden will reconnect automatically." } }
        }
    }
    func refreshWorkspace(){
        Task { @MainActor in
            var request=URLRequest(url:base.appendingPathComponent("assistant/workspace").appending(queryItems:[URLQueryItem(name:"warden",value:profile.id)]));request.timeoutInterval=5
            guard let (data,response)=try? await URLSession.shared.data(for:request),(response as? HTTPURLResponse)?.statusCode==200,let status=(try? JSONSerialization.jsonObject(with:data)) as? [String:Any] else{return}
            let workspace=status["workspace"] as? [String:Any]
            workspaceStatus=workspace?["path"] as? String ?? "No folder connected"
            workspaceScope=workspace?["scope_id"] as? String ?? ""
            automaticTools = !workspaceScope.isEmpty && workspaceScope == UserDefaults.standard.string(forKey:"\(profile.id).autoToolScope")
        }
    }
    func chooseWorkspace(){
        let panel=NSOpenPanel();panel.canChooseDirectories=true;panel.canChooseFiles=false;panel.allowsMultipleSelection=false
        panel.message="Choose the project folder Warden may use. Every file or terminal operation still needs your approval."
        if panel.runModal() == .OK,let url=panel.url {setWorkspace(url.path)}
    }
    func setAutomaticTools(_ enabled:Bool){
        if !enabled {automaticTools=false;UserDefaults.standard.removeObject(forKey:"\(profile.id).autoToolScope");return}
        guard !workspaceScope.isEmpty else{workspaceStatus="Choose a folder before enabling Auto.";return}
        let alert=NSAlert();alert.messageText="Enable Auto for this folder?"
        alert.informativeText="Warden may read and list files, create new files and folders, and run the supported Git diagnostics without asking again inside \(workspaceStatus). Existing-file edits and multi-step plans always require review. Auto cannot delete files, run arbitrary commands, or change security settings. You can return to Manual at any time."
        alert.addButton(withTitle:"Enable Auto");alert.addButton(withTitle:"Stay Manual")
        if alert.runModal() == .alertFirstButtonReturn {automaticTools=true;UserDefaults.standard.set(workspaceScope,forKey:"\(profile.id).autoToolScope")}
    }
    func setWorkspace(_ path:String?){
        setAutomaticTools(false)
        Task { @MainActor in
            do {
                var request=URLRequest(url:base.appendingPathComponent("assistant/workspace"));request.httpMethod="POST";request.timeoutInterval=8
                request.setValue("application/json",forHTTPHeaderField:"Content-Type")
                request.httpBody=try JSONSerialization.data(withJSONObject:["path":path as Any? ?? NSNull(),"warden":profile.id])
                let (_,response)=try await URLSession.shared.data(for:request)
                guard (response as? HTTPURLResponse)?.statusCode==200 else{workspaceStatus="Could not connect that folder. Try again.";return}
                workspaceStatus=path ?? "No folder connected";proposedToken=nil;refreshWorkspace()
            }catch{workspaceStatus="Cannot reach the daemon"}
        }
    }
    func confirmAction(automated:Bool = false) {
        guard let token = proposedToken, !thinking else { return }
        if automated && (!automaticTools || !proposedAutoEligible || proposedScope.isEmpty || proposedScope != UserDefaults.standard.string(forKey:"\(profile.id).autoToolScope")){return}
        if !automated {
        let alert = NSAlert()
        alert.messageText = proposedPreview.isEmpty ? proposedLabel : "Review Warden’s action"
        if !proposedPreview.isEmpty {
            let scroll=NSScrollView(frame:NSRect(x:0,y:0,width:520,height:260));scroll.hasVerticalScroller=true
            let text=NSTextView(frame:scroll.bounds);text.isEditable=false;text.isSelectable=true;text.font=NSFont.monospacedSystemFont(ofSize:11,weight:.regular)
            text.string=proposedPreview;text.isVerticallyResizable=true;text.autoresizingMask=[.width];scroll.documentView=text;alert.accessoryView=scroll
        }
        alert.informativeText = "Run this reviewed plan? Steps stop on failure. Completed changes remain if a later step fails or you stop."
        alert.addButton(withTitle:"Run plan"); alert.addButton(withTitle:"Cancel")
        NSApp.activate(ignoringOtherApps:true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        }
        let streamWorkspace = !proposedScope.isEmpty
        proposedToken = nil; thinking = true;runningTools=streamWorkspace;toolBlocks=[];toolProgress="Starting reviewed plan"
        task = Task { @MainActor in
            defer { markUnconfirmedTools();thinking = false;runningTools=false;task=nil }
            do {
                var request = URLRequest(url:base.appendingPathComponent(streamWorkspace ? "assistant/action/stream" : "assistant/action"))
                request.httpMethod = "POST"; request.timeoutInterval = streamWorkspace ? 90 : 20
                request.setValue("application/json",forHTTPHeaderField:"Content-Type")
                request.httpBody = try JSONSerialization.data(withJSONObject:["token":token])
                if streamWorkspace {
                    request.setValue("text/event-stream",forHTTPHeaderField:"Accept")
                    let (bytes,response)=try await URLSession.shared.bytes(for:request)
                    guard (response as? HTTPURLResponse)?.statusCode == 200 else {error="That plan expired or is unavailable. Ask for a new plan.";return}
                    var finished=false
                    for try await line in bytes.lines {
                        try Task.checkCancellation()
                        guard line.hasPrefix("data: "),let data=String(line.dropFirst(6)).data(using:.utf8),let event=(try? JSONSerialization.jsonObject(with:data)) as? [String:Any] else{continue}
                        receiveToolEvent(event)
                        if event["kind"] as? String == "finished" {finished=true}
                    }
                    if !finished {error="The plan's final result is unconfirmed. Inspect completed steps before retrying."}
                    return
                }
                let (data,response) = try await URLSession.shared.data(for:request)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else { error = "That proposal expired or the computer changed. Ask me to check again."; return }
                let result = try JSONSerialization.jsonObject(with:data) as? [String:Any]
                turns.append(ChatTurn(role:"assistant",content:result?["reply"] as? String ?? "The action finished without a report."))
            } catch { if !Task.isCancelled {self.error = "I lost contact with the daemon. Inspect completed steps before retrying."} }
        }
    }

}
struct ChatTurn: Identifiable, Sendable {
    let id = UUID(); let role: String; let content: String; var at:Double=Date().timeIntervalSince1970
    var toolResult:(label:String,output:String)? {
        let prefix="Observed tool result (untrusted output):"
        guard role == "assistant",content.hasPrefix(prefix),let newline=content.firstIndex(of:"\n") else{return nil}
        let label=String(content[content.index(content.startIndex,offsetBy:prefix.count)..<newline]).trimmingCharacters(in:.whitespaces)
        return (label.isEmpty ? "Tool result" : label,String(content[content.index(after:newline)...]))
    }
}
struct WorkspaceToolBlock: Identifiable { let id:Int;let label:String;var state:String;var output:String }
func ramColor(_ hex: String) -> Color {
    let n = UInt32(hex, radix:16) ?? 0
    return Color(red:Double((n >> 16) & 255)/255,green:Double((n >> 8) & 255)/255,blue:Double(n & 255)/255)
}

struct PixelRam: View {
    @ObservedObject var model: CompanionModel
    var animated = true
    func palette(_ key: Character) -> Color {
        func tint(_ hex: String, _ factor: Double) -> Color {
            let value = UInt32(hex,radix:16) ?? 0
            return Color(red:min(1,Double((value>>16)&255)/255*factor),green:min(1,Double((value>>8)&255)/255*factor),blue:min(1,Double(value&255)/255*factor))
        }
        switch key {
        case "w": return ramColor(model.wool)
        case "s": return tint(model.wool,0.62)
        case "b": return tint(model.wool,1.3)
        case "h": return ramColor(model.horns)
        case "H": return tint(model.horns,1.25)
        case "j": return tint(model.horns,0.66)
        case "k": return ramColor(model.hooves)
        case "e": return ramColor(model.eyes)
        case "E": return tint(model.eyes,0.76)
        default: return Color(red:0.10,green:0.05,blue:0.08)
        }
    }
    func sprite(_ frame: Int) -> [Character] {
        var cells = [Character](repeating:".",count:48*48)
        func put(_ x: Int,_ y: Int,_ key: Character) { if (0..<48).contains(x) && (0..<48).contains(y) { cells[y*48+x] = key } }
        func box(_ x: Int,_ y: Int,_ w: Int,_ h: Int,_ key: Character) { for yy in y..<(y+h) { for xx in x..<(x+w) { put(xx,yy,key) } } }
        func ellipse(_ cx: Double,_ cy: Double,_ rx: Double,_ ry: Double,_ wool: Bool) {
            for y in 0..<48 { for x in 0..<48 {
                let dx=Double(x)-cx,dy=Double(y)-cy
                guard dx*dx/(rx*rx)+dy*dy/(ry*ry)<=1 else { continue }
                let edge=dx*dx/((rx-1.2)*(rx-1.2))+dy*dy/((ry-1.2)*(ry-1.2))>1
                put(x,y,edge ? "o" : wool ? (dx>rx*0.45 || dy>ry*0.55 ? "s" : dx < -rx*0.35 && dy < 0 ? "b" : "w") : "k")
            } }
        }
        // Tiny body, plush outline, and two cloven hooves.
        box(17,42,5,6,"o"); box(27,42,5,6,"o")
        box(18,43,3,4,"k"); box(28,43,3,4,"k")
        ellipse(24,38,9,7,true)
        box(18,34,2,3,"b"); box(29,35,2,3,"s")
        // Pixel-quantized curls with an outline, warm highlight, and inner shade.
        for center in [8.0,40.0] {
            for y in 10..<33 { for x in 0..<48 {
                let dx=Double(x)-center,dy=Double(y)-21
                let r=(dx*dx+dy*dy).squareRoot()
                guard r<=9.4 && r>=3.7 else { continue }
                let inward = center<24 ? dx>1 : dx < -1
                if inward && dy>3 { continue }
                let key: Character = r>8.2 || r<4.8 ? "o" : dy < -2 ? "H" : dy>3 ? "j" : "h"
                put(x,y,key)
            } }
            let x=Int(center)
            box(x-1,20,3,2,"h"); box(x,21,2,4,"H")
        }
        // Ears and a large, expressive head.
        ellipse(11,23,5,4,true); ellipse(37,23,5,4,true)
        for y in 12...36 {
            let half = y<17 ? 8+(y-12) : y<23 ? 12 : max(4,12-(y-22)*2/3)
            for x in (24-half)...(24+half) {
                let edge=x==24-half || x==24+half || y==12 || y==36
                put(x,y,edge ? "o" : x>27 || y>29 ? "s" : x<19 ? "b" : "w")
            }
        }
        box(21,10,3,2,"o"); box(24,9,2,3,"o"); box(22,11,5,3,"b")
        let blink = frame % 25 == 0 && !model.thinking
        for x in [15,26] {
            box(x-1,19,9,6,"o")
            if blink { box(x,22,7,1,"E") }
            else {
                box(x,20,7,4,"e"); box(x+1,23,5,1,"E")
                if model.pupilStyle == "slit" { box(x+1,21,5,1,"o") }
                else if model.pupilStyle == "round" { box(x+3,21,2,2,"o"); put(x+3,20,"e") }
            }
        }
        // Long tapered muzzle, flat nose pad, and a pointed beard below the chin.
        box(22,27,5,3,"b")
        box(20,31,9,2,"o"); box(21,33,7,1,"o")
        put(21,31,"s"); put(27,31,"s")
        if model.speaking && frame%2==0 { box(23,34,3,2,"o") }
        else { box(23,35,3,1,"o") }
        box(22,37,5,2,"o"); box(23,37,3,3,"s"); put(24,40,"o")
        return cells
    }
    var body: some View {
        TimelineView(.animation(minimumInterval:0.25,paused:!animated)) { tick in
            let frame=Int(tick.date.timeIntervalSinceReferenceDate*4)
            Canvas { context,size in
                let cell=floor(min(size.width/48,(size.height-18)/48))
                let originX=floor((size.width-cell*48)/2)
                let bob = model.thinking ? (frame%2==0 ? 0.0 : 2.0) : (frame%8<4 ? 0.0 : 1.0)
                let cells=sprite(frame)
                // Batch disjoint pixel rectangles by color: parse each palette entry and
                // submit one fill per color, rather than one per occupied pixel.
                var paths: [Character: Path] = [:]
                for y in 0..<48 { for x in 0..<48 {
                    let key=cells[y*48+x]
                    if key != "." {
                        paths[key, default: Path()].addRect(CGRect(x:originX+Double(x)*cell,y:Double(y)*cell+bob,width:cell,height:cell))
                    }
                } }
                for (key,path) in paths {
                    context.fill(path,with:.color(palette(key).opacity(model.connected ? 1 : 0.60)))
                }
                if !model.connected || model.thinking {
                    let label = !model.connected ? "Reconnecting…" : "Thinking…"
                    context.draw(Text(label).font(.system(size:10,weight:.medium)).foregroundColor(.primary),at:CGPoint(x:size.width/2,y:size.height-6))
                }
            }
        }.accessibilityLabel(model.connected ? "RamWarden ram companion. Click to open assistant." : "RamWarden companion. Reconnecting to daemon.")
    }
}

// Native clear Liquid Glass follows system contrast/transparency preferences.
struct ChatGlass: ViewModifier {
    @ViewBuilder func body(content: Content) -> some View {
        // Xcode 26 ships Swift 6.2 and the SDK that declares glassEffect.
        // The runtime check alone cannot compile against the older CI SDK.
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            content.glassEffect(.clear, in: RoundedRectangle(cornerRadius:20))
        } else {
            content.background(.ultraThinMaterial,in:RoundedRectangle(cornerRadius:20))
        }
        #else
        content.background(.ultraThinMaterial,in:RoundedRectangle(cornerRadius:20))
        #endif
    }
}

struct AssistantView: View {
    @ObservedObject var model: CompanionModel
    var dismiss: () -> Void
    var settings: () -> Void
    @State private var message = ""
    @FocusState private var inputFocused: Bool
    var body: some View {
        VStack(alignment:.leading,spacing:8) {
            HStack(spacing:8) {
                Circle().fill(model.connected ? Color.green : Color.orange).frame(width:6,height:6)
                Text(model.name).font(.system(size:12,weight:.semibold))
                Spacer()
                CompanionMuteMenu(warden:model.profile.id)
                Button {model.chooseWorkspace()} label: {
                    Image(systemName:model.workspaceScope.isEmpty ? "folder.badge.plus" : "folder.fill").font(.system(size:11))
                }.buttonStyle(.plain).disabled(model.thinking).accessibilityLabel("Choose workspace folder")
                    .help("\(model.workspaceStatus). Choose the folder for this companion.")
                Button(action:settings) { Image(systemName:"gearshape").font(.system(size:11)) }.buttonStyle(.plain).accessibilityLabel("Settings")
                Button(action:dismiss) { Image(systemName:"xmark").font(.system(size:10,weight:.semibold)) }
                    .buttonStyle(.plain).accessibilityLabel("Close chat")
            }
            if !model.turns.isEmpty {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment:.leading,spacing:10) {
                        if model.turns.isEmpty { Text("Ask me what’s using RAM or what I’ve been doing.").foregroundStyle(.secondary) }
                        ForEach(model.turns) { turn in
                            VStack(alignment:.leading,spacing:4) {
                                Text(turn.role == "user" ? "You" : model.name).font(.system(size:10,weight:.semibold)).foregroundStyle(.secondary)
                                if let result=turn.toolResult {
                                    DisclosureGroup(result.label) {
                                        Text(result.output).font(.system(size:10,design:.monospaced)).textSelection(.enabled)
                                    }.font(.caption).padding(6).background(Color.primary.opacity(0.05),in:RoundedRectangle(cornerRadius:6))
                                } else {Text(turn.content).font(.system(size:12)).textSelection(.enabled)}
                            }.frame(maxWidth:.infinity,alignment:.leading).id(turn.id)
                        }
                        ForEach(model.toolBlocks.filter{$0.state == "Running" || $0.state == "Unconfirmed"}) { block in
                            DisclosureGroup("\(block.state) · \(block.label)") {
                                Text(block.output.isEmpty ? "Waiting for tool output" : block.output)
                                    .font(.system(size:10,design:.monospaced)).textSelection(.enabled)
                            }.font(.caption).padding(6).background(Color.primary.opacity(0.05),in:RoundedRectangle(cornerRadius:6)).id("workspace-\(block.id)")
                        }
                    }.padding(8)
                }.onChange(of:model.turns.count) { _ in if let last = model.turns.last { proxy.scrollTo(last.id,anchor:.bottom) } }
                .onChange(of:model.toolBlocks.count) { _ in if let last=model.toolBlocks.last {proxy.scrollTo("workspace-\(last.id)",anchor:.bottom)} }
            }
            }
            if let review=model.tabReview {
                Text("These tabs may still matter. Choose what to keep or close.").font(.caption)
                ScrollView {
                    VStack(alignment:.leading) {
                        ForEach(review.tabs, id: \.key) { tab in
                            Toggle(isOn:Binding(get:{ model.selectedReviewTabs.contains(tab.key) },set:{ on in
                                if on { model.selectedReviewTabs.insert(tab.key) } else { model.selectedReviewTabs.remove(tab.key) }
                            })) {
                                VStack(alignment:.leading) {
                                    Text(tab.title).lineLimit(2)
                                    Text(URL(string:tab.url)?.host ?? "Web page").foregroundStyle(.secondary)
                                    Text(tab.reason).font(.caption2).foregroundStyle(.secondary)
                                }
                            }.toggleStyle(.checkbox)
                        }
                    }
                }.frame(maxHeight:220)
                HStack {
                    Button("Close selected") { model.answerReview("close") }.disabled(model.selectedReviewTabs.isEmpty)
                    Button("Keep selected") { model.answerReview("keep") }.disabled(model.selectedReviewTabs.isEmpty)
                    Button("Later") { model.answerReview("later") }
                }.font(.caption).controlSize(.small).disabled(model.reviewBusy)
            }
            if let proposal = model.focusProposal {
                Text("Relevant to this task").font(.caption.weight(.semibold))
                ScrollView {
                    VStack(alignment:.leading,spacing:5) {
                        ForEach(proposal.tabs,id:\.key) { tab in
                            Toggle(isOn:Binding(get:{model.focusSelected.contains(tab.key)},set:{ on in
                                if on { model.focusSelected.insert(tab.key) } else { model.focusSelected.remove(tab.key) }
                            })) {
                                VStack(alignment:.leading) {
                                    Text(tab.title.isEmpty ? "Web page" : tab.title).lineLimit(2)
                                    Text(URL(string:tab.url)?.host ?? "Web page").font(.caption2).foregroundStyle(.secondary)
                                }
                            }.toggleStyle(.checkbox)
                        }
                    }
                }.frame(maxHeight:130)
                Button("Use these preferences") { model.applyFocusFeedback() }.controlSize(.small).disabled(model.thinking)
            }
            if !model.focusFeedback.isEmpty { Text(model.focusFeedback).font(.caption2).foregroundStyle(.secondary) }
            if model.focusMode {
                HStack {
                    Button("Clear focus") { model.saveFocus("") }
                    Button("Cancel") { model.focusMode = false }
                }.font(.caption)
            }
            if model.thinking {
                TimelineView(.periodic(from: .now, by: 3)) { tick in
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(model.runningTools ? model.toolProgress : model.profile.thinkingLine(at:tick.date))
                            if model.runningTools {Button("Stop plan") {model.cancelConversation()}.font(.caption)}
                            else {Text("Waiting for a response").font(.caption2).foregroundStyle(.secondary)}
                        }
                    }.accessibilityLabel("Waiting for a response.")
                }
            }
            if let error = model.error { Text(error).foregroundStyle(.red).font(.caption) }
            if model.proposedToken != nil {
                Button("Review plan…") { model.confirmAction() }.font(.system(size:12,weight:.semibold))
                    .help(model.proposedLabel)
            }
            HStack {
                TextField(model.focusMode ? "What are you working on?" : model.profile.placeholder,text:$message).focused($inputFocused).textFieldStyle(.plain).font(.system(size:12)).onSubmit { send() }
                Button(action:send) { Image(systemName:"arrow.up.circle.fill").font(.system(size:19)) }.buttonStyle(.plain).accessibilityLabel("Send message").disabled(model.thinking || message.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty)
            }
        }.padding(12).frame(width:288,height:model.chatHeight - 16)
            .modifier(ChatGlass())
            .padding(8)
            .onAppear { inputFocused = true;model.refreshWorkspace() }
            .onReceive(NotificationCenter.default.publisher(for: Notification.Name("WardenChatOpened"))) { note in if note.object as? String == model.profile.id { inputFocused = true;model.refreshWorkspace() } }
            .onExitCommand(perform:dismiss)
    }
    func send() { model.send(message); message = "" }
}


final class PetPanel: NSPanel { override var canBecomeKey: Bool { false } }
final class ChatPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}
final class PetHost: NSHostingView<PixelRam> {
    var onClick: (() -> Void)?
    var onMoved: (() -> Void)?
    var onSettings: (() -> Void)?
    var onFocus: (() -> Void)?
    var onQuiet: (() -> Void)?
    var isQuiet: (() -> Bool)?
    var onHide: (() -> Void)?
    override func rightMouseDown(with event: NSEvent) {
        let menu = NSMenu()
        menu.autoenablesItems = false
        for (title, action) in [("Chat",#selector(chatAction)),("Set current focus…",#selector(focusAction)),("Settings…",#selector(settingsAction)),(isQuiet?() == true ? "Unmute companion" : "Mute companion",#selector(quietAction)),("Hide ram",#selector(hideAction))] {
            let item = NSMenuItem(title:title,action:action,keyEquivalent:"")
            item.target = self; item.isEnabled = true; menu.addItem(item)
        }
        NSMenu.popUpContextMenu(menu,with:event,for:self)
    }
    @objc func chatAction() { onClick?() }
    @objc func focusAction() { onFocus?() }
    @objc func settingsAction() { onSettings?() }
    @objc func quietAction() { onQuiet?() }
    @objc func hideAction() { onHide?() }

    var dragged = false
    var dragStart: NSPoint = .zero
    var windowStart: NSPoint = .zero
    override func mouseDown(with event: NSEvent) {
        dragged = false; dragStart = NSEvent.mouseLocation
        windowStart = window?.frame.origin ?? .zero
    }
    override func mouseDragged(with event: NSEvent) {
        guard let window = window else { return }
        let mouse = NSEvent.mouseLocation
        guard dragged || hypot(mouse.x-dragStart.x,mouse.y-dragStart.y) > 3 else { return }
        dragged = true
        let desired = NSPoint(x:windowStart.x+mouse.x-dragStart.x,y:windowStart.y+mouse.y-dragStart.y)
        let bounds = NSScreen.screens.first(where: { $0.frame.contains(mouse) })?.visibleFrame ?? window.screen?.visibleFrame
        if let bounds = bounds {
            window.setFrame(PetSizing.resizedFrame(NSRect(origin:desired,size:window.frame.size),size:window.frame.size,bounds:bounds),display:true)
        } else { window.setFrameOrigin(desired) }
        onMoved?()
        UserDefaults.standard.set(window.frame.minX,forKey:"ram.x")
        UserDefaults.standard.set(window.frame.minY,forKey:"ram.y")
    }
    override func mouseUp(with event: NSEvent) { if !dragged { onClick?() } }
}
@MainActor final class Companion: WardenCompanion, NSWindowDelegate {
    let voice = VoiceModel()
    var pet: PetPanel?
    var chat: ChatPanel?
    var bubble: BubblePanel?
    var reviewTimer: Timer?
    var positioning = false
    var subscriptions = Set<AnyCancellable>()
    override init() {
        super.init(profile:RamWardenProfile())
        reviewTimer=Timer.scheduledTimer(withTimeInterval:30,repeats:true) { [weak self] _ in
            Task { @MainActor in await self?.model.pollReview() }
        }
        model.$tabReview.sink { [weak self] _ in
            DispatchQueue.main.async { self?.resizeChat() }
        }.store(in:&subscriptions)
        model.$focusProposal.sink { [weak self] _ in
            DispatchQueue.main.async { self?.resizeChat() }
        }.store(in:&subscriptions)
        model.$turns.sink { [weak self] _ in
            DispatchQueue.main.async { self?.resizeChat() }
        }.store(in:&subscriptions)
        NotificationCenter.default.publisher(for:.wardenPetSizeChanged).sink { [weak self] notification in
            guard notification.userInfo?["warden"] as? String == "ram" else { return }
            self?.resizePet()
        }.store(in:&subscriptions)
        voice.onChange = { [weak self] in self?.updateBubble() }
        voice.allowSpeech = { [weak self] in self?.chat?.isVisible != true }
        voice.onFocusRequested = { [weak self] in self?.requestFocus() }
    }
    /// Show, move or hide the speech bubble to match what the warden is saying.
    /// Pets come with Plus and Pro: without one, the ram is put away (the
    /// user's own show/hide choice is kept for when it comes back).
    func requestFocus() {
        model.focusMode = true
        model.turns.append(ChatTurn(role:"assistant",content:"What are you working on? Let’s see which tabs belong in your little kingdom."))
        openChat()
    }
    func updateBubble() {
        if !voice.pet {
            hideBubble()
            if pet != nil { closeChat(); pet?.orderOut(nil); pet?.contentView = nil; pet = nil; model.companionVisible = false }
            return
        }
        if pet == nil && visible { showPet() }
        guard voice.bubble != nil, let pet = pet, chat?.isVisible != true else { hideBubble(); return }
        if bubble == nil {
            let p = BubblePanel(contentRect:NSRect(x:0,y:0,width:272,height:96),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
            p.isOpaque = false; p.backgroundColor = .clear; p.hasShadow = true
            p.level = .floating; p.hidesOnDeactivate = false
            p.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
            p.contentView = NSHostingView(rootView:BubbleView(voice:voice))
            bubble = p
        }
        guard let b = bubble else { return }
        // Rebuild the hosting root before measuring: SwiftUI may otherwise
        // return the previous message's height and clip a longer reply.
        b.contentView = NSHostingView(rootView:BubbleView(voice:voice))
        b.contentView?.layoutSubtreeIfNeeded()
        let size = b.contentView?.fittingSize ?? NSSize(width:272,height:96)
        b.setContentSize(size)
        let screen = NSScreen.screens.first(where: { $0.frame.contains(NSPoint(x:pet.frame.midX,y:pet.frame.midY)) }) ?? NSScreen.main
        let bounds = screen?.visibleFrame ?? pet.frame
        b.setFrame(PetSizing.attachedFrame(pet:pet.frame,size:size,bounds:bounds,preferAbove:true),display:true)
        b.orderFrontRegardless()
    }
    func hideBubble() { bubble?.orderOut(nil) }
    var colors: NSWindow?
    weak var threatCustomizationModel:CompanionModel?
    let settingsSelection=WardenSettingsSelection()
    var visible: Bool { UserDefaults.standard.bool(forKey:"ram.visible") }
    func restore() { if visible && voice.pet { showPet() } }
    func toggle() {
        if pet != nil { CompanionSpeech.shared.stop("ram"); closeChat(); hideBubble(); pet?.orderOut(nil); pet?.contentView = nil; pet = nil; UserDefaults.standard.set(false,forKey:"ram.visible") }
        else { UserDefaults.standard.set(true,forKey:"ram.visible"); showPet() }
        model.companionVisible = pet != nil
    }
    func showPet() {
        guard pet == nil, voice.pet else { return }
        let size = PetSizing.size("ram",scale:PetSizing.scale("ram"))
        let p = PetPanel(contentRect:NSRect(origin:NSPoint(x:100,y:100),size:size),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        p.isOpaque = false; p.backgroundColor = .clear; p.hasShadow = false
        p.level = .floating; p.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]; p.hidesOnDeactivate = false
        let host = PetHost(rootView:PixelRam(model:model))
        host.onClick = { [weak self] in self?.toggleChat() }
        host.onMoved = { [weak self] in self?.positionChat(movePet:true) }
        host.onSettings = { [weak self] in self?.openColors() }
        host.isQuiet = { [weak self] in self?.voice.muted == true || UserDefaults.standard.bool(forKey:"ram.audioMuted") }
        host.onQuiet = { [weak self] in guard let self=self else{return}; if self.voice.muted || UserDefaults.standard.bool(forKey:"ram.audioMuted") {CompanionAudio.setMuted(false,warden:"ram");self.voice.unmute()} else {CompanionAudio.setMuted(true,warden:"ram");self.voice.mute(nil)} }
        host.onFocus = { [weak self] in self?.requestFocus() }
        host.onHide = { [weak self] in self?.toggle() }
        p.delegate = self
        p.contentView = host
        let d = UserDefaults.standard
        let desired = NSPoint(x:d.object(forKey:"ram.x") as? Double ?? 100,y:d.object(forKey:"ram.y") as? Double ?? 100)
        let screen = NSScreen.screens.first(where: { $0.visibleFrame.contains(desired) }) ?? NSScreen.main
        if let frame = screen?.visibleFrame {
            p.setFrame(PetSizing.resizedFrame(NSRect(origin:desired,size:size),size:size,bounds:frame),display:false)
        }
        pet = p; p.orderFrontRegardless()
    }
    func resizePet() {
        guard let pet = pet else { return }
        let screen = NSScreen.screens.first(where: { $0.frame.contains(NSPoint(x:pet.frame.midX,y:pet.frame.midY)) }) ?? NSScreen.main
        let bounds = screen?.visibleFrame ?? pet.frame
        let size = PetSizing.size("ram",scale:PetSizing.scale("ram"))
        positioning = true
        pet.setFrame(PetSizing.resizedFrame(pet.frame,size:size,bounds:bounds),display:true)
        positioning = false
        UserDefaults.standard.set(pet.frame.minX,forKey:"ram.x")
        UserDefaults.standard.set(pet.frame.minY,forKey:"ram.y")
        positionChat(movePet:false)
        updateBubble()
    }
    func toggleChat() {
        if chat?.isVisible == true { closeChat() } else { openChat() }
    }
    func openChat() {
        model.speaking = false
        hideBubble()
        if pet == nil { UserDefaults.standard.set(true,forKey:"ram.visible"); showPet(); model.companionVisible = true }
        if chat == nil {
            let w = ChatPanel(contentRect:NSRect(x:0,y:0,width:304,height:model.chatHeight),styleMask:[.borderless],backing:.buffered,defer:false)
            w.title = "Ram chat"; w.isReleasedWhenClosed = false
            w.isFloatingPanel = true; w.becomesKeyOnlyIfNeeded = false
            w.isOpaque = false; w.backgroundColor = .clear; w.hasShadow = true
            w.level = .floating; w.hidesOnDeactivate = false
            w.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
            w.contentView = NSHostingView(rootView:AssistantView(model:model,dismiss:{ [weak self] in self?.closeChat() },settings:{ [weak self] in self?.openColors() }))
            chat = w
        }
        resizeChat()
        positionChat(movePet:true)
        NSApp.activate(ignoringOtherApps:true)
        chat?.makeKeyAndOrderFront(nil)
        DispatchQueue.main.async { NotificationCenter.default.post(name: Notification.Name("WardenChatOpened"), object: self.model.profile.id) }
    }
    func windowDidMove(_ notification: Notification) {
        guard notification.object as? NSWindow === pet else { return }
        positionChat(movePet:false)
        updateBubble()
    }
    func positionChat(movePet: Bool) {
        guard !positioning, let pet = pet, let chat = chat else { return }
        positioning = true; defer { positioning = false }
        let screen = NSScreen.screens.first(where: { $0.frame.contains(NSPoint(x:pet.frame.midX,y:pet.frame.midY)) }) ?? NSScreen.main
        guard let bounds = screen?.visibleFrame else { return }
        chat.setFrame(PetSizing.attachedFrame(pet:pet.frame,size:chat.frame.size,bounds:bounds,preferAbove:false),display:true)
    }
    func resizeChat() {
        guard let chat = chat else { return }
        guard chat.frame.height != model.chatHeight else{return}
        var frame = chat.frame; frame.size.height = model.chatHeight
        chat.setFrame(frame,display:true)
        positionChat(movePet:true)
    }
    func closeChat() {
        model.cancelConversation();CompanionSpeech.shared.stop("ram")
        chat?.orderOut(nil); colors?.close(); NSColorPanel.shared.orderOut(nil)
        DispatchQueue.main.async { [weak self] in self?.updateBubble() }
    }
    func windowWillClose(_ notification: Notification) {
        if notification.object as? NSWindow === colors {
            NSColorPanel.shared.close()
            colors?.contentView = nil
            colors = nil
        }
    }
    func openColors(warden:String="ram") {
        settingsSelection.selected=warden
        NSColorPanel.shared.close()
        if colors == nil {
            let w = NSWindow(contentRect:NSRect(x:0,y:0,width:334,height:714),styleMask:[.titled,.closable],backing:.buffered,defer:false)
            w.title = "Warden settings"; w.isReleasedWhenClosed = false
            w.level = .floating; w.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
            w.delegate = self
            if let threat=threatCustomizationModel {w.contentView=NSHostingView(rootView:WardenSettingsView(ram:model,threat:threat,selection:settingsSelection))}
            else {w.contentView=NSHostingView(rootView:ColorsView(model:model))}
            w.center(); colors = w
        }
        NSApp.activate(ignoringOtherApps:true); colors?.makeKeyAndOrderFront(nil)
    }
}
