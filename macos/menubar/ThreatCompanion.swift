import AppKit
import SwiftUI

private struct ThreatPixel { let x: Double; let y: Double; let width: Double; let height: Double; let hex: String }
private final class ThreatPixels: NSObject, XMLParserDelegate {
    var pixels: [ThreatPixel] = []
    func parser(_ parser: XMLParser, didStartElement name: String, namespaceURI: String?, qualifiedName: String?, attributes a: [String:String]) {
        guard name == "rect",let x=Double(a["x"] ?? ""),let y=Double(a["y"] ?? ""),let hex=a["fill"],UInt32(hex.dropFirst(),radix:16) != nil else{return}
        pixels.append(ThreatPixel(x:x,y:y,width:Double(a["width"] ?? "1") ?? 1,height:Double(a["height"] ?? "1") ?? 1,hex:hex))
    }
    static let sprite: [ThreatPixel] = {
        guard let url = Bundle.main.url(forResource:"ThreatWarden",withExtension:"svg"), let parser = XMLParser(contentsOf:url) else { return [] }
        let delegate = ThreatPixels(); parser.delegate = delegate; parser.shouldResolveExternalEntities = false
        _ = parser.parse(); return delegate.pixels
    }()
}

@MainActor final class ThreatState: ObservableObject {
    @Published var headline = "Checking protection…"
    @Published var detail = "Waiting for the local security report."
    @Published var level = "setup"
    @Published var engines: [String] = []
    @Published var connections: [String] = []
    @Published var networkScope = ""
    @Published var checkedAt: Double = 0
    @Published var alertCount = 0
    @Published var alerts:[ThreatAlert] = []
    var onStatus: ((String)->Void)?
    @Published var pixelScale = PetSizing.scale("threat")
    var lastPoll = Date.distantPast
    var busy = false
    var isVisible: (() -> Bool)?
    @MainActor func poll() async {
        guard !busy, Date().timeIntervalSince(lastPoll) >= 15 else { return }
        busy = true; lastPoll = Date(); defer { busy = false }
        guard let report = await fetchJSON("wardens/threat") as? [String:Any], let status = report["status"] as? [String:Any] else {
            level = "off"; headline = "Protection unverified"
            detail = "The local security report is unavailable. A companion on screen does not mean this Mac is protected."
            engines = []; connections = []; networkScope = ""; return
        }
        let previouslyChecked = checkedAt > 0
        checkedAt = status["checked_at"] as? Double ?? 0
        alerts = (((report["report"] as? [String:Any])?["alerts"] as? [[String:Any]]) ?? []).prefix(50).enumerated().map { ThreatAlert(index:$0.offset,row:$0.element) }
        alertCount = ((report["report"] as? [String:Any])?["alerts"] as? [[String:Any]])?.count ?? 0
        if previouslyChecked,let serious=alerts.first(where:{$0.level >= 12 && $0.isRecent}) {
            GoatAlerts.shared.notify(warden:"threat",event:serious.timestamp+":"+serious.rule,critical:serious.level >= 15)
        }
        let previousHeadline = headline
        level = status["level"] as? String ?? "off"
        headline = status["headline"] as? String ?? "Setup needed"
        detail = status["detail"] as? String ?? "Check the security agent and connection."
        if isVisible?() == true && !headline.localizedCaseInsensitiveContains("Pro warden") && headline != previousHeadline { CompanionSpeech.shared.say(headline+". "+detail,warden:"threat") }
        if headline != previousHeadline { onStatus?(headline) }
        let engineRows = (report["report"] as? [String:Any])?["engines"] as? [[String:Any]] ?? []
        engines = engineRows.map { "\($0["name"] as? String ?? "Engine"): \($0["state"] as? String ?? "unknown")\n\($0["detail"] as? String ?? "")" }
        let network = ((report["report"] as? [String:Any])?["sensors"] as? [String:Any])?["network"] as? [String:Any] ?? [:]
        networkScope = network["problem"] as? String ?? network["scope"] as? String ?? "Network inventory unavailable"
        connections = (network["connections"] as? [[String:Any]] ?? []).prefix(100).enumerated().map { index, row in
            "\(index + 1). \(row["process"] as? String ?? "Unknown") (PID \(row["pid"] as? Int ?? 0)) -> \(row["remote"] as? String ?? "")"
        }
    }
}

struct ThreatAlert: Identifiable {
    let id:String;let timestamp:String;let rule:String;let level:Int;let description:String;let advice:String?;let evidence:[String:String];let eventID:String;let objects:[ThreatEvidence]
    var isRecent:Bool {
        let parser=ISO8601DateFormatter();parser.formatOptions=[.withInternetDateTime,.withFractionalSeconds]
        let date=parser.date(from:timestamp) ?? ISO8601DateFormatter().date(from:timestamp)
        guard let date=date else{return false};let age=Date().timeIntervalSince(date);return age >= -60 && age < 300
    }
    init(index:Int,row:[String:Any]){timestamp=row["timestamp"] as? String ?? "Time unavailable";rule=row["rule_id"] as? String ?? "Unknown";level=row["level"] as? Int ?? 0;description=String((row["description"] as? String ?? "No alert description supplied").prefix(1200));advice=row["advice"] as? String;evidence=row["evidence"] as? [String:String] ?? [:];eventID=row["id"] as? String ?? "";objects=(row["evidence_objects"] as? [[String:Any]] ?? []).map{ThreatEvidence(row:$0)};id="\(timestamp):\(rule):\(index)"}
}
struct ThreatEvidence:Identifiable {
    let id:String;let summary:String;let source:String;let complete:Bool;let details:String
    init(row:[String:Any]){summary=row["summary"] as? String ?? "Evidence";source=row["source"] as? String ?? "Sentinel";complete=row["complete"] as? Bool ?? false;id="\(row["event_id"] as? String ?? ""):\(row["kind"] as? String ?? "")";let data=(try? JSONSerialization.data(withJSONObject:row["data"] ?? [:],options:[.prettyPrinted,.sortedKeys])) ?? Data();details=String(data:data,encoding:.utf8) ?? ""}
}
@MainActor private enum SecuritySourceViewer {
    static var window:NSWindow?
    static func show(_ text:String){
        let win=window ?? NSWindow(contentRect:NSRect(x:0,y:0,width:620,height:560),styleMask:[.titled,.closable,.resizable],backing:.buffered,defer:false)
        win.title="ThreatWarden · Source evidence (read-only)";win.isReleasedWhenClosed=false
        let scroll=NSScrollView(frame:NSRect(x:0,y:0,width:620,height:560));scroll.hasVerticalScroller=true;scroll.hasHorizontalScroller=true;scroll.autoresizingMask=[.width,.height]
        let view=NSTextView(frame:scroll.bounds);view.isEditable=false;view.isSelectable=true;view.isVerticallyResizable=true;view.autoresizingMask=[.width];view.font=NSFont.monospacedSystemFont(ofSize:11,weight:.regular);view.string=text;scroll.documentView=view
        win.contentView=scroll;window=win;win.center();NSApp.activate(ignoringOtherApps:true);win.makeKeyAndOrderFront(nil)
    }
}
struct ThreatAvatar:View {
    @ObservedObject var model:CompanionModel
    private func shade(_ hex:String,_ amount:Double=0)->Color {
        let rgb=UInt32(hex,radix:16) ?? 0
        func channel(_ shift:UInt32)->Double {let value=Double((rgb>>shift)&255)/255;return amount>=0 ? value+(1-value)*amount : value*(1+amount)}
        return Color(red:channel(16),green:channel(8),blue:channel(0))
    }
    private func color(_ pixel:ThreatPixel)->Color {
        if pixel.y>=43 && pixel.hex != "#07050b" {return shade(model.hooves,pixel.hex == "#e3e8ee" ? 0.25 : 0)}
        switch pixel.hex {
        case "#9aa3ad":return shade(model.wool)
        case "#e3e8ee":return shade(model.wool,0.65)
        case "#5b636d":return shade(model.wool,-0.4)
        case "#5b21b6","#7c3aed":return shade(model.horns)
        case "#8b5cf6":return shade(model.horns,0.18)
        case "#ddd6fe","#c4b5fd":return shade(model.horns,0.65)
        case "#3b0f7a","#1a0f2e":return shade(model.horns,-0.5)
        case "#39ff88":return shade(model.eyes)
        case "#d9ffe9","#eafff2","#a7f3c7":return shade(model.eyes,0.65)
        default:return ramColor(String(pixel.hex.dropFirst()))
        }
    }
    var body:some View {
        Canvas {context,size in
            let scale=floor(min(size.width,size.height)/48);let x=floor((size.width-48*scale)/2);let y=floor((size.height-48*scale)/2)
            for pixel in ThreatPixels.sprite {context.fill(Path(CGRect(x:x+pixel.x*scale,y:y+pixel.y*scale,width:pixel.width*scale,height:pixel.height*scale)),with:.color(color(pixel)))}
        }.accessibilityLabel("ThreatWarden cyborg companion")
    }
}
private struct ThreatSprite:View {
    @ObservedObject var state:ThreatState
    @ObservedObject var model:CompanionModel
    var body:some View {ThreatAvatar(model:model).frame(width:CGFloat(48*state.pixelScale),height:CGFloat(48*state.pixelScale)).frame(width:PetSizing.size("threat",scale:state.pixelScale).width,height:PetSizing.size("threat",scale:state.pixelScale).height).accessibilityLabel("ThreatWarden companion. Click to toggle chat.")}
}
private struct ThreatReport: View {
    @ObservedObject private var speech=CompanionSpeech.shared
    @AppStorage("threat.speech") private var spoken = false
    @ObservedObject var state: ThreatState
    @ObservedObject var model:CompanionModel
    var hide: () -> Void
    var chat: () -> Void
    var forget: () -> Void
    @State private var sourceBusy=false
    @State private var sourceError:String?
    func readSource(_ alert:ThreatAlert){
        guard !sourceBusy else{return};sourceBusy=true;sourceError=nil
        Task { @MainActor in
            defer{sourceBusy=false}
            do{
                var request=URLRequest(url:base.appendingPathComponent("wardens/threat/events").appendingPathComponent(alert.eventID));request.timeoutInterval=12
                let (data,response)=try await URLSession.shared.data(for:request)
                guard (response as? HTTPURLResponse)?.statusCode == 200,data.count<=256*1024 else{sourceError="Source evidence is unavailable or access changed.";return}
                let object=try JSONSerialization.jsonObject(with:data);let pretty=try JSONSerialization.data(withJSONObject:object,options:[.prettyPrinted,.sortedKeys]);SecuritySourceViewer.show(String(data:pretty,encoding:.utf8) ?? "")
            }catch{sourceError="Could not retrieve this source event."}
        }
    }
    var body: some View {
        VStack(alignment:.leading,spacing:8) {
            HStack {ThreatAvatar(model:model).frame(width:48,height:48);Text(model.name).font(.headline)}
            DisclosureGroup("Companion settings") { GoatAlertControl(warden:"threat"); Button("Forget local chat history",action:forget); Toggle("Spoken status",isOn:$spoken).onChange(of:spoken) { if !$0 { CompanionSpeech.shared.stop("threat") } }; PetSizeControl(warden:"threat",label:"Companion size"); if let error=speech.errors["threat"] {Text(error).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal:false,vertical:true)} }

            Text(state.headline).fontWeight(.semibold)
            Text(state.detail).font(.system(size:12)).textSelection(.enabled).fixedSize(horizontal:false,vertical:true)
            HStack {
                Text(state.checkedAt > 0 ? "Reported alerts: \(state.alertCount)" : "Alerts unverified")
                Spacer()
                Text(state.checkedAt > 0 ? Date(timeIntervalSince1970:state.checkedAt).formatted(date:.omitted,time:.shortened) : "Not checked")
            }.font(.caption).foregroundStyle(.secondary)
            LazyVGrid(columns:[GridItem(.flexible()),GridItem(.flexible())],alignment:.leading,spacing:4) { ForEach(state.engines,id:\.self) { Text($0).font(.caption) } }
            DisclosureGroup("Connections on this Mac (first 100)") {
                Text(state.networkScope).font(.caption2).foregroundStyle(.secondary)
                ScrollView { LazyVStack(alignment:.leading) { ForEach(state.connections,id:\.self) { Text($0).font(.caption2).textSelection(.enabled) } } }.frame(maxHeight:150)
            }
            if !state.alerts.isEmpty {
                Text("Recent rule alerts · not confirmed malware").font(.caption).foregroundStyle(.secondary)
                ScrollView {
                    LazyVStack(alignment:.leading,spacing:10) {
                        ForEach(state.alerts) { alert in
                            VStack(alignment:.leading,spacing:4) {
                                HStack {Text("Level \(alert.level) · Rule \(alert.rule)").fontWeight(.semibold);Spacer()}.foregroundStyle(alert.level >= 12 ? Color.red : Color.orange)
                                Text(alert.description).fixedSize(horizontal:false,vertical:true)
                                ForEach(alert.evidence.keys.sorted(),id:\.self) { key in Text("\(key): \(alert.evidence[key] ?? "")").font(.caption2).fixedSize(horizontal:false,vertical:true) }
                                ForEach(alert.objects){object in
                                    Text(object.summary).fontWeight(.medium)
                                    Text(object.source + (object.complete ? "" : " · incomplete")).font(.caption2).foregroundStyle(.secondary)
                                    DisclosureGroup("Evidence objects"){Text(object.details).font(.system(size:10,design:.monospaced)).textSelection(.enabled)}
                                }
                                if !alert.eventID.isEmpty {Button("Read source event"){readSource(alert)}.disabled(sourceBusy)}
                                Text(alert.timestamp).font(.caption2).foregroundStyle(.secondary)
                                if let advice=alert.advice {Text(advice).font(.caption)}
                            }.font(.caption).textSelection(.enabled).padding(9).frame(maxWidth:.infinity,alignment:.leading).background(.quaternary,in:RoundedRectangle(cornerRadius:8))
                        }
                    }
                }.frame(height:210)
            }

            if let error=sourceError {Text(error).font(.caption).foregroundStyle(.red)}
            Button("Ask ThreatWarden",action:chat)
            HStack {
                Button("Hide companion", action:hide)
                Spacer()
                Button(state.level == "locked" ? "Get Pro…" : "Manage account…") {
                    let site = UserDefaults.standard.string(forKey:"website.url") ?? "https://ramwarden.com"
                    if let url = URL(string:site + "/account.html?plan=2") { NSWorkspace.shared.open(url) }
                }
            }
            Text(state.level == "locked" ? "Upgrade opens checkout for your approval." : "Your account controls your plan and connected computers.").font(.caption).foregroundStyle(.secondary)
            Text("Status comes from this Mac's daemon. Detection is read-only.").font(.caption).foregroundStyle(.secondary)
        }.padding(14).frame(width:310).modifier(ChatGlass())
    }
}
private final class ThreatHost: NSHostingView<ThreatSprite> {
    var settings: (() -> Void)?
    var report: (() -> Void)?
    var chat: (() -> Void)?
    var moved: (() -> Void)?
    var hide: (() -> Void)?
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
        moved?()
        UserDefaults.standard.set(window.frame.minX,forKey:"threat.x")
        UserDefaults.standard.set(window.frame.minY,forKey:"threat.y")
    }
    override func mouseUp(with event: NSEvent) { if !dragged { chat?() } }
    override func rightMouseDown(with event: NSEvent) {
        let menu = NSMenu(); menu.autoenablesItems = false
        let show = NSMenuItem(title:"Protection status",action:#selector(showStatus),keyEquivalent:""); show.target = self; menu.addItem(show)
        let talk = NSMenuItem(title:"Open chat",action:#selector(openAssistant),keyEquivalent:""); talk.target = self; menu.addItem(talk)
        let preferences = NSMenuItem(title:"Settings…",action:#selector(showSettings),keyEquivalent:""); preferences.target=self; menu.addItem(preferences)
        let quiet = NSMenuItem(title:CompanionAudio.isMuted("threat") ? "Unmute companion" : "Mute companion",action:#selector(toggleQuiet),keyEquivalent:""); quiet.target=self; menu.addItem(quiet)
        let close = NSMenuItem(title:"Hide threat companion",action:#selector(hidePet),keyEquivalent:""); close.target = self; menu.addItem(close)
        NSMenu.popUpContextMenu(menu,with:event,for:self)
    }
    @objc func showSettings() { settings?() }
    @objc func toggleQuiet() { CompanionAudio.setMuted(!CompanionAudio.isMuted("threat"),warden:"threat") }
    @objc func openAssistant() { chat?() }
    @objc func showStatus() { report?() }
    @objc func hidePet() { hide?() }
}

@MainActor final class ThreatCompanion: WardenCompanion {
    let state = ThreatState()
    var panel: NSPanel?
    var report: NSPopover?
    weak var statusButton: NSStatusBarButton?
    var openCustomization:(()->Void)?
    private var sizeObserver: NSObjectProtocol?
    override init() {
        super.init(profile:ThreatWardenProfile())
        CompanionSpeech.shared.trackVisibility("threat") { [weak self] in
            guard let panel=self?.panel else{return false}
            return panel.isVisible && panel.occlusionState.contains(.visible)
        }
        state.onStatus = { [weak self] text in guard let self=self,self.visible,!self.model.thinking,self.assistantPanel?.isVisible != true else{return};self.recordStatus(text);self.showStatusBubble(text,anchor:self.panel,open:{[weak self] in self?.openChat()}) }
        sizeObserver = NotificationCenter.default.addObserver(forName:.wardenPetSizeChanged,object:nil,queue:.main) { [weak self] notification in
            guard notification.userInfo?["warden"] as? String == "threat" else { return }
            Task { @MainActor in self?.resizePet() }
        }
    }
    deinit { if let observer = sizeObserver { NotificationCenter.default.removeObserver(observer) } }
    var visible: Bool { UserDefaults.standard.bool(forKey:"threat.visible") }
    func restore() { state.isVisible = { [weak self] in self?.visible == true }; if visible { show() } }
    func hide() {
        UserDefaults.standard.set(false,forKey:"threat.visible")
        CompanionSpeech.shared.stop("threat")
        hideSharedPanels()
        SecuritySourceViewer.window?.close()
        panel?.orderOut(nil); panel = nil; report?.performClose(nil)
    }
    func toggle() {
        if visible { hide() }
        else { UserDefaults.standard.set(true,forKey:"threat.visible"); show() }
    }
    @MainActor func poll() async { await state.poll() }
    func show() {
        guard panel == nil else { return }
        state.pixelScale = PetSizing.scale("threat")
        let size = PetSizing.size("threat",scale:state.pixelScale)
        let p = PetPanel(contentRect:NSRect(origin:NSPoint(x:300,y:100),size:size),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        p.isOpaque = false; p.backgroundColor = .clear; p.hasShadow = false; p.level = .floating; p.hidesOnDeactivate = false
        p.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
        let host = ThreatHost(rootView:ThreatSprite(state:state,model:model)); host.report = { [weak self] in self?.showReport() }; host.chat = { [weak self] in self?.toggleChat() }; host.moved = { [weak self] in self?.didMove(anchor:self?.panel) }; host.hide = { [weak self] in self?.hide() }
        host.settings = { [weak self] in self?.openCustomization?() }
        p.contentView = host
        let d = UserDefaults.standard
        let bounds = NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1000,height:800)
        let x = d.object(forKey:"threat.x") as? Double ?? bounds.maxX-190
        let y = d.object(forKey:"threat.y") as? Double ?? bounds.minY+100
        p.setFrame(PetSizing.resizedFrame(NSRect(origin:NSPoint(x:x,y:y),size:size),size:size,bounds:bounds),display:false)
        panel = p; model.companionVisible=true;p.orderFrontRegardless(); if state.checkedAt>0 {showStatusBubble(state.headline,anchor:p,open:{[weak self] in self?.openChat()})}; Task { @MainActor in await state.poll() }
    }
    func resizePet() {
        state.pixelScale = PetSizing.scale("threat")
        guard let panel = panel else { return }
        let screen = NSScreen.screens.first(where: { $0.frame.contains(NSPoint(x:panel.frame.midX,y:panel.frame.midY)) }) ?? NSScreen.main
        let bounds = screen?.visibleFrame ?? panel.frame
        panel.setFrame(PetSizing.resizedFrame(panel.frame,size:PetSizing.size("threat",scale:state.pixelScale),bounds:bounds),display:true)
        UserDefaults.standard.set(panel.frame.minX,forKey:"threat.x")
        UserDefaults.standard.set(panel.frame.minY,forKey:"threat.y")
    }
    private func recordStatus(_ text:String) {
        guard model.turns.last?.content != text else{return}
        model.turns.append(ChatTurn(role:"assistant",content:text))
        if model.turns.count>200 {model.turns.removeFirst(model.turns.count-200)}
    }
    func toggleChat(){if assistantPanel?.isVisible == true {closeAssistant()} else {openChat()}}
    func openChat() { if model.turns.isEmpty {recordStatus(state.headline)}; model.connected = state.level != "off";showAssistant(anchor:panel,settings:{[weak self] in self?.openCustomization?()}) }
    func showReport() {
        guard let button = statusButton else { return }
        if report?.isShown == true { report?.performClose(nil); return }
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentViewController = NSHostingController(rootView:ThreatReport(state:state,model:model,hide:{ [weak self] in self?.hide(); self?.report?.performClose(nil) },chat:{[weak self] in self?.report?.performClose(nil);self?.openChat()},forget:{[weak self] in self?.model.forgetHistory()}))
        popover.contentSize = NSSize(width:340,height:min(state.alerts.isEmpty ? 430 : 680,(button.window?.screen?.visibleFrame.height ?? 800)-70))
        report = popover
        popover.show(relativeTo:button.bounds,of:button,preferredEdge:.minY)
        Task { @MainActor in await state.poll() }
    }
}
