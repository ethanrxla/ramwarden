import AppKit
import AVFoundation

// Speech credentials stay in the daemon. The UI receives only bounded MP3 audio.
final class CompanionSpeech {
    static let shared = CompanionSpeech()
    private var players: [String:AVAudioPlayer] = [:]
    private var tasks: [String:Task<Void,Never>] = [:]
    private var speakingReply: String?
    private var speechID = UUID()

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
        guard UserDefaults.standard.bool(forKey: "\(warden).speech"),
              !background || speakingReply == nil else { return }
        let parts = Self.chunks(text)
        guard !parts.isEmpty else { return }
        for name in Array(tasks.keys) { stop(name) }
        let id = UUID()
        speechID = id
        if !background { speakingReply = warden }
        tasks[warden] = Task { @MainActor in
            defer { if speechID == id { stop(warden) } }
            do {
                for chunk in parts {
                    try Task.checkCancellation()
                    guard UserDefaults.standard.bool(forKey: "\(warden).speech") else { return }
                    var request = URLRequest(url: base.appendingPathComponent("warden/speech"))
                    request.httpMethod = "POST"; request.timeoutInterval = 30
                    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                    request.httpBody = try JSONSerialization.data(withJSONObject: ["warden": warden, "text": chunk])
                    let (data, response) = try await URLSession.shared.data(for: request)
                    try Task.checkCancellation()
                    guard UserDefaults.standard.bool(forKey: "\(warden).speech"),
                          (response as? HTTPURLResponse)?.statusCode == 200,
                          data.count <= 2_000_000 else { return }
                    let player = try AVAudioPlayer(data: data)
                    self.players[warden] = player
                    guard player.play() else { return }
                    while player.isPlaying { try await Task.sleep(for: .milliseconds(100)) }
                }
            } catch { /* Text remains usable when speech is unavailable. */ }
        }
    }

    func stop(_ warden: String) {
        tasks.removeValue(forKey: warden)?.cancel()
        players.removeValue(forKey: warden)?.stop()
        if speakingReply == warden { speakingReply = nil }
    }
}
