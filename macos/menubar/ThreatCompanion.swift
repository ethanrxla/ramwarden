import AppKit
import SwiftUI

private struct ThreatPixel { let x: Double; let y: Double; let width: Double; let height: Double; let color: Color }
private final class ThreatPixels: NSObject, XMLParserDelegate {
    var pixels: [ThreatPixel] = []
    func parser(_ parser: XMLParser, didStartElement name: String, namespaceURI: String?, qualifiedName: String?, attributes a: [String:String]) {
        guard name == "rect", let x = Double(a["x"] ?? ""), let y = Double(a["y"] ?? ""), let hex = a["fill"], let rgb = UInt32(hex.dropFirst(),radix:16) else { return }
        pixels.append(ThreatPixel(x:x,y:y,width:Double(a["width"] ?? "1") ?? 1,height:Double(a["height"] ?? "1") ?? 1,color:Color(red:Double((rgb>>16)&255)/255,green:Double((rgb>>8)&255)/255,blue:Double(rgb&255)/255)))
    }
    static let sprite: [ThreatPixel] = {
        guard let url = Bundle.main.url(forResource:"ThreatWarden",withExtension:"svg"), let parser = XMLParser(contentsOf:url) else { return [] }
        let delegate = ThreatPixels(); parser.delegate = delegate; parser.shouldResolveExternalEntities = false
        _ = parser.parse(); return delegate.pixels
    }()
}

final class ThreatState: ObservableObject {
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
        checkedAt = status["checked_at"] as? Double ?? 0
        alerts = (((report["report"] as? [String:Any])?["alerts"] as? [[String:Any]]) ?? []).prefix(50).enumerated().map { ThreatAlert(index:$0.offset,row:$0.element) }
        alertCount = ((report["report"] as? [String:Any])?["alerts"] as? [[String:Any]])?.count ?? 0
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
    let id:String;let timestamp:String;let rule:String;let level:Int;let description:String;let advice:String?;let evidence:[String:String]
    init(index:Int,row:[String:Any]){timestamp=row["timestamp"] as? String ?? "Time unavailable";rule=row["rule_id"] as? String ?? "Unknown";level=row["level"] as? Int ?? 0;description=String((row["description"] as? String ?? "No alert description supplied").prefix(1200));advice=row["advice"] as? String;evidence=row["evidence"] as? [String:String] ?? [:];id="\(timestamp):\(rule):\(index)"}
}
private struct ThreatSprite: View {
    @ObservedObject var state: ThreatState
    var body: some View {
        VStack(spacing:2) {
            Canvas { context,size in
                let scale = floor(min(size.width,size.height)/48)
                for pixel in ThreatPixels.sprite {
                    context.fill(Path(CGRect(x:pixel.x*scale,y:pixel.y*scale,width:pixel.width*scale,height:pixel.height*scale)),with:.color(pixel.color))
                }
            }.frame(width:CGFloat(48*state.pixelScale),height:CGFloat(48*state.pixelScale)).accessibilityLabel("ThreatWarden companion. Click for protection status.")

        }.frame(width:PetSizing.size("threat",scale:state.pixelScale).width,height:PetSizing.size("threat",scale:state.pixelScale).height)
    }
}
private struct ThreatReport: View {
    @ObservedObject private var speech=CompanionSpeech.shared
    @AppStorage("threat.speech") private var spoken = false
    @ObservedObject var state: ThreatState
    var hide: () -> Void
    var chat: () -> Void
    var body: some View {
        VStack(alignment:.leading,spacing:8) {
            Text("ThreatWarden").font(.headline)
            DisclosureGroup("Companion settings") { Toggle("Spoken status",isOn:$spoken).onChange(of:spoken) { if !$0 { CompanionSpeech.shared.stop("threat") } }; PetSizeControl(warden:"threat",label:"Companion size"); if let error=speech.errors["threat"] {Text(error).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal:false,vertical:true)} }

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
                                Text(alert.timestamp).font(.caption2).foregroundStyle(.secondary)
                                if let advice=alert.advice {Text(advice).font(.caption)}
                            }.font(.caption).textSelection(.enabled).padding(9).frame(maxWidth:.infinity,alignment:.leading).background(.quaternary,in:RoundedRectangle(cornerRadius:8))
                        }
                    }
                }.frame(height:210)
            }

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
        let close = NSMenuItem(title:"Hide threat companion",action:#selector(hidePet),keyEquivalent:""); close.target = self; menu.addItem(close)
        NSMenu.popUpContextMenu(menu,with:event,for:self)
    }
    @objc func openAssistant() { chat?() }
    @objc func showStatus() { report?() }
    @objc func hidePet() { hide?() }
}

final class ThreatCompanion: WardenCompanion {
    let state = ThreatState()
    var panel: NSPanel?
    var report: NSPopover?
    weak var statusButton: NSStatusBarButton?
    private var sizeObserver: NSObjectProtocol?
    override init() {
        super.init(profile:ThreatWardenProfile())
        state.onStatus = { [weak self] text in guard let self=self,self.visible else{return};self.showStatusBubble(text,anchor:self.panel,open:{[weak self] in self?.openChat()}) }
        sizeObserver = NotificationCenter.default.addObserver(forName:.wardenPetSizeChanged,object:nil,queue:.main) { [weak self] notification in
            guard notification.userInfo?["warden"] as? String == "threat" else { return }
            self?.resizePet()
        }
    }
    deinit { if let observer = sizeObserver { NotificationCenter.default.removeObserver(observer) } }
    var visible: Bool { UserDefaults.standard.bool(forKey:"threat.visible") }
    func restore() { state.isVisible = { [weak self] in self?.visible == true }; if visible { show() } }
    func hide() {
        UserDefaults.standard.set(false,forKey:"threat.visible")
        CompanionSpeech.shared.stop("threat")
        hideSharedPanels()
        panel?.orderOut(nil); panel = nil; report?.performClose(nil)
    }
    func toggle() {
        if visible { hide() }
        else { UserDefaults.standard.set(true,forKey:"threat.visible"); show() }
    }
    @MainActor func poll() async { if visible { await state.poll() } }
    func show() {
        guard panel == nil else { return }
        state.pixelScale = PetSizing.scale("threat")
        let size = PetSizing.size("threat",scale:state.pixelScale)
        let p = PetPanel(contentRect:NSRect(origin:NSPoint(x:300,y:100),size:size),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        p.isOpaque = false; p.backgroundColor = .clear; p.hasShadow = false; p.level = .floating; p.hidesOnDeactivate = false
        p.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
        let host = ThreatHost(rootView:ThreatSprite(state:state)); host.report = { [weak self] in self?.showReport() }; host.chat = { [weak self] in self?.openChat() }; host.moved = { [weak self] in self?.didMove(anchor:self?.panel) }; host.hide = { [weak self] in self?.hide() }
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
    func openChat() { model.connected = state.level != "off";showAssistant(anchor:panel,settings:{[weak self] in self?.showReport()}) }
    func showReport() {
        guard let button = statusButton else { return }
        if report?.isShown == true { report?.performClose(nil); return }
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentViewController = NSHostingController(rootView:ThreatReport(state:state,hide:{ [weak self] in self?.hide(); self?.report?.performClose(nil) },chat:{[weak self] in self?.report?.performClose(nil);self?.openChat()}))
        popover.contentSize = NSSize(width:340,height:min(state.alerts.isEmpty ? 430 : 680,(button.window?.screen?.visibleFrame.height ?? 800)-70))
        report = popover
        popover.show(relativeTo:button.bounds,of:button,preferredEdge:.minY)
        Task { @MainActor in await state.poll() }
    }
}
