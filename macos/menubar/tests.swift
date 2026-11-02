import AppKit

let stale: [String: Any] = [
    "unlocked": false,
    "report": ["summary": "stale", "alerts": [["description": "stale"]],
               "agents": [["name": "other"]], "problems": ["stale"]]
]
let locked = threatPane(from: stale)
precondition(locked.level == "locked" && locked.upgrade != nil && !locked.turnOn)
precondition(locked.summary == nil && locked.alerts.isEmpty && locked.agents.isEmpty && locked.problems.isEmpty)
precondition(locked.engines.count == 4 && locked.engines.allSatisfy { $0.state == "locked" })

let off: [String: Any] = ["unlocked": true, "enabled": true, "status": ["level": "off"],
                          "report": ["local": ["installed": false]]]
precondition(threatPane(from: off).turnOn)
var installed = off
installed["report"] = ["local": ["installed": true]]
precondition(!threatPane(from: installed).turnOn)

var many = off
many["report"] = ["alerts": Array(repeating: ["groups": ["clamav"]], count: 10)]
let pane = threatPane(from: many)
precondition(pane.alerts.count == 8)
precondition(pane.engines[1].detail == "10 alerts recently")

let accepted = Data(#"{"started":true}"#.utf8)
let refused = Data(#"{"error":"Sign in first"}"#.utf8)
precondition(setupMessage(data: accepted, status: 202, error: nil).contains("is setting up"))
precondition(setupMessage(data: refused, status: 403, error: nil).contains("Sign in first"))
precondition(setupMessage(data: accepted, status: 500, error: nil).contains("did not start"))
precondition(setupMessage(data: nil, status: 200, error: nil).contains("did not start"))
precondition(setupMessage(data: nil, status: nil, error: URLError(.timedOut)).contains("did not start"))
print("Swift pane and setup tests passed")
