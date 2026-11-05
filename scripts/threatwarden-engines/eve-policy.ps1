# Fixed, minimal EVE output for the existing Windows sensor installation.
# Kept separate so it can be validated without admin rights or restarting capture.
function Set-ThreatWardenEve([string]$Text) {
    $pattern = '(?m)^- eve-log:\r?\n(?:^[ \t]+[^\r\n]*\r?\n|^\r?\n)*'
    if ([regex]::Matches($Text, $pattern).Count -ne 1) { throw 'Expected one EVE output; configuration retained' }
    $eve = @'
- eve-log:
    enabled: true
    filetype: regular
    filename: D:\ThreatWarden\Suricata\log\eve-%Y-%m-%d.json
    rotate-interval: day
    buffer-size: 0
    metadata: false
    types:
    - alert:
        payload: false
        payload-printable: false
        packet: false
        metadata: false
        http-body: false
        http-body-printable: false
        websocket-payload: false
        websocket-payload-printable: false
        tagged-packets: false
    - flow
    - stats:
        totals: true
        threads: false
        deltas: false
'@
    return [regex]::Replace($Text, $pattern, [Text.RegularExpressions.MatchEvaluator]{ param($match) $eve + "`n" })
}
