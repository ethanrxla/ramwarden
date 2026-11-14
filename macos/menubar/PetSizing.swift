import AppKit
import SwiftUI

extension Notification.Name { static let wardenPetSizeChanged = Notification.Name("WardenPetSizeChanged") }

/// Integer pixel scales keep the 48×48 artwork crisp at every supported size.
enum PetSizing {
    static func normalized(_ value: Double) -> Int {
        guard value.isFinite else { return 3 }
        return Int(max(2, min(6, value)).rounded())
    }
    static func scale(_ warden: String, defaults: UserDefaults = .standard) -> Int {
        guard defaults.object(forKey: "\(warden).pixelScale") != nil else { return 3 }
        return normalized(defaults.double(forKey: "\(warden).pixelScale"))
    }
    static func size(_ warden: String, scale: Int) -> NSSize {
        let pixels = CGFloat(48 * normalized(Double(scale)))
        return NSSize(width: pixels + (warden == "threat" ? 16 : 36), height: pixels + 36)
    }
    /// Anchor auxiliary windows beside the pet without escaping the current display.
    static func attachedFrame(pet: NSRect, size: NSSize, bounds: NSRect, preferAbove: Bool) -> NSRect {
        let size = NSSize(width:min(size.width,bounds.width),height:min(size.height,bounds.height))
        let above = pet.maxY + 4
        let below = pet.minY - size.height - 4
        let fitsAbove = above + size.height <= bounds.maxY
        let fitsBelow = below >= bounds.minY
        let y = preferAbove ? (fitsAbove ? above : below) : (fitsBelow ? below : above)
        return NSRect(x:max(bounds.minX,min(pet.midX-size.width/2,bounds.maxX-size.width)),
                      y:max(bounds.minY,min(y,bounds.maxY-size.height)),width:size.width,height:size.height)
    }
    /// Keep the pet's center and bottom edge anchored, then fit it on-screen.
    static func resizedFrame(_ old: NSRect, size: NSSize, bounds: NSRect) -> NSRect {
        let width = min(size.width, bounds.width), height = min(size.height, bounds.height)
        return NSRect(x: max(bounds.minX, min(old.midX-width/2, bounds.maxX-width)),
                      y: max(bounds.minY, min(old.minY, bounds.maxY-height)), width: width, height: height)
    }
}

struct PetSizeControl: View {
    let warden: String
    let label: String
    @AppStorage private var storedScale: Double
    init(warden: String, label: String) {
        self.warden = warden; self.label = label
        _storedScale = AppStorage(wrappedValue: 3, "\(warden).pixelScale")
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label).font(.subheadline)
            HStack {
                Text("Small").font(.caption).foregroundStyle(.secondary)
                Slider(value: Binding(get: { Double(PetSizing.normalized(storedScale)) }, set: { value in
                    storedScale = Double(PetSizing.normalized(value))
                    NotificationCenter.default.post(name: .wardenPetSizeChanged, object: nil, userInfo: ["warden": warden])
                }), in: 2...6, step: 1).accessibilityLabel(label)
                Text("Large").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
