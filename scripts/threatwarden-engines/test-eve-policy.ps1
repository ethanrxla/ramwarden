$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'eve-policy.ps1')
$original=@'
outputs:
- eve-log:
    enabled: true
    types:
    - alert
    - flow
    xff:
      enabled: false
- other-output:
    enabled: false
pcap:
- interface: default
'@
foreach ($inputText in @($original, $original.Replace("`n","`r`n"))) {
    $updated = Set-ThreatWardenEve $inputText
    if ($updated -notmatch '(?m)^pcap:' -or $updated -notmatch '(?m)^- other-output:') { throw 'Changed an unrelated section' }
    if ($updated -notmatch 'metadata: false' -or $updated -notmatch 'tagged-packets: false') { throw 'Missing privacy guard' }
    if ($updated -match '(?m)^        (http|tls):') { throw 'Deprecated metadata flags' }
    if ((Set-ThreatWardenEve $updated) -ne $updated) { throw 'Not idempotent' }
}
$refused=$false
try { Set-ThreatWardenEve 'outputs: []' | Out-Null } catch { $refused=$true }
if (-not $refused) { throw 'Accepted unexpected layout' }
Write-Output 'EVE policy tests passed (LF/CRLF, privacy, unrelated sections, replay, unexpected input).'
