import AppKit

// The one time the menu-bar app asks for anything: Full Disk Access, when
// setup never got the chance to (a script install, or setup closed early).
// `FullDiskAccess` (macos/setup/Permissions.swift) holds the shared marker, so
// whichever app asks first is the only one that ever asks. Declining is final:
// storage scans leave the private folders out and say so, and the setup
// step's button still opens the pane. docs/specs/permissions.md.
@MainActor enum FullDiskPrompt {
    /// Waits for the daemon (it may still be starting at login), then asks if
    /// Full Disk Access is off and nothing has asked before.
    static func askOnce() async {
        guard !FullDiskAccess.asked else { return }
        for _ in 0..<20 {
            if let view = await fetchJSON("permissions") as? [String: Any] {
                let needs = view["needs"] as? [[String: Any]] ?? []
                guard let need = needs.first(where: { $0["id"] as? String == "full_disk_access" }),
                      need["granted"] as? Bool == false, !FullDiskAccess.asked else { return }
                FullDiskAccess.markAsked()
                ask(program: need["program"] as? String)
                return
            }
            try? await Task.sleep(nanoseconds: 3_000_000_000)
        }
    }

    private static func ask(program: String?) {
        let alert = NSAlert()
        alert.messageText = "Turn on RamWarden in Full Disk Access"
        alert.informativeText = "This lets the storage warden measure Desktop, Documents, Downloads, your media libraries and other apps' data when your disk is filling up. Nothing is read for any other reason, and nothing leaves this Mac.\n\nSystem Settings will open on Full Disk Access: switch on RamWarden (the red ram). If it is not in the list, press + and drag in the RamWarden that Finder shows you.\n\nRamWarden asks this once. Without it, those folders are left out of storage scans."
        // The same icon the row in System Settings will show.
        if let program = program { alert.icon = NSWorkspace.shared.icon(forFile: program) }
        alert.addButton(withTitle: "Open System Settings")
        alert.addButton(withTitle: "Not now")
        NSApp.activate(ignoringOtherApps: true)
        if alert.runModal() == .alertFirstButtonReturn { FullDiskAccess.open(program: program) }
    }
}
