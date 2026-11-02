# Scan-only ClamAV bridge. Scheduled runs defer unless RamWarden reports clear memory.
param([switch]$Scheduled, [Parameter(Mandatory=$true)][string[]]$Paths)
$ErrorActionPreference = 'Stop'
$mutex = New-Object System.Threading.Mutex($false, 'Global\ThreatWardenClamScan')
if (-not $mutex.WaitOne(0)) { exit 0 }
try {
    if ($Scheduled) {
        try { $state = Invoke-RestMethod 'http://127.0.0.1:7823/psi' -TimeoutSec 3 }
        catch { Write-Output 'Deferred: memory status unavailable'; exit 0 }
        if ($state.rung -ne 'release') { Write-Output 'Deferred: memory pressure'; exit 0 }
    }
    $scanner = Join-Path $env:ProgramFiles 'ClamAV\clamscan.exe'
    if (-not (Test-Path $scanner)) { $scanner = 'D:\ThreatWarden\ClamAV\clamscan.exe' }
    $database = Join-Path (Split-Path $scanner) 'database'
    $logDir = Join-Path $env:ProgramData 'ThreatWarden\logs'
    New-Item -ItemType Directory -Force $logDir | Out-Null
    $output = & $scanner --database=$database --recursive --infected -- @Paths 2>&1
    $scanExit = $LASTEXITCODE
    foreach ($line in $output) {
        if ("$line".EndsWith(' FOUND')) {
            @{engine='threatwarden_clamav';event='malware_found';detail="$line"} |
                ConvertTo-Json -Compress | Add-Content (Join-Path $logDir 'clamav.json') -Encoding UTF8
        }
    }
    @{engine='threatwarden_clamav';event='scan_finished';exit_code=$scanExit} |
        ConvertTo-Json -Compress | Add-Content (Join-Path $logDir 'clamav.json') -Encoding UTF8
    $output | Write-Output
    exit $scanExit
} finally { $mutex.ReleaseMutex(); $mutex.Dispose() }
