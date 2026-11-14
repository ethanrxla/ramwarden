# Restore read-only endpoint evidence access without changing sensor services.
param(
    [Parameter(Mandatory=$true)][string]$ReaderSid,
    [string[]]$EvidencePaths=@('C:\ProgramData\ThreatWarden\logs','D:\ThreatWarden\Suricata\log','D:\ThreatWarden\ClamAV\database')
)
$ErrorActionPreference='Stop'
$identity=New-Object Security.Principal.SecurityIdentifier($ReaderSid)
foreach ($path in $EvidencePaths) {
    if (-not (Test-Path -LiteralPath $path -PathType Container)) { throw "Evidence directory missing: $path" }
    $items=@(Get-Item -LiteralPath $path)+@(Get-ChildItem -LiteralPath $path -Recurse -Force)
    foreach ($item in $items) {
        # Explicit file rights also cover old files with protected inheritance.
        $rights=if($item.PSIsContainer){'(OI)(CI)RX'}else{'RX'}
        & icacls.exe $item.FullName /grant "*$($identity.Value):$rights" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Could not grant evidence read access: $($item.FullName)" }
    }
    Write-Output ("Read-only access configured: $path ($($items.Count) entries)")
}
