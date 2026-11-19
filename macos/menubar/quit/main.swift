// ramwarden-quit: ask one running app to quit, the way its own Quit menu item would.
//
// The daemon used `osascript -e 'tell application "X" to quit'`. That is an
// Apple event to X, so macOS asked "RamWarden wants access to control X" the
// first time each app was chosen, in the middle of use (Apple Music on the
// reference Mac, 2026-11-19). NSRunningApplication.terminate() asks the app to
// quit through AppKit instead: the app still shows its own save prompt and may
// refuse, and no Automation grant is involved.
//
// Usage: ramwarden-quit <app name>   exit 0 sent, 1 not running, 2 refused, 64 usage.
import AppKit

let args = CommandLine.arguments.dropFirst()
guard let name = args.first, !name.isEmpty, args.count == 1 else {
    FileHandle.standardError.write("usage: ramwarden-quit <app name>\n".data(using: .utf8)!)
    exit(64)
}
let wanted = name.lowercased()
let matches = NSWorkspace.shared.runningApplications.filter {
    $0.activationPolicy == .regular && ($0.localizedName?.lowercased() == wanted || $0.bundleURL?.deletingPathExtension().lastPathComponent.lowercased() == wanted)
}
guard !matches.isEmpty else { exit(1) }
// Never ourselves, never the Finder or the Dock: the daemon's allow-list should
// not name them, and this is the last place to refuse.
let never: Set<String> = ["com.ramwarden.menubar", "com.apple.finder", "com.apple.dock", "com.apple.loginwindow"]
let sent = matches.filter { !never.contains($0.bundleIdentifier ?? "") }.map { $0.terminate() }
exit(sent.contains(true) ? 0 : 2)
