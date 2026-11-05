# Administrator repair for the existing capture installation, without changing rules/config.
param([switch]$Restart)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'windows-eve-access.ps1')
$directory='D:\ThreatWarden\Suricata\log'
$backup=Join-Path $env:ProgramData ('ThreatWarden\backups\eve-acl-'+(Get-Date -Format 'yyyyMMddTHHmmss'))
New-Item -ItemType Directory -Path $backup -Force | Out-Null
& icacls.exe $directory /save (Join-Path $backup 'before.acl') /t /c | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'ACL backup failed; no repair attempted' }
Repair-ThreatWardenEveWriterAccess $directory
if ($Restart) { Restart-Service -Name Suricata -ErrorAction Stop }
Write-Output "EVE writer access restored. ACL backup: $backup"
