import AppKit
import AVFoundation
import Combine

// Speech credentials stay in the daemon. The UI receives only bounded MP3 audio.
@MainActor final class CompanionSpeech: ObservableObject {
    @Published var errors:[String:String] = [:]
    static let shared = CompanionSpeech()
    private var players: [String:AVAudioPlayer] = [:]
    private var tasks: [String:Task<Void,Never>] = [:]
    private var visibility: [String:() -> Bool] = [:]
    private var visibilityObserver: NSObjectProtocol?
    private init() {
        visibilityObserver = NotificationCenter.default.addObserver(forName:NSWindow.didChangeOcclusionStateNotification,object:nil,queue:.main) { [weak self] _ in
            Task { @MainActor in self?.stopHiddenCompanions() }
        }
    }
    func trackVisibility(_ warden:String, visible:@escaping () -> Bool) { visibility[warden]=visible }
    private func canSpeak(_ warden:String)->Bool {
        visibility[warden]?() == true && !CompanionAudio.isMuted(warden) && UserDefaults.standard.bool(forKey:"\(warden).speech")
    }
    private func stopHiddenCompanions() {
        for warden in Array(visibility.keys) where visibility[warden]?() != true { stop(warden) }
    }
    private var speakingReply: String?
    private var speechID = UUID()
    // The Mac's own voice, for when the daemon cannot produce the warden's: no
    // key, the provider's quota used up, or the provider unreachable. A reply
    // nobody hears looked like a broken feature (2026-11-20: the key had no
    // credit left and the app said only "unavailable on this service").
    private let systemVoice = AVSpeechSynthesizer()
    private var systemVoiceOwner: String?
    /// Statuses that mean "no audio could be made": key rejected or missing,
    /// quota, provider down. Not 403 (voice off or muted), 422 (nothing to
    /// say) or 429 (resting, or already speaking): those must stay silent.
    nonisolated static func fallsBackToSystemVoice(_ status: Int) -> Bool { [401, 402, 502, 503].contains(status) }
    /// The daemon's own sentence for a refusal: {"error": "..."}.
    nonisolated static func refusal(_ data: Data) -> String? {
        ((try? JSONSerialization.jsonObject(with: data)) as? [String: Any])?["error"] as? String
    }

    static func chunks(_ text: String) -> [String] {
        var remaining = text.unicodeScalars[...]
        var parts: [String] = []
        while !remaining.isEmpty {
            let window = remaining.prefix(800)
            var end = window.endIndex
            if end != remaining.endIndex {
                if let sentence = window.indices.last(where: { index in
                    ".!?".unicodeScalars.contains(window[index]) &&
                    remaining[remaining.index(after: index)].properties.isWhitespace
                }) {
                    end = remaining.index(after: sentence)
                } else if let space = window.lastIndex(where: { $0.properties.isWhitespace }),
                          space != window.startIndex {
                    end = remaining.index(after: space)
                }
            }
            let chunk = String(String.UnicodeScalarView(remaining[..<end]))
            if !chunk.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { parts.append(chunk) }
            remaining = remaining[end...]
        }
        return parts
    }

    func say(_ text: String, warden: String, background: Bool = false) {
        guard canSpeak(warden), !background || speakingReply == nil else { return }
        let parts = Self.chunks(text)
        guard !parts.isEmpty else { return }
        for name in Array(tasks.keys) { stop(name) }
        let id = UUID()
        speechID = id
        if !background { speakingReply = warden }
        tasks[warden] = Task { @MainActor in
            defer { if speechID == id { stop(warden) } }
            do {
                var useSystemVoice = false
                for chunk in parts {
                    try Task.checkCancellation()
                    guard canSpeak(warden) else { return }
                    if useSystemVoice { try await self.speakWithSystemVoice(chunk, warden: warden); continue }
                    var request = URLRequest(url: base.appendingPathComponent("warden/speech"))
                    request.httpMethod = "POST"; request.timeoutInterval = 30
                    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                    request.httpBody = try JSONSerialization.data(withJSONObject: ["warden": warden, "text": chunk])
                    let (data, response) = try await URLSession.shared.data(for: request)
                    try Task.checkCancellation()
                    let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                    if status != 200 {
                        // Say why in the daemon's words, so it can be fixed.
                        let why = Self.refusal(data) ?? "The warden's voice is unavailable right now."
                        guard Self.fallsBackToSystemVoice(status) else { self.errors[warden] = why + " Text chat still works."; return }
                        self.errors[warden] = why + " Using this Mac's built-in voice until that is fixed."
                        useSystemVoice = true
                        try await self.speakWithSystemVoice(chunk, warden: warden)
                        continue
                    }
                    guard canSpeak(warden), data.count <= 2_000_000 else { return }
                    self.errors[warden]=nil
                    let player = try AVAudioPlayer(data: data)
                    self.players[warden] = player
                    guard player.play() else { return }
                    while player.isPlaying { try await Task.sleep(for: .milliseconds(100)) }
                }
            } catch { if !(error is CancellationError) && !Task.isCancelled { self.errors[warden]="Spoken voice is temporarily unavailable. Text chat still works." } }
        }
    }

    private func speakWithSystemVoice(_ text: String, warden: String) async throws {
        guard canSpeak(warden) else { return }
        systemVoiceOwner = warden
        systemVoice.speak(AVSpeechUtterance(string: text))
        while systemVoice.isSpeaking { try await Task.sleep(for: .milliseconds(100)) }
    }

    func stop(_ warden: String) {
        tasks.removeValue(forKey: warden)?.cancel()
        players.removeValue(forKey: warden)?.stop()
        if systemVoiceOwner == warden { systemVoice.stopSpeaking(at: .immediate); systemVoiceOwner = nil }
        if speakingReply == warden { speakingReply = nil }
    }
}

@MainActor enum CompanionAudio {
    nonisolated static func isMuted(_ warden:String)->Bool {
        let d=UserDefaults.standard
        let until=d.double(forKey:"\(warden).audioMutedUntil")
        return d.bool(forKey:"\(warden).audioMuted") && (until == 0 || until > Date().timeIntervalSince1970)
    }
    static func setMuted(_ muted:Bool,warden:String,hours:Double? = nil) {
        UserDefaults.standard.set(hours.map{Date().addingTimeInterval($0*3600).timeIntervalSince1970} ?? 0,forKey:"\(warden).audioMutedUntil")
        UserDefaults.standard.set(muted,forKey:"\(warden).audioMuted")
        if muted {CompanionSpeech.shared.stop(warden); GoatAlerts.shared.stop()}
    }
}
