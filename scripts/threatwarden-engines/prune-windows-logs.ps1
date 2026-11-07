# Retain seven days of rotated flow logs; leave current/open files untouched.
param([string]$Root='D:\ThreatWarden\Suricata')
$ErrorActionPreference='Stop'
Get-ChildItem (Join-Path $Root 'log') -Filter 'eve-*.json' -File |
    Where-Object {$_.LastWriteTime -lt (Get-Date).AddDays(-7)} |
    Remove-Item -Force
