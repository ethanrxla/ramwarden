import AppKit
import SwiftUI
import Combine

class WardenProfile {
    let id:String; let name:String; let placeholder:String
    init(id:String,name:String,placeholder:String) {self.id=id;self.name=name;self.placeholder=placeholder}
}
final class RamWardenProfile: WardenProfile {
    init(){super.init(id:"ram",name:"Warden",placeholder:"Ask your ram…")}
}
final class ThreatWardenProfile: WardenProfile {
    init(){super.init(id:"threat",name:"ThreatWarden",placeholder:"Ask about your protection…")}
}
// Shared identity, conversation transport, glass panel and speech-bubble lifecycle.
// Memory and security companions specialize their observations, not each other's actions.
class WardenCompanion: NSObject {
    let model:CompanionModel
    var assistantPanel:ChatPanel?
    var statusBubble:BubblePanel?
    private var chatObserver:AnyCancellable?
    private var dismissBubble:DispatchWorkItem?
    override init(){model=CompanionModel(profile:RamWardenProfile());super.init();observeChat()}
    init(profile:WardenProfile){model=CompanionModel(profile:profile);super.init();observeChat()}
    private func observeChat(){
        chatObserver=model.$turns.sink{[weak self] _ in DispatchQueue.main.async{self?.resizeAssistant()}}
    }
    func showAssistant(anchor:NSWindow?,settings:@escaping ()->Void) {
        statusBubble?.orderOut(nil)
        if assistantPanel == nil {
            let p=ChatPanel(contentRect:NSRect(x:0,y:0,width:304,height:240),styleMask:[.borderless],backing:.buffered,defer:false)
            p.isOpaque=false;p.backgroundColor = .clear;p.hasShadow=true;p.level = .floating;p.hidesOnDeactivate=false
            p.isFloatingPanel=true;p.becomesKeyOnlyIfNeeded=false;p.collectionBehavior=[.canJoinAllSpaces,.fullScreenAuxiliary]
            p.contentView=NSHostingView(rootView:AssistantView(model:model,dismiss:{[weak self] in self?.closeAssistant()},settings:settings))
            assistantPanel=p
        }
        resizeAssistant()
        if let p=assistantPanel,let anchor=anchor {
            p.setFrame(PetSizing.attachedFrame(pet:anchor.frame,size:p.frame.size,bounds:anchor.screen?.visibleFrame ?? NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1440,height:900),preferAbove:false),display:true)
        } else {assistantPanel?.center()}
        NSApp.activate(ignoringOtherApps:true);assistantPanel?.makeKeyAndOrderFront(nil)
        DispatchQueue.main.async { NotificationCenter.default.post(name:Notification.Name("WardenChatOpened"),object:self.model.profile.id) }
    }
    private func resizeAssistant(){guard let p=assistantPanel else{return};p.setContentSize(NSSize(width:304,height:model.chatHeight))}
    func showStatusBubble(_ text:String,anchor:NSWindow?,open:@escaping ()->Void) {
        guard let anchor=anchor,assistantPanel?.isVisible != true else{return}
        dismissBubble?.cancel()
        let p=statusBubble ?? BubblePanel(contentRect:.zero,styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
        p.isOpaque=false;p.backgroundColor = .clear;p.hasShadow=true;p.level = .floating;p.hidesOnDeactivate=false;p.collectionBehavior=[.canJoinAllSpaces,.fullScreenAuxiliary]
        p.contentView=NSHostingView(rootView:Button(action:open){Text(String(text.prefix(240))).font(.system(size:12)).multilineTextAlignment(.leading).fixedSize(horizontal:false,vertical:true).padding(12).frame(width:260).modifier(ChatGlass())}.buttonStyle(.plain))
        p.contentView?.layoutSubtreeIfNeeded();let size=p.contentView?.fittingSize ?? NSSize(width:260,height:80)
        let bounds=anchor.screen?.visibleFrame ?? NSScreen.main?.visibleFrame ?? NSRect(x:0,y:0,width:1440,height:900)
        p.setFrame(PetSizing.attachedFrame(pet:anchor.frame,size:size,bounds:bounds,preferAbove:true),display:true)
        statusBubble=p;p.orderFrontRegardless()
        let work=DispatchWorkItem{[weak self] in self?.statusBubble?.orderOut(nil)};dismissBubble=work;DispatchQueue.main.asyncAfter(deadline:.now()+12,execute:work)
    }
    func closeAssistant(){model.cancelConversation();CompanionSpeech.shared.stop(model.profile.id);assistantPanel?.orderOut(nil)}
    func didMove(anchor:NSWindow?) {
        statusBubble?.orderOut(nil)
        guard let anchor=anchor,let chat=assistantPanel,chat.isVisible,let bounds=anchor.screen?.visibleFrame else{return}
        chat.setFrame(PetSizing.attachedFrame(pet:anchor.frame,size:chat.frame.size,bounds:bounds,preferAbove:false),display:true)
    }
    func hideSharedPanels(){model.companionVisible=false;model.cancelConversation();dismissBubble?.cancel();assistantPanel?.orderOut(nil);statusBubble?.orderOut(nil);CompanionSpeech.shared.stop(model.profile.id)}
}
