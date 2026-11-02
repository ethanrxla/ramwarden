param([string]$ScanPath = (Join-Path $env:USERPROFILE 'Downloads'))
$ErrorActionPreference='Stop'
$base='C:\ProgramData\ThreatWarden'
Copy-Item (Join-Path $PSScriptRoot 'scan.ps1') ($base+'\scan.ps1') -Force
& icacls.exe $base /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' 'Administrators:(OI)(CI)F' /T | Out-Null
& icacls.exe D:\ThreatWarden /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' 'Administrators:(OI)(CI)F' /T | Out-Null
$principal=New-ScheduledTaskPrincipal -UserId SYSTEM -LogonType ServiceAccount
$settings=New-ScheduledTaskSettingsSet -StartWhenAvailable -MultipleInstances IgnoreNew -Priority 7 -ExecutionTimeLimit (New-TimeSpan -Hours 4)
$action=New-ScheduledTaskAction -Execute D:\ThreatWarden\ClamAV\freshclam.exe -Argument '--config-file=D:\ThreatWarden\ClamAV\freshclam.conf --quiet'
Register-ScheduledTask -TaskName ThreatWardenSignatureUpdate -Action $action -Trigger (New-ScheduledTaskTrigger -Daily -At 3am) -Principal $principal -Settings $settings -Force | Out-Null
$action=New-ScheduledTaskAction -Execute powershell.exe -Argument ('-NoProfile -File C:\ProgramData\ThreatWarden\scan.ps1 -Scheduled -Paths "' + $ScanPath + '"')
Register-ScheduledTask -TaskName ThreatWardenScan -Action $action -Trigger (New-ScheduledTaskTrigger -Daily -At 4am) -Principal $principal -Settings $settings -Force | Out-Null
Get-ScheduledTask -TaskName ThreatWardenScan,ThreatWardenSignatureUpdate | Select TaskName,State
