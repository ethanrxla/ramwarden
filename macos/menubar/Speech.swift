import AppKit
import AVFoundation
import Combine

// Speech credentials stay in the daemon. The UI receives only bounded MP3 audio.
final class CompanionSpeech: ObservableObject {
    @Published var errors:[String:String] = [:]
    static let shared = CompanionSpeech()
    private var players: [String:AVAudioPlayer] = [:]
    private var tasks: [String:Task<Void,Never>] = [:]
    func say(_ text: String, warden: String) {
        guard UserDefaults.standard.bool(forKey:"\(warden).speech"), !text.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty else { return }
        tasks.values.forEach { $0.cancel() }; players.values.forEach { $0.stop() }
        tasks[warden] = Task { @MainActor in
            do {
                var request = URLRequest(url:base.appendingPathComponent("warden/speech"))
                request.httpMethod = "POST"; request.timeoutInterval = 30
                request.setValue("application/json",forHTTPHeaderField:"Content-Type")
                request.httpBody = try JSONSerialization.data(withJSONObject:["warden":warden,"text":String(text.prefix(800))])
                let (data,response) = try await URLSession.shared.data(for:request)
                guard !Task.isCancelled else { return }
                if (response as? HTTPURLResponse)?.statusCode == 402 { self.errors[warden]="Spoken voice is unavailable on this service right now. Text chat still works.";return }
                guard UserDefaults.standard.bool(forKey:"\(warden).speech"),
                      (response as? HTTPURLResponse)?.statusCode == 200, data.count <= 2_000_000 else { return }
                self.errors[warden]=nil
                let player = try AVAudioPlayer(data:data)
                self.players[warden] = player; player.prepareToPlay(); player.play()
            } catch { /* Text remains usable when speech is unavailable. */ }
        }
    }
    func stop(_ warden: String) { tasks[warden]?.cancel(); players[warden]?.stop(); players[warden] = nil }
}
