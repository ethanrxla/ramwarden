import SwiftUI
import AppKit

// Guest pairing is independent of ScholarCal calendar authorization.
@MainActor final class AgentPairing: ObservableObject {
    @Published var busy=false
    @Published var connected=false
    @Published var status="Connect to create a pairing invitation. No ScholarCal account is required."
    @Published var invitation=""
    @Published var peers:[[String:Any]]=[]
    private func request(_ path:String,body:[String:Any]?=nil) async throws->[String:Any] {
        var request=URLRequest(url:URL(string:"http://127.0.0.1:7823/agent-network/"+path)!)
        request.timeoutInterval=40
        if let body=body {request.httpMethod="POST";request.setValue("application/json",forHTTPHeaderField:"Content-Type");request.httpBody=try JSONSerialization.data(withJSONObject:body)}
        let(data,response)=try await URLSession.shared.data(for:request)
        guard data.count<=524288,let result=try JSONSerialization.jsonObject(with:data) as? [String:Any] else {throw PairingError.unavailable}
        guard (response as? HTTPURLResponse)?.statusCode==200 else {throw PairingError.message(result["error"] as? String ?? "Hosted pairing is unavailable. Try again later.")}
        return result
    }
    func connect() async {await perform {
        let result=try await self.request("connect",body:["name":Host.current().localizedName ?? "My Mac"])
        self.peers=result["pairs"] as? [[String:Any]] ?? [];self.connected=true
        self.status="Connected. Invitations expire after ten minutes and can be used once."
    }}
    func create() async {await perform {
        let result=try await self.request("invites",body:[:])
        guard let invitation=result["invitation"] as? String,invitation.hasPrefix("agn1.did:agn:") else{throw PairingError.unavailable}
        self.invitation=invitation;self.status="Share only with your intended agent. Review its fingerprint before approving. This invitation expires in ten minutes."
    }}
    func accept(_ invitation:String) async {await perform {
        _=try await self.request("accept",body:["invitation":invitation.trimmingCharacters(in:.whitespacesAndNewlines)])
        self.status="Invitation accepted. The other device must confirm your identity before messaging."
        self.peers=try await self.request("pairs")["pairs"] as? [[String:Any]] ?? []
    }}
    func refresh() async {await perform {self.peers=try await self.request("pairs")["pairs"] as? [[String:Any]] ?? []}}
    func decide(_ id:String,approve:Bool) async {await perform {
        guard UUID(uuidString:id) != nil else{throw PairingError.unavailable}
        _=try await self.request("pairs/\(id)/"+(approve ? "confirm":"revoke"),body:[:])
        self.peers=try await self.request("pairs")["pairs"] as? [[String:Any]] ?? []
        self.status=approve ? "Peer approved. Pairing does not authorize file changes or commands.":"Peer access revoked."
    }}
    private func perform(_ action:() async throws->Void) async {
        guard !busy else{return};busy=true;let started=Date();var success=false
        SetupEvents.record(.pairingOperationStarted,step:5)
        defer{busy=false;SetupEvents.record(.pairingOperationFinished,step:5,result:success,duration:Date().timeIntervalSince(started))}
        do {try await action();success=true} catch let error as PairingError {status=error.localizedDescription} catch {status="The local companion or hosted pairing service could not be reached. Install the latest app and try again."}
    }
    enum PairingError:LocalizedError {
        case unavailable,message(String)
        var errorDescription:String? {switch self {case .unavailable:return "Pairing is unavailable. No invitation was created.";case .message(let text):return text}}
    }
}
struct AgentPairingView:View {
    @StateObject private var model=AgentPairing()
    @State private var incoming=""
    var body:some View {
        VStack(alignment:.leading,spacing:10) {
            Text("Agent pairing").font(.headline)
            Text(model.status).font(.caption).fixedSize(horizontal:false,vertical:true)
            HStack {
                Button("Connect agent network"){Task{await model.connect()}}
                if model.connected {
                    Button("Create invitation"){Task{await model.create()}}
                    Button("Refresh peers"){Task{await model.refresh()}}
                }
            }
            if !model.invitation.isEmpty {
                Text(model.invitation).font(.system(.caption,design:.monospaced)).textSelection(.enabled).fixedSize(horizontal:false,vertical:true)
                Button("Copy pairing invitation"){NSPasteboard.general.clearContents();NSPasteboard.general.setString(model.invitation,forType:.string)}
            }
            if model.connected {
                TextField("Invitation from another agent",text:$incoming)
                Button("Accept invitation"){Task{await model.accept(incoming);incoming=""}}.disabled(incoming.isEmpty)
                ForEach(model.peers.indices,id:\.self) { index in
                    let pair=model.peers[index]
                    let peer=pair["peer"] as? [String:Any] ?? [:]
                    let id=pair["id"] as? String ?? ""
                    VStack(alignment:.leading) {
                        Text(peer["displayName"] as? String ?? "Peer").bold()
                        Text(peer["did"] as? String ?? "Unknown fingerprint").font(.system(.caption,design:.monospaced)).textSelection(.enabled)
                        Text(pair["state"] as? String ?? "Unknown").font(.caption)
                        HStack {
                            if pair["needsApproval"] as? Bool == true {Button("Confirm this fingerprint"){Task{await model.decide(id,approve:true)}}}
                            Button("Revoke"){Task{await model.decide(id,approve:false)}}
                        }
                    }
                }
            }
        }.disabled(model.busy)
    }
}
