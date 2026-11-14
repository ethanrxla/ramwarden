// Compile with Companion.swift, Voice.swift and Speech.swift. All HTTP is intercepted; no daemon is used.
import AppKit
import SwiftUI
import Foundation

let base = URL(string: "http://127.0.0.1:1")!
// Voice.swift shares main.swift's small HTTP helper. All test traffic is mocked.
func fetchJSON(_ path: String) async -> Any? {
    guard let (data, _) = try? await URLSession.shared.data(from: base.appendingPathComponent(path)) else { return nil }
    return try? JSONSerialization.jsonObject(with: data)
}

final class ReviewProtocol: URLProtocol {
    static var requests: [[String: Any]] = []
    static var response = "{}"
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        var body = request.httpBody ?? Data()
        if let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(contentsOf: buffer.prefix(count))
            }
        }
        if let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
            Self.requests.append(object)
        }
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: 200,
            httpVersion: nil, headerFields: ["Content-Type": "application/json"])!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(Self.response.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

@main struct ReviewChecks {
    @MainActor static func main() async throws {
        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        URLProtocol.registerClass(ReviewProtocol.self)
        let model = CompanionModel(persistMemory:false)
        model.companionVisible = true
        let tabs = [
            ReviewTab(browser: "fixture", id: 1, url: "https://example.org/one", title: "Research notes", reason: "You might return to this tab"),
            ReviewTab(browser: "fixture", id: 2, url: "https://example.org/two", title: "Reference article", reason: "Waiting for your decision")
        ]
        func offer(_ token: String = "fixture", expiry: Double = Date().timeIntervalSince1970 + 600) {
            model.tabReview = TabReview(token: token, expires_at: expiry, tabs: tabs)
        }
        func finish() async throws {
            for _ in 0..<100 {
                if !model.reviewBusy { return }
                try await Task.sleep(nanoseconds: 10_000_000)
            }
            preconditionFailure("review response timed out")
        }
        precondition(model.chatHeight == 80)
        model.focusProposal = FocusProposal(token: "focus", tabs: [], expires_at: Date().timeIntervalSince1970 + 600)
        precondition(model.chatHeight == 360)
        model.focusProposal = nil
        offer()
        precondition(model.selectedReviewTabs.isEmpty)
        precondition(model.chatHeight == 440)
        // AppKit-backed glass cannot be captured by SwiftUI ImageRenderer.
        let host = NSHostingView(rootView: AssistantView(model: model, dismiss: {}, settings: {}).environment(\.colorScheme, .light).background(Color.white))
        precondition(host.fittingSize.height == 440, "checklist content must fill the review window")
        let window = NSWindow(contentRect: NSRect(x: -5000, y: 0, width: 304, height: 440),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        window.orderFrontRegardless()
        host.layoutSubtreeIfNeeded()
        try await Task.sleep(nanoseconds: 200_000_000)
        guard let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) else {
            preconditionFailure("checklist did not render")
        }
        host.cacheDisplay(in: host.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else {
            preconditionFailure("checklist image unavailable")
        }
        precondition(host.bounds.height == 440)
        window.orderOut(nil)
        if let output = ProcessInfo.processInfo.environment["RW_REVIEW_IMAGE"] {
            try png.write(to: URL(fileURLWithPath: output))
        }
        model.selectedReviewTabs = [tabs[1].key]
        model.answerReview("close")
        try await finish()
        precondition(ReviewProtocol.requests.last?["action"] as? String == "close")
        let sent = ReviewProtocol.requests.last?["tabs"] as? [[String: Any]]
        precondition(sent?.count == 1 && sent?.first?["id"] as? Int == 2)
        precondition(model.tabReview == nil && model.selectedReviewTabs.isEmpty)
        offer(); model.selectedReviewTabs = [tabs[0].key]
        model.answerReview("keep"); try await finish()
        precondition(ReviewProtocol.requests.last?["action"] as? String == "keep")
        offer(); model.answerReview("later"); try await finish()
        precondition(ReviewProtocol.requests.last?["action"] as? String == "later")
        let count = ReviewProtocol.requests.count
        offer(expiry: Date().timeIntervalSince1970 - 1)
        model.answerReview("close")
        precondition(ReviewProtocol.requests.count == count && model.tabReview == nil)
        let recallTurns=[ChatTurn(role:"user",content:"My project is a tutoring scheduler")]+(0..<8).map{ChatTurn(role:"assistant",content:"Unrelated \($0)")}
        let recalled=CompanionRecall.context(recallTurns,query:"What about my tutoring project?")
        precondition(recalled.count == 5 && recalled.first?["content"] == "My project is a tutoring scheduler")
        var baa=BaaPolicy()
        precondition(baa.accept(warden:"ram",event:"critical",muted:false,now:1000))
        precondition(!baa.accept(warden:"ram",event:"critical",muted:false,now:3000))
        precondition(!baa.accept(warden:"threat",event:"new-alert",muted:false,now:1100))
        precondition(!baa.accept(warden:"threat",event:"muted-alert",muted:true,now:3000))
        precondition(baa.accept(warden:"threat",event:"serious-alert",muted:false,now:3001))
        baa.clear("ram")
        precondition(baa.accept(warden:"ram",event:"critical",muted:false,now:4001))
        model.turns = [ChatTurn(role: "assistant", content: "Fixture reply")]
        precondition(model.chatHeight == 240)
        let threat=CompanionModel(profile:ThreatWardenProfile(),persistMemory:false)
        let ramEyes=model.eyes;threat.eyes="55FF88";precondition(model.eyes==ramEyes && threat.profile.id == "threat")
        let selected=WardenSettingsSelection();selected.selected="threat"
        let settings=NSHostingView(rootView:WardenSettingsView(ram:model,threat:threat,selection:selected))
        settings.frame=NSRect(x:0,y:0,width:334,height:714)
        let settingsWindow=NSWindow(contentRect:settings.frame,styleMask:[.titled],backing:.buffered,defer:false)
        settingsWindow.contentView=settings;settingsWindow.orderFront(nil);settings.layoutSubtreeIfNeeded()
        try await Task.sleep(nanoseconds:200_000_000)
        if let output=ProcessInfo.processInfo.environment["RW_SETTINGS_IMAGE"],let bitmap=settings.bitmapImageRepForCachingDisplay(in:settings.bounds){
            settings.cacheDisplay(in:settings.bounds,to:bitmap)
            if let png=bitmap.representation(using:.png,properties:[:]){try png.write(to:URL(fileURLWithPath:output))}
        }
        selected.selected="ram";settings.layoutSubtreeIfNeeded();precondition(model.profile.id == "ram")
        settingsWindow.orderOut(nil)
        model.receiveToolEvent(["kind":"started","index":0,"total":2,"label":"Create notes"])
        model.receiveToolEvent(["kind":"started","index":0,"total":2,"label":"Duplicate event"])
        precondition(model.toolBlocks.count == 1 && model.toolBlocks[0].state == "Running")
        model.receiveToolEvent(["kind":"result","index":0,"total":2,"label":"Create notes","succeeded":true,"result":["bytes_written":8]])
        precondition(model.toolBlocks[0].state == "Completed" && model.toolBlocks[0].output.contains("bytes_written"))
        precondition(model.turns.last?.toolResult?.label == "Completed · Create notes")
        model.receiveToolEvent(["kind":"started","index":1,"total":2,"label":"Read missing file"])
        model.receiveToolEvent(["kind":"result","index":1,"total":2,"label":"Read missing file","succeeded":false,"result":["error":"Missing file"]])
        precondition(model.toolBlocks[1].state == "Failed")
        model.receiveToolEvent(["kind":"finished","succeeded":false,"reply":"Stopped after 1 successful step of 2."])
        precondition(model.turns.last?.content == "Stopped after 1 successful step of 2.")
        precondition(threat.toolBlocks.isEmpty,"Companion tool output must stay separate")
        model.receiveToolEvent(["kind":"started","index":99,"total":100,"label":"Invalid event"])
        precondition(model.toolBlocks.count == 2)
        model.receiveToolEvent(["kind":"started","index":2,"total":3,"label":"Interrupted diagnostic"])
        model.markUnconfirmedTools()
        precondition(model.toolBlocks[2].state == "Unconfirmed")
        if let output=ProcessInfo.processInfo.environment["RW_WORKFLOW_IMAGE"] {
            model.tabReview=nil;model.focusProposal=nil
            model.turns=Array(model.turns.suffix(3))
            let preview=NSHostingView(rootView:AssistantView(model:model,dismiss:{},settings:{}).environment(\.colorScheme,.dark))
            let frame=NSRect(x:0,y:0,width:304,height:model.chatHeight)
            preview.frame=frame
            let window=NSWindow(contentRect:frame,styleMask:[.borderless],backing:.buffered,defer:false)
            window.contentView=preview;window.orderFrontRegardless();preview.layoutSubtreeIfNeeded()
            try await Task.sleep(nanoseconds:200_000_000)
            if let bitmap=preview.bitmapImageRepForCachingDisplay(in:preview.bounds) {
                preview.cacheDisplay(in:preview.bounds,to:bitmap)
                if let png=bitmap.representation(using:.png,properties:[:]) {try png.write(to:URL(fileURLWithPath:output))}
            }
            window.orderOut(nil)
        }
        print("PASS: native checklist, workspace progress/results, companion isolation and chat sizing")
    }
}
