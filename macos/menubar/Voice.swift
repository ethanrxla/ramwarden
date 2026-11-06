// The warden's speech bubble on macOS: a small panel above the floating ram.
//
// The daemon decides what the warden says and when (docs/specs/warden-voice.md);
// this only shows the bubble from GET /warden/bubble and posts the user's choice
// to /warden/answer, so the Mac, Windows, Linux and the extension all say the
// same thing and an answer anywhere counts everywhere. With the ram hidden, a
// question appears at the top of the menu-bar menu instead.
import AppKit
import SwiftUI

struct WardenChoice: Identifiable, Equatable { let id: String; let label: String }

struct WardenBubble: Equatable {
    let id: String
    let kind: String
    let text: String
    let choices: [WardenChoice]
    let url: String?
    let source:URL?
    var isQuestion: Bool { !choices.isEmpty }

    init?(_ obj: Any?) {
        guard let b = obj as? [String: Any], let id = b["id"] as? String, let text = b["text"] as? String else { return nil }
        self.id = id
        kind = b["kind"] as? String ?? "quip"
        self.text = text
        choices = (b["choices"] as? [[String: Any]] ?? []).compactMap {
            guard let id = $0["id"] as? String, let label = $0["label"] as? String else { return nil }
            return WardenChoice(id: id, label: label)
        }
        source=(b["source_url"] as? String).flatMap(URL.init(string:))
        url = (b["tab"] as? [String: Any])?["url"] as? String
    }
}

/// The same mute choices as the other faces: (label, hours), nil = until unmuted.
let wardenMuteChoices: [(String, Double?)] = [("Quiet for an hour", 1), ("Quiet for today", 12), ("Quiet until I unmute", nil)]

@MainActor final class VoiceModel: ObservableObject {
    @Published var bubble: WardenBubble?
    @Published var muted = false
    /// Pets come with Plus and Pro, not Lite.
    @Published var pet = true
    @Published var busy = false
    /// Called on the main thread when the bubble or the pet changes.
    var onChange: (() -> Void)?
    var onFocusRequested: (() -> Void)?
    var allowSpeech: (() -> Bool)?
    private var answerVisibleUntil = Date.distantPast

    func poll() async {
        guard let obj = await fetchJSON("warden/bubble") as? [String: Any] else { return }
        await MainActor.run {
            muted = obj["muted"] as? Bool ?? false
            guard Date() >= answerVisibleUntil else { return }
            let next = WardenBubble(obj["bubble"])
            let changed = next != bubble || (obj["pet"] as? Bool ?? true) != pet
            bubble = next
            muted = obj["muted"] as? Bool ?? false
            pet = obj["pet"] as? Bool ?? true
            if changed {
                onChange?()
                if let line = next, !muted, allowSpeech?() != false { CompanionSpeech.shared.say(line.text,warden:"ram") }
            }
        }
    }

    private func post(_ path: String, _ body: [String: Any]) async -> [String: Any]? {
        var req = URLRequest(url: base.appendingPathComponent(path))
        req.httpMethod = "POST"; req.timeoutInterval = 40
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try? JSONSerialization.data(withJSONObject: body)
        guard let (data, _) = try? await URLSession.shared.data(for: req) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    func answer(_ choice: String) {
        if choice == "focus" { onFocusRequested?(); return }
        guard let b = bubble, !busy else { return }
        busy = true
        Task {
            let response = await post("warden/answer", ["id": b.id, "choice": choice])
            await MainActor.run {
                self.busy = false
                // Keep the result visible even if the question expired or the
                // browser refused the close. Polling alone loses HTTP errors.
                let reply = response?["reply"] as? String
                    ?? "I couldn't confirm that choice reached the browser. The tab may still be open."
                if !reply.isEmpty {
                    self.answerVisibleUntil = Date().addingTimeInterval(15)
                    self.bubble = WardenBubble(["id": "answer-" + b.id, "kind": "reply", "text": reply, "choices": []] as [String: Any])
                    self.onChange?()
                }
            }
        }
    }

    func mute(_ hours: Double?) {
        CompanionSpeech.shared.stop("ram")
        Task {
            var body: [String: Any] = ["muted": true]
            if let h = hours { body["hours"] = h }
            if let response = await post("warden/mute", body), let state = response["muted"] as? Bool { self.muted = state; self.onChange?() }
            await poll()
        }
    }

    func unmute() { Task { if let response = await post("warden/mute", ["muted": false]), let state = response["muted"] as? Bool { self.muted = state; self.onChange?() }; await poll() } }
}

struct BubbleView: View {
    @ObservedObject var voice: VoiceModel
    var body: some View {
        if let b = voice.bubble {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 6) {
                    Text(b.text).font(.system(size: 12)).fixedSize(horizontal: false, vertical: true)
                        .help(b.url ?? "")
                    Spacer(minLength: 4)
                    Menu {
                        ForEach(wardenMuteChoices.indices, id: \.self) { i in
                            Button(wardenMuteChoices[i].0) { voice.mute(wardenMuteChoices[i].1) }
                        }
                    } label: { Image(systemName: "speaker.slash").font(.system(size: 10)) }
                        .menuStyle(.borderlessButton).fixedSize().accessibilityLabel("Quiet, please")
                    Button { voice.answer("dismiss") } label: { Image(systemName: "xmark").font(.system(size: 9, weight: .semibold)) }
                        .buttonStyle(.plain).accessibilityLabel("Dismiss")
                }
                if let source=b.source,source.scheme == "https" {Link("Fact source",destination:source).font(.caption2)}
                if b.isQuestion {
                    HStack(spacing: 6) {
                        ForEach(b.choices) { c in
                            Button(c.label) { voice.answer(c.id) }
                                .controlSize(.small).disabled(voice.busy)
                        }
                    }
                }
            }
            .padding(10).frame(width: 260, alignment: .leading)
            .modifier(ChatGlass())
            .overlay(RoundedRectangle(cornerRadius: 20).stroke(Color(red: 0.75, green: 0.22, blue: 0.17), lineWidth: 1.5))
            .padding(6)
        }
    }
}

/// A nonactivating panel: clicking a choice never steals focus from your work.
final class BubblePanel: NSPanel { override var canBecomeKey: Bool { false } }
