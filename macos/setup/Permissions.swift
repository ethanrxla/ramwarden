import SwiftUI
import AppKit

// The one place RamWarden asks for anything. The daemon's GET /permissions
// lists each need, why, whether it is granted and where to grant it; after
// setup nothing prompts again (docs/specs/permissions.md). A need left off
// only makes its feature smaller.
struct PermissionNeed: Identifiable, Equatable {
    let id: String
    let label: String
    let why: String
    let without: String
    let granted: Bool?
    let optional: Bool
    let settings: URL?
    let program: String?

    init?(_ value: Any) {
        guard let o = value as? [String: Any], let id = o["id"] as? String, let label = o["label"] as? String else { return nil }
        self.id = id; self.label = label
        why = o["why"] as? String ?? ""; without = o["without"] as? String ?? ""
        granted = o["granted"] as? Bool; optional = o["optional"] as? Bool ?? true
        settings = (o["settings"] as? String).flatMap(URL.init(string:)); program = o["program"] as? String
    }
}

@MainActor final class PermissionsModel: ObservableObject {
    @Published var needs: [PermissionNeed] = []
    @Published var status = "Not checked yet"
    @Published var checking = false

    func refresh() async {
        checking = true; defer { checking = false }
        guard let url = URL(string: "http://127.0.0.1:7823/permissions") else { return }
        var request = URLRequest(url: url); request.timeoutInterval = 5
        guard let (data, _) = try? await URLSession.shared.data(for: request),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            status = "The warden is not answering yet. Install it in the previous step, then check again."
            return
        }
        needs = (obj["needs"] as? [Any] ?? []).compactMap(PermissionNeed.init)
        status = obj["all_granted"] as? Bool == true ? "Everything is allowed. RamWarden will not ask again." : "Allow what you want now. RamWarden will not ask again later."
    }

    /// Opens the right pane and shows the program to add, so the user drags it
    /// in rather than hunting for a hidden binary.
    func grant(_ need: PermissionNeed) {
        if let program = need.program { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: program)]) }
        if let url = need.settings { NSWorkspace.shared.open(url) }
    }
}

struct PermissionsView: View {
    @StateObject private var model = PermissionsModel()
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("RamWarden asks for everything it needs here, once. After this step it never shows a permission prompt: anything you leave off just makes that feature smaller.")
                .fixedSize(horizontal: false, vertical: true)
            ForEach(model.needs) { need in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Image(systemName: need.granted == true ? "checkmark.circle.fill" : "circle").foregroundStyle(need.granted == true ? .green : .secondary)
                        Text(need.label).bold()
                        if need.optional { Text("optional").font(.caption).foregroundStyle(.secondary) }
                    }
                    Text(need.why).font(.callout).fixedSize(horizontal: false, vertical: true)
                    if need.granted != true {
                        Text("Without it: " + need.without).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        if need.settings != nil {
                            Button("Open System Settings") { model.grant(need) }
                            if need.program != nil {
                                Text("Turn on RamWarden's daemon in the list. If it is not there, press + and drag in the file Finder just showed you.").font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    }
                }.padding(10).background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
            }
            HStack { Button(model.checking ? "Checking…" : "Check again") { Task { await model.refresh() } }.disabled(model.checking); Text(model.status).font(.caption).foregroundStyle(.secondary) }
        }.task { await model.refresh() }
    }
}
