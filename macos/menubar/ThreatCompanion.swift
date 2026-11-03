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
    @Published var checkedAt: Double = 0
    @Published var alertCount = 0
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
            engines = []; return
        }
        checkedAt = status["checked_at"] as? Double ?? 0
        alertCount = ((report["report"] as? [String:Any])?["alerts"] as? [[String:Any]])?.count ?? 0
        let previousHeadline = headline
        level = status["level"] as? String ?? "off"
        headline = status["headline"] as? String ?? "Setup needed"
        detail = status["detail"] as? String ?? "Check the security agent and connection."
        if isVisible?() == true && !headline.localizedCaseInsensitiveContains("Pro warden") && headline != previousHeadline { CompanionSpeech.shared.say(headline+". "+detail,warden:"threat") }
        let engineRows = (report["report"] as? [String:Any])?["engines"] as? [[String:Any]] ?? []
        engines = engineRows.map { "\($0["name"] as? String ?? "Engine"): \($0["state"] as? String ?? "unknown")" }
    }
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
            Text(state.headline).font(.system(size:10,weight:.semibold)).lineLimit(2)
                .foregroundStyle(state.level == "clear" ? Color.green : Color.orange)
                .padding(5).background(.ultraThinMaterial,in:Capsule())
        }.frame(width:PetSizing.size("threat",scale:state.pixelScale).width,height:PetSizing.size("threat",scale:state.pixelScale).height)
    }
}
private struct ThreatReport: View {
    @AppStorage("threat.speech") private var spoken = false
    @ObservedObject var state: ThreatState
    var hide: () -> Void
    var body: some View {
        VStack(alignment:.leading,spacing:8) {
            Text("ThreatWarden").font(.headline)
            Toggle("Spoken status",isOn:$spoken).onChange(of:spoken) { if !$0 { CompanionSpeech.shared.stop("threat") } }

            Text(state.headline).fontWeight(.semibold)
            Text(state.detail).font(.system(size:12)).textSelection(.enabled)
            HStack {
                Text(state.checkedAt > 0 ? "Reported alerts: \(state.alertCount)" : "Alerts unverified")
                Spacer()
                Text(state.checkedAt > 0 ? Date(timeIntervalSince1970:state.checkedAt).formatted(date:.omitted,time:.shortened) : "Not checked")
            }.font(.caption).foregroundStyle(.secondary)
            ForEach(state.engines,id:\.self) { Text($0).font(.caption) }
            HStack {
                Button("Hide companion", action:hide)
                Spacer()
                Button(state.level == "locked" ? "Get Pro…" : "Manage account…") {
                    let site = UserDefaults.standard.string(forKey:"website.url") ?? "https://ramwarden.com"
                    if let url = URL(string:site + "/account.html?plan=2") { NSWorkspace.shared.open(url) }
                }
            }
            Text("Upgrade opens checkout. Your card or wallet approval happens there.").font(.caption).foregroundStyle(.secondary)
            Text("Status comes from this Mac's daemon. Detection is read-only.").font(.caption).foregroundStyle(.secondary)
        }.padding(14).frame(width:310).modifier(ChatGlass())
    }
}
private final class ThreatHost: NSHostingView<ThreatSprite> {
    var report: (() -> Void)?
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
        UserDefaults.standard.set(window.frame.minX,forKey:"threat.x")
        UserDefaults.standard.set(window.frame.minY,forKey:"threat.y")
    }
    override func mouseUp(with event: NSEvent) { if !dragged { report?() } }
    override func rightMouseDown(with event: NSEvent) {
        let menu = NSMenu(); menu.autoenablesItems = false
        let show = NSMenuItem(title:"Protection status",action:#selector(showStatus),keyEquivalent:""); show.target = self; menu.addItem(show)
        let close = NSMenuItem(title:"Hide threat companion",action:#selector(hidePet),keyEquivalent:""); close.target = self; menu.addItem(close)
        NSMenu.popUpContextMenu(menu,with:event,for:self)
    }
    @objc func showStatus() { report?() }
    @objc func hidePet() { hide?() }
}

final class ThreatCompanion {
    let state = ThreatState()
    var panel: NSPanel?
    var report: NSPopover?
    weak var statusButton: NSStatusBarButton?
    private var sizeObserver: NSObjectProtocol?
    init() {
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
        let host = ThreatHost(rootView:ThreatSprite(state:state)); host.report = { [weak self] in self?.showReport() }; host.hide = { [weak self] in self?.hide() }
        p.contentView = host
        let d = UserDefaults.standard
        let bounds = NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1000,height:800)
        let x = d.object(forKey:"threat.x") as? Double ?? bounds.maxX-190
        let y = d.object(forKey:"threat.y") as? Double ?? bounds.minY+100
        p.setFrame(PetSizing.resizedFrame(NSRect(origin:NSPoint(x:x,y:y),size:size),size:size,bounds:bounds),display:false)
        panel = p; p.orderFrontRegardless(); Task { @MainActor in await state.poll() }
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
    func showReport() {
        guard let button = statusButton else { return }
        if report?.isShown == true { report?.performClose(nil); return }
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentViewController = NSHostingController(rootView:ThreatReport(state:state,hide:{ [weak self] in self?.hide(); self?.report?.performClose(nil) }))
        popover.contentSize = NSSize(width:340,height:280)
        report = popover
        popover.show(relativeTo:button.bounds,of:button,preferredEdge:.minY)
        Task { @MainActor in await state.poll() }
    }
}
