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

final class CompanionModel: ObservableObject {
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
    @Published var thinking = false
    @Published var speaking = false
    @Published var turns: [ChatTurn] = []
    @Published var error: String?
    @Published var tier = ""
    @Published var proposedToken: String?
    @Published var proposedLabel = ""
    @Published var wool = UserDefaults.standard.string(forKey: "ram.wool") ?? "D84545"
    @Published var horns = UserDefaults.standard.string(forKey: "ram.horns") ?? "F5DEAA"
    @Published var hooves = UserDefaults.standard.string(forKey: "ram.hooves") ?? "29242B"
    @Published var eyes = UserDefaults.standard.string(forKey: "ram.eyes") ?? "FFFFFF"
    @Published var pupilStyle = UserDefaults.standard.string(forKey: "ram.pupilStyle") ?? "slit"
    private var task: Task<Void, Never>?
    func saveColors() {
        for (key, value) in [("wool", wool), ("horns", horns), ("hooves", hooves), ("eyes", eyes), ("pupilStyle", pupilStyle)] {
            UserDefaults.standard.set(value, forKey: "ram.\(key)")
        }
    }
    func send(_ message: String) {
        let message = boundedChatText(message.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !message.isEmpty, !thinking else { return }
        let history = turns.suffix(4).map { ["role": $0.role, "content": boundedChatText($0.content)] }
        turns.append(ChatTurn(role: "user", content: message))
        error = nil; thinking = true; speaking = false; proposedToken = nil
        task = Task { @MainActor in
            defer { thinking = false; task = nil }
            do {
                var req = URLRequest(url: base.appendingPathComponent("assistant/chat"))
                req.httpMethod = "POST"; req.timeoutInterval = 48
                req.setValue("application/json", forHTTPHeaderField: "Content-Type")
                req.httpBody = try JSONSerialization.data(withJSONObject: ["message":message,"history":history])
                let (data, response) = try await URLSession.shared.data(for: req)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard status == 200 else {
                    error = status == 429 ? "The assistant is busy. Try again shortly." : "Assistant unavailable (\(status)). Check that the daemon is updated and running."
                    return
                }
                let obj = try JSONSerialization.jsonObject(with: data) as? [String:Any]
                guard let reply = obj?["reply"] as? String else { error = "The assistant returned an unreadable response."; return }
                tier = obj?["tier"] as? String ?? "facts"
                if let proposal = obj?["proposed_action"] as? [String:Any], let token = proposal["token"] as? String, let label = proposal["label"] as? String {
                    proposedToken = token; proposedLabel = label
                }
                turns.append(ChatTurn(role:"assistant",content:reply))
                if turns.count > 20 { turns.removeFirst(turns.count - 20) }
                speaking = true
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 3_000_000_000)
                    self.speaking = false
                }
            } catch { self.error = "Cannot reach the assistant. RamWarden will reconnect automatically." }
        }
    }
    func confirmAction() {
        guard let token = proposedToken, !thinking else { return }
        let alert = NSAlert()
        alert.messageText = proposedLabel
        alert.informativeText = "Apply the change described above? Nothing else will be changed."
        alert.addButton(withTitle:"Apply fix"); alert.addButton(withTitle:"Cancel")
        NSApp.activate(ignoringOtherApps:true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        proposedToken = nil; thinking = true
        Task { @MainActor in
            defer { thinking = false }
            do {
                var request = URLRequest(url:base.appendingPathComponent("assistant/action"))
                request.httpMethod = "POST"; request.timeoutInterval = 20
                request.setValue("application/json",forHTTPHeaderField:"Content-Type")
                request.httpBody = try JSONSerialization.data(withJSONObject:["token":token])
                let (data,response) = try await URLSession.shared.data(for:request)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else { error = "That proposal expired or the computer changed. Ask me to check again."; return }
                let result = try JSONSerialization.jsonObject(with:data) as? [String:Any]
                turns.append(ChatTurn(role:"assistant",content:result?["reply"] as? String ?? "The action finished without a report."))
            } catch { self.error = "I lost contact with the daemon. I won’t claim the fix succeeded." }
        }
    }

}
struct ChatTurn: Identifiable { let id = UUID(); let role: String; let content: String }
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
                for y in 0..<48 { for x in 0..<48 {
                    let key=cells[y*48+x]
                    if key != "." {
                        context.fill(Path(CGRect(x:originX+Double(x)*cell,y:Double(y)*cell+bob,width:cell,height:cell)),with:.color(palette(key).opacity(model.connected ? 1 : 0.60)))
                    }
                } }
                let label = !model.connected ? "Reconnecting…" : model.thinking ? "Thinking…" : "Warden"
                context.draw(Text(label).font(.system(size:10,weight:.medium)).foregroundColor(.primary),at:CGPoint(x:size.width/2,y:size.height-6))
            }
        }.overlay(alignment:.top) {
            if model.tabReview != nil {
                Text("Keep these tabs or close some? Click to review.")
                    .font(.system(size:11)).multilineTextAlignment(.center).padding(6)
                    .background(.regularMaterial,in:RoundedRectangle(cornerRadius:10))
                    .offset(y:-12).allowsHitTesting(false)
            }
        }.accessibilityLabel(model.connected ? "RamWarden ram companion. Click to open assistant." : "RamWarden companion. Reconnecting to daemon.")
    }
}

// Native clear Liquid Glass follows system contrast/transparency preferences.
struct ChatGlass: ViewModifier {
    @ViewBuilder func body(content: Content) -> some View {
        if #available(macOS 26.0, *) {
            content.glassEffect(.clear, in: RoundedRectangle(cornerRadius:20))
        } else {
            content.background(.ultraThinMaterial,in:RoundedRectangle(cornerRadius:20))
        }
    }
}

struct AssistantView: View {
    @ObservedObject var model: CompanionModel
    var dismiss: () -> Void
    var settings: () -> Void
    @State private var message = ""
    var body: some View {
        VStack(alignment:.leading,spacing:8) {
            HStack(spacing:8) {
                Circle().fill(model.connected ? Color.green : Color.orange).frame(width:6,height:6)
                Text("Warden").font(.system(size:12,weight:.semibold))
                Spacer()
                Button(action:settings) { Image(systemName:"gearshape").font(.system(size:11)) }.buttonStyle(.plain).accessibilityLabel("Settings")
                Button(action:dismiss) { Image(systemName:"xmark").font(.system(size:10,weight:.semibold)) }
                    .buttonStyle(.plain).accessibilityLabel("Close chat")
            }
            if !model.turns.isEmpty {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment:.leading,spacing:10) {
                        if model.turns.isEmpty { Text("Ask me what’s using RAM or what I’ve been doing.").foregroundStyle(.secondary) }
                        ForEach(model.turns) { turn in
                            VStack(alignment:.leading,spacing:4) {
                                Text(turn.role == "user" ? "You" : "Warden").font(.system(size:10,weight:.semibold)).foregroundStyle(.secondary)
                                Text(turn.content).font(.system(size:12)).textSelection(.enabled)
                            }.frame(maxWidth:.infinity,alignment:.leading).id(turn.id)
                        }
                    }.padding(8)
                }.onChange(of:model.turns.count) { _ in if let last = model.turns.last { proxy.scrollTo(last.id,anchor:.bottom) } }
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
                }.font(.caption).disabled(model.reviewBusy)
            }
            if model.thinking { ProgressView("Thinking…").controlSize(.small) }
            if let error = model.error { Text(error).foregroundStyle(.red).font(.caption) }
            if model.proposedToken != nil {
                Button("Review fix…") { model.confirmAction() }.font(.system(size:12,weight:.semibold))
                    .help(model.proposedLabel)
            }
            HStack {
                TextField("Ask your ram…",text:$message).textFieldStyle(.plain).font(.system(size:12)).onSubmit { send() }
                Button(action:send) { Image(systemName:"arrow.up.circle.fill").font(.system(size:19)) }.buttonStyle(.plain).accessibilityLabel("Send message").disabled(model.thinking || message.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty)
            }
        }.padding(12).frame(width:288,height:model.turns.isEmpty ? 64 : 224)
            .modifier(ChatGlass())
            .padding(8)
            .onExitCommand(perform:dismiss)
    }
    func send() { model.send(message); message = "" }
}

struct ColorsView: View {
    @ObservedObject var model: CompanionModel
    func binding(_ key: ReferenceWritableKeyPath<CompanionModel,String>) -> Binding<Color> {
        Binding(get: { ramColor(model[keyPath:key]) }, set: { color in
            guard let rgb = NSColor(color).usingColorSpace(.deviceRGB) else { return }
            model[keyPath:key] = String(format:"%02X%02X%02X",Int(rgb.redComponent*255),Int(rgb.greenComponent*255),Int(rgb.blueComponent*255))
            model.saveColors()
        })
    }
    var body: some View {
        VStack(spacing:18) {
            Text("Customize your ram").font(.title2.bold())
            PixelRam(model:model,animated:false).frame(width:180,height:180)
            ColorPicker("Wool",selection:binding(\.wool),supportsOpacity:false)
            ColorPicker("Horns",selection:binding(\.horns),supportsOpacity:false)
            ColorPicker("Hooves",selection:binding(\.hooves),supportsOpacity:false)
            ColorPicker("Eyes",selection:binding(\.eyes),supportsOpacity:false)
            Picker("Pupils",selection:Binding(get:{model.pupilStyle},set:{model.pupilStyle = $0; model.saveColors()})) {
                Text("Ram slits").tag("slit")
                Text("Round").tag("round")
                Text("Glowing / none").tag("none")
            }
            Button("Reset eyes") { model.eyes = "FFFFFF"; model.pupilStyle = "slit"; model.saveColors() }
        }.padding(24).frame(width:260)
    }
}

final class PetPanel: NSPanel { override var canBecomeKey: Bool { false } }
final class ChatPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}
final class PetHost: NSHostingView<PixelRam> {
    var onClick: (() -> Void)?
    var onMoved: (() -> Void)?
    var onSettings: (() -> Void)?
    var onHide: (() -> Void)?
    override func rightMouseDown(with event: NSEvent) {
        let menu = NSMenu()
        menu.autoenablesItems = false
        for (title, action) in [("Chat",#selector(chatAction)),("Settings…",#selector(settingsAction)),("Hide ram",#selector(hideAction))] {
            let item = NSMenuItem(title:title,action:action,keyEquivalent:"")
            item.target = self; item.isEnabled = true; menu.addItem(item)
        }
        NSMenu.popUpContextMenu(menu,with:event,for:self)
    }
    @objc func chatAction() { onClick?() }
    @objc func settingsAction() { onSettings?() }
    @objc func hideAction() { onHide?() }

    var dragged = false
    override func mouseDown(with event: NSEvent) { dragged = false }
    override func mouseDragged(with event: NSEvent) {
        dragged = true
        window?.performDrag(with:event)
        onMoved?()
        if let origin = window?.frame.origin {
            UserDefaults.standard.set(origin.x,forKey:"ram.x")
            UserDefaults.standard.set(origin.y,forKey:"ram.y")
        }
    }
    override func mouseUp(with event: NSEvent) { if !dragged { onClick?() } }
}
final class Companion: NSObject, NSWindowDelegate {
    let model = CompanionModel()
    let voice = VoiceModel()
    var pet: PetPanel?
    var chat: ChatPanel?
    var bubble: BubblePanel?
    var reviewTimer: Timer?
    var positioning = false
    var subscriptions = Set<AnyCancellable>()
    override init() {
        super.init()
        reviewTimer=Timer.scheduledTimer(withTimeInterval:30,repeats:true) { [weak self] _ in
            Task { @MainActor in await self?.model.pollReview() }
        }
        model.$tabReview.sink { [weak self] _ in
            DispatchQueue.main.async { self?.resizeChat(hasTurns:true) }
        }.store(in:&subscriptions)
        model.$turns.sink { [weak self] turns in
            DispatchQueue.main.async { self?.resizeChat(hasTurns:!turns.isEmpty) }
        }.store(in:&subscriptions)
        voice.onChange = { [weak self] in self?.updateBubble() }
    }
    /// Show, move or hide the speech bubble to match what the warden is saying.
    /// Pets come with Plus and Pro: without one, the ram is put away (the
    /// user's own show/hide choice is kept for when it comes back).
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
        let size = b.contentView?.fittingSize ?? NSSize(width:272,height:96)
        b.setContentSize(size)
        let screen = NSScreen.screens.first(where: { $0.frame.contains(NSPoint(x:pet.frame.midX,y:pet.frame.midY)) }) ?? NSScreen.main
        let bounds = screen?.visibleFrame ?? pet.frame
        let x = max(bounds.minX,min(pet.frame.midX-size.width/2,bounds.maxX-size.width))
        // Above the ram, or below it when the ram sits at the top of the screen.
        let above = pet.frame.maxY - 20
        let y = above + size.height <= bounds.maxY ? above : max(bounds.minY,pet.frame.minY-size.height)
        b.setFrameOrigin(NSPoint(x:x,y:y))
        b.orderFrontRegardless()
    }
    func hideBubble() { bubble?.orderOut(nil) }
    var colors: NSWindow?
    var visible: Bool { UserDefaults.standard.bool(forKey:"ram.visible") }
    func restore() { if visible && voice.pet { showPet() } }
    func toggle() {
        if pet != nil { closeChat(); hideBubble(); pet?.orderOut(nil); pet?.contentView = nil; pet = nil; UserDefaults.standard.set(false,forKey:"ram.visible") }
        else { UserDefaults.standard.set(true,forKey:"ram.visible"); showPet() }
        model.companionVisible = pet != nil
    }
    func showPet() {
        guard pet == nil, voice.pet else { return }
        let p = PetPanel(contentRect:NSRect(x:100,y:100,width:180,height:180),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        p.isOpaque = false; p.backgroundColor = .clear; p.hasShadow = false
        p.level = .floating; p.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]; p.hidesOnDeactivate = false
        let host = PetHost(rootView:PixelRam(model:model))
        host.onClick = { [weak self] in self?.toggleChat() }
        host.onMoved = { [weak self] in self?.positionChat(movePet:true) }
        host.onSettings = { [weak self] in self?.openColors() }
        host.onHide = { [weak self] in self?.toggle() }
        p.delegate = self
        p.contentView = host
        let d = UserDefaults.standard
        let desired = NSPoint(x:d.object(forKey:"ram.x") as? Double ?? 100,y:d.object(forKey:"ram.y") as? Double ?? 100)
        let screen = NSScreen.screens.first(where: { $0.visibleFrame.contains(desired) }) ?? NSScreen.main
        if let frame = screen?.visibleFrame {
            p.setFrameOrigin(NSPoint(x:max(frame.minX,min(desired.x,frame.maxX-180)),y:max(frame.minY,min(desired.y,frame.maxY-180))))
        }
        pet = p; p.orderFrontRegardless()
    }
    func toggleChat() {
        if chat?.isVisible == true { closeChat() } else { openChat() }
    }
    func openChat() {
        model.speaking = false
        hideBubble()
        if pet == nil { UserDefaults.standard.set(true,forKey:"ram.visible"); showPet(); model.companionVisible = true }
        if chat == nil {
            let w = ChatPanel(contentRect:NSRect(x:0,y:0,width:304,height:model.turns.isEmpty ? 80 : 240),styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
            w.title = "Ram chat"; w.isReleasedWhenClosed = false
            w.isOpaque = false; w.backgroundColor = .clear; w.hasShadow = true
            w.level = .floating; w.hidesOnDeactivate = false
            w.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
            w.contentView = NSHostingView(rootView:AssistantView(model:model,dismiss:{ [weak self] in self?.closeChat() },settings:{ [weak self] in self?.openColors() }))
            chat = w
        }
        resizeChat(hasTurns:!model.turns.isEmpty)
        positionChat(movePet:true)
        chat?.makeKeyAndOrderFront(nil)
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
        if movePet && pet.frame.minY < bounds.minY + chat.frame.height + 8 {
            pet.setFrameOrigin(NSPoint(x:pet.frame.minX,y:bounds.minY + chat.frame.height + 8))
            UserDefaults.standard.set(pet.frame.minY,forKey:"ram.y")
        }
        let x = max(bounds.minX,min(pet.frame.midX-chat.frame.width/2,bounds.maxX-chat.frame.width))
        let y = max(bounds.minY,pet.frame.minY-chat.frame.height-4)
        chat.setFrameOrigin(NSPoint(x:x,y:y))
    }
    func resizeChat(hasTurns: Bool) {
        guard let chat = chat else { return }
        var frame = chat.frame; frame.size.height = model.tabReview != nil ? 440 : hasTurns ? 240 : 80
        chat.setFrame(frame,display:true)
        positionChat(movePet:true)
    }
    func closeChat() {
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
    func openColors() {
        if colors == nil {
            let w = NSWindow(contentRect:NSRect(x:0,y:0,width:310,height:420),styleMask:[.titled,.closable],backing:.buffered,defer:false)
            w.title = "Ram settings"; w.isReleasedWhenClosed = false
            w.level = .floating; w.collectionBehavior = [.canJoinAllSpaces,.fullScreenAuxiliary]
            w.delegate = self
            w.contentView = NSHostingView(rootView:ColorsView(model:model)); w.center(); colors = w
        }
        NSApp.activate(ignoringOtherApps:true); colors?.makeKeyAndOrderFront(nil)
    }
}
