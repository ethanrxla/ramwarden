import AppKit
import SwiftUI

@MainActor final class WardenSettingsSelection:ObservableObject { @Published var selected="ram" }
struct WardenSettingsView:View {
    @ObservedObject var ram:CompanionModel
    @ObservedObject var threat:CompanionModel
    @ObservedObject var selection:WardenSettingsSelection
    var body:some View {
        VStack(spacing:0) {
            Picker("Companion",selection:$selection.selected){Text("RamWarden").tag("ram");Text("ThreatWarden").tag("threat")}.pickerStyle(.segmented).padding(12)
            ColorsView(model:selection.selected == "threat" ? threat : ram).id(selection.selected)
        }.onChange(of:selection.selected){_ in NSColorPanel.shared.close()}
    }
}

struct ColorsView: View {
    @ObservedObject private var speech=CompanionSpeech.shared
    @AppStorage private var spoken:Bool
    @AppStorage private var audioMuted:Bool
    @StateObject private var quietVoice=VoiceModel()
    init(model:CompanionModel){self.model=model;self._audioMuted=AppStorage(wrappedValue:false,"\(model.profile.id).audioMuted");self._spoken=AppStorage(wrappedValue:false,"\(model.profile.id).speech")}
    @ObservedObject var model: CompanionModel
    func binding(_ key: ReferenceWritableKeyPath<CompanionModel,String>) -> Binding<Color> {
        Binding(get: { ramColor(model[keyPath:key]) }, set: { color in
            guard let rgb = NSColor(color).usingColorSpace(.deviceRGB) else { return }
            model[keyPath:key] = String(format:"%02X%02X%02X",Int(rgb.redComponent*255),Int(rgb.greenComponent*255),Int(rgb.blueComponent*255))
            model.saveColors()
        })
    }
    var body: some View {
        ScrollView {
        VStack(spacing:12) {
            Text(model.profile.id == "ram" ? "Customize your ram" : "Customize ThreatWarden").font(.title2.bold())
            Toggle("Mute companion audio",isOn:$audioMuted).onChange(of:audioMuted){CompanionAudio.setMuted($0,warden:model.profile.id)}
            if model.profile.id == "ram" {Toggle("Quiet automatic comments",isOn:Binding(get:{quietVoice.muted},set:{if $0 {quietVoice.mute(nil)} else {quietVoice.unmute()}}))}
            GoatAlertControl(warden:model.profile.id)
            Button("Forget local chat history"){model.forgetHistory()}
            Toggle("Spoken replies",isOn:$spoken).onChange(of:spoken) { if !$0 { CompanionSpeech.shared.stop(model.profile.id) } }
            if let error=speech.errors[model.profile.id] {Text(error).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal:false,vertical:true)}
            TextField("Name",text:Binding(get:{model.name},set:{ value in
                let name = String(value.prefix(40)); model.name = name
                UserDefaults.standard.set(name.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty ? model.profile.name : name,forKey:"\(model.profile.id).name")
            })).textFieldStyle(.roundedBorder).accessibilityLabel("Companion name")
            Button("Choose folder for file tools…"){model.chooseWorkspace()}
            Text(model.workspaceStatus).font(.caption).foregroundStyle(.secondary)
            Button("Disconnect folder"){model.setWorkspace(nil)}
            Toggle("Auto file and terminal tools",isOn:Binding(get:{model.automaticTools},set:{model.setAutomaticTools($0)}))
            Text(model.automaticTools ? "Auto · supported tools in this folder" : "Manual · review each operation").font(.caption).foregroundStyle(.secondary)
            PetSizeControl(warden:model.profile.id,label:"Companion size")
            if model.profile.id == "ram" {PixelRam(model:model,animated:false).frame(width:180,height:180)}
            else {ThreatAvatar(model:model).frame(width:180,height:180)}
            ColorPicker(model.profile.id == "ram" ? "Wool" : "Metal",selection:binding(\.wool),supportsOpacity:false)
            ColorPicker(model.profile.id == "ram" ? "Horns" : "Armor / horns",selection:binding(\.horns),supportsOpacity:false)
            ColorPicker(model.profile.id == "ram" ? "Hooves" : "Boots",selection:binding(\.hooves),supportsOpacity:false)
            ColorPicker(model.profile.id == "ram" ? "Eyes" : "Visor / lights",selection:binding(\.eyes),supportsOpacity:false)
            if model.profile.id == "ram" {
            Picker("Pupils",selection:Binding(get:{model.pupilStyle},set:{model.pupilStyle = $0; model.saveColors()})) {
                Text("Ram slits").tag("slit")
                Text("Round").tag("round")
                Text("Glowing / none").tag("none")
            }
            Button("Reset eyes") { model.eyes = "FFFFFF"; model.pupilStyle = "slit"; model.saveColors() }
            }
        }.padding(24).frame(width:260)
        }.frame(width:310,height:660).onAppear{model.refreshWorkspace();Task{await quietVoice.poll()}}
    }
}

