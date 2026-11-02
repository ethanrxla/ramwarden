import AppKit
import SwiftUI

private struct ThreatPixel { let x: Double; let y: Double; let color: Color }
private final class ThreatPixels: NSObject, XMLParserDelegate {
    var pixels: [ThreatPixel] = []
    func parser(_ parser: XMLParser, didStartElement name: String, namespaceURI: String?, qualifiedName: String?, attributes a: [String:String]) {
        guard name == "rect", let x = Double(a["x"] ?? ""), let y = Double(a["y"] ?? ""), let hex = a["fill"], let rgb = UInt32(hex.dropFirst(),radix:16) else { return }
        pixels.append(ThreatPixel(x:x,y:y,color:Color(red:Double((rgb>>16)&255)/255,green:Double((rgb>>8)&255)/255,blue:Double(rgb&255)/255)))
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
    var lastPoll = Date.distantPast
    var busy = false
    @MainActor func poll() async {
        guard !busy, Date().timeIntervalSince(lastPoll) >= 15 else { return }
        busy = true; lastPoll = Date(); defer { busy = false }
        guard let report = await fetchJSON("wardens/threat") as? [String:Any], let status = report["status"] as? [String:Any] else {
            level = "off"; headline = "Protection unverified"
            detail = "The local security report is unavailable. A companion on screen does not mean this Mac is protected."
            engines = []; return
        }
        level = status["level"] as? String ?? "off"
        headline = status["headline"] as? String ?? "Setup needed"
        detail = status["detail"] as? String ?? "Check the security agent and connection."
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
                    context.fill(Path(CGRect(x:pixel.x*scale,y:pixel.y*scale,width:scale,height:scale)),with:.color(pixel.color))
                }
            }.frame(width:144,height:144).accessibilityLabel("ThreatWarden companion. Click for protection status.")
            Text(state.headline).font(.system(size:10,weight:.semibold)).lineLimit(2)
                .foregroundStyle(state.level == "clear" ? Color.green : Color.orange)
                .padding(5).background(.ultraThinMaterial,in:Capsule())
        }.frame(width:160,height:180)
    }
}
private struct ThreatReport: View {
    @ObservedObject var state: ThreatState
    var body: some View {
        VStack(alignment:.leading,spacing:12) {
            Text("ThreatWarden").font(.headline)
            Text(state.headline).fontWeight(.semibold)
            Text(state.detail).font(.system(size:12)).textSelection(.enabled)
            ForEach(state.engines,id:\.self) { Text($0).font(.caption) }
            Text("Status comes from this Mac's daemon. Detection is read-only.").font(.caption).foregroundStyle(.secondary)
        }.padding(20).frame(width:320).modifier(ChatGlass())
    }
}
private final class ThreatHost: NSHostingView<ThreatSprite> {
    var report: (() -> Void)?
    var hide: (() -> Void)?
    var dragged = false
    override func mouseDown(with event: NSEvent) { dragged = false }
    override func mouseDragged(with event: NSEvent) {
        dragged = true; window?.performDrag(with:event)
        if let point = window?.frame.origin { UserDefaults.standard.set(point.x,forKey:"threat.x"); UserDefaults.standard.set(point.y,forKey:"threat.y") }
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
    var report: NSWindow?
    var visible: Bool { UserDefaults.standard.bool(forKey:"threat.visible") }
    func restore() { if visible { show() } }
    func toggle() {
        if visible { UserDefaults.standard.set(false,forKey:"threat.visible"); panel?.orderOut(nil); panel = nil; report?.orderOut(nil) }
        else { UserDefaults.standard.set(true,forKey:"threat.visible"); show() }
    }
    @MainActor func poll() async { if visible { await state.poll() } }
    func show() {
        guard panel == nil else { return }
        let p = PetPanel(contentRect:NSRect(x:300,y:100,width:160,height:180),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        p.isOpaque = false; p.backgroundColor = .clear; p.hasShadow = false; p.level = .floating; p.hidesOnDeactivate = false
        p.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
        let host = ThreatHost(rootView:ThreatSprite(state:state)); host.report = { [weak self] in self?.showReport() }; host.hide = { [weak self] in self?.toggle() }
        p.contentView = host
        let d = UserDefaults.standard
        let bounds = NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1000,height:800)
        let x = d.object(forKey:"threat.x") as? Double ?? bounds.maxX-190
        let y = d.object(forKey:"threat.y") as? Double ?? bounds.minY+100
        p.setFrameOrigin(NSPoint(x:max(bounds.minX,min(x,bounds.maxX-160)),y:max(bounds.minY,min(y,bounds.maxY-180))))
        panel = p; p.orderFrontRegardless(); Task { @MainActor in await state.poll() }
    }
    func showReport() {
        if report == nil {
            let window = NSWindow(contentRect:NSRect(x:0,y:0,width:360,height:260),styleMask:[.titled,.closable],backing:.buffered,defer:false)
            window.title = "ThreatWarden status"; window.isReleasedWhenClosed = false; window.level = .floating
            window.contentView = NSHostingView(rootView:ThreatReport(state:state)); window.center(); report = window
        }
        NSApp.activate(ignoringOtherApps:true); report?.makeKeyAndOrderFront(nil)
    }
}
