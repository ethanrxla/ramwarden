# Upgrade the existing, opted-in sensors. Requires an elevated PowerShell.
param([string]$ScanPath = (Join-Path $env:USERPROFILE 'Downloads'))
$ErrorActionPreference='Stop'
$root='D:\ThreatWarden\Suricata'
$config=Join-Path $root 'suricata.yaml'
$backup=$config+'.backup-'+[DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$candidate=$config+'.candidate'
$text=[IO.File]::ReadAllText($config)
. (Join-Path $PSScriptRoot 'eve-policy.ps1')
$text=Set-ThreatWardenEve $text
[IO.File]::WriteAllText($candidate,$text,(New-Object Text.UTF8Encoding($false)))
& (Join-Path $root 'suricata.exe') -T -c $candidate -l (Join-Path $root 'log') *> (Join-Path $root 'log\evidence-validation.log')
if ($LASTEXITCODE -ne 0) { throw 'Candidate failed validation; running configuration retained' }
[IO.File]::Replace($candidate,$config,$backup)
. (Join-Path $PSScriptRoot 'windows-capture-clock.ps1')
try {
    Set-SuricataCaptureClock
    Restart-Service Suricata -ErrorAction Stop
} catch { Copy-Item $backup $config -Force; Restart-Service Suricata; throw }
& (Join-Path $PSScriptRoot 'schedule-windows.ps1') -ScanPath $ScanPath
# Expose evidence read-only to the current interactive user. No script write access.
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
& (Join-Path $PSScriptRoot 'repair-windows-evidence-access.ps1') -ReaderSid $sid
Write-Output 'Sensor evidence and daily scan/update tasks configured. Defender unchanged.'
