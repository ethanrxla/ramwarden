// Compile with Companion.swift only. All HTTP is intercepted; no daemon is used.
import AppKit
import SwiftUI
import Foundation

let base = URL(string: "http://127.0.0.1:1")!

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
        let model = CompanionModel()
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
        model.turns = [ChatTurn(role: "assistant", content: "Fixture reply")]
        precondition(model.chatHeight == 240)
        print("PASS: native checklist render, unchecked defaults, selected-only close, Keep, Later, expiry and chat sizing")
    }
}
