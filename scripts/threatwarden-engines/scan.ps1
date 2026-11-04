# Scan-only ClamAV bridge. Scheduled runs defer unless RamWarden reports clear memory.
param([switch]$Scheduled, [Parameter(Mandatory=$true)][string[]]$Paths)
$ErrorActionPreference = 'Stop'
function Write-ScanEvent($event) {
    $event.time = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $path = Join-Path $logDir 'clamav.json'
    $stream = [System.IO.File]::Open($path, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)
    $writer = New-Object System.IO.StreamWriter($stream, (New-Object System.Text.UTF8Encoding($false)))
    try { $writer.WriteLine(($event | ConvertTo-Json -Compress)) } finally { $writer.Dispose() }
}
$mutex = New-Object System.Threading.Mutex($false, 'Global\ThreatWardenClamScan')
if (-not $mutex.WaitOne(0)) { exit 0 }
try {
    $logDir = Join-Path $env:ProgramData 'ThreatWarden\logs'
    New-Item -ItemType Directory -Force $logDir | Out-Null
    if ($Scheduled) {
        try { $state = Invoke-RestMethod 'http://127.0.0.1:7823/psi' -TimeoutSec 3 }
        catch { Write-ScanEvent @{engine='threatwarden_clamav';event='scan_deferred';reason='memory_status_unavailable'}; exit 0 }
        if ($state.rung -ne 'release') { Write-ScanEvent @{engine='threatwarden_clamav';event='scan_deferred';reason='memory_pressure'}; exit 0 }
    }
    $scanner = Join-Path $env:ProgramFiles 'ClamAV\clamscan.exe'
    if (-not (Test-Path $scanner)) { $scanner = 'D:\ThreatWarden\ClamAV\clamscan.exe' }
    $database = Join-Path (Split-Path $scanner) 'database'
    $logDir = Join-Path $env:ProgramData 'ThreatWarden\logs'
    New-Item -ItemType Directory -Force $logDir | Out-Null
    Write-ScanEvent @{engine='threatwarden_clamav';event='scan_started';scope='selected_paths';path_count=$Paths.Count}
    & $scanner --database=$database --recursive --infected -- @Paths 2>&1 | ForEach-Object {
        if ("$_".EndsWith(' FOUND')) {
            $detail = "$_"
            Write-ScanEvent @{engine='threatwarden_clamav';event='malware_found';detail=$detail.Substring(0,[Math]::Min(4096,$detail.Length))}
        }
    }
    $scanExit = $LASTEXITCODE
    Write-ScanEvent @{engine='threatwarden_clamav';event='scan_finished';exit_code=$scanExit}
    exit $scanExit
} catch {
    Write-ScanEvent @{engine='threatwarden_clamav';event='scan_finished';exit_code=2}
    throw
} finally { $mutex.ReleaseMutex(); $mutex.Dispose() }
