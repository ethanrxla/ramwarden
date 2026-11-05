# Read-only artifact gate. Passing these checks is not Store certification.
param(
    [Parameter(Mandatory=$true)][string]$PayloadDirectory,
    [string]$InstallerPath,
    [string]$VersionedDownloadUrl,
    [string]$ExpectedPublisher,
    [Parameter(Mandatory=$true)][string]$ReportPath
)
$ErrorActionPreference='Stop'
$root=(Resolve-Path -LiteralPath $PayloadDirectory).Path
$checks=New-Object 'System.Collections.Generic.List[object]'
function Add-Check([string]$Name,[bool]$Pass,[string]$Evidence) {$checks.Add([pscustomobject]@{name=$Name;pass=$Pass;evidence=$Evidence})}
$files=@(Get-ChildItem -LiteralPath $root -File -Recurse)
$private=@($files | Where-Object {$_.Name -match '^(\.env.*|root\.env|secrets\.toml|device-token|companion-chat-(ram|threat)\.json|routing\.json|config-with-chat\.toml)$' -or $_.Extension -in @('.pfx','.key')})
Add-Check 'No private developer credentials in payload' ($private.Count -eq 0) (($private | ForEach-Object {$_.Name}) -join ', ')
$pe=@($files | Where-Object {$_.Extension -in @('.exe','.dll','.sys')})
Add-Check 'PE payload present' ($pe.Count -gt 0) ($pe.Count.ToString()+' PE files')
foreach($file in $pe) {
    $signature=Get-AuthenticodeSignature -LiteralPath $file.FullName
    $valid=$signature.Status -eq 'Valid'
    if ($ExpectedPublisher) {$valid=$valid -and $signature.SignerCertificate.Subject -eq $ExpectedPublisher}
    Add-Check ('Trusted signature: '+$file.Name) $valid ([string]$signature.Status)
}
$installerExists=$InstallerPath -and (Test-Path -LiteralPath $InstallerPath -PathType Leaf)
Add-Check 'Standalone installer supplied' ([bool]$installerExists) 'A tray executable alone is not an installer; offline/silent behavior requires clean-machine acceptance.'
if ($installerExists) {
    $installer=Get-Item -LiteralPath $InstallerPath
    $signature=Get-AuthenticodeSignature -LiteralPath $installer.FullName
    Add-Check 'Installer is EXE/MSI' ($installer.Extension -in @('.exe','.msi')) $installer.Extension
    $valid=$signature.Status -eq 'Valid'
    if ($ExpectedPublisher) {$valid=$valid -and $signature.SignerCertificate.Subject -eq $ExpectedPublisher}
    Add-Check 'Trusted installer signature' $valid ([string]$signature.Status)
}
$url=$null
$urlValid=[Uri]::TryCreate($VersionedDownloadUrl,[UriKind]::Absolute,[ref]$url)
$urlValid=$urlValid -and $url.Scheme -eq 'https' -and -not $url.IsLoopback -and -not $url.Query -and $url.AbsolutePath -match '/v?\d+\.\d+\.\d+/[^/]+\.(exe|msi)$'
Add-Check 'Versioned HTTPS installer URL shape' ([bool]$urlValid) 'Must separately verify anonymous download, immutable bytes and SHA256; this check makes no network request.'
Add-Check 'Publisher identity specified' (-not [string]::IsNullOrWhiteSpace($ExpectedPublisher)) 'Must match the trusted signing certificate and Partner Center identity.'
$passed=@($checks | Where-Object {-not $_.pass}).Count -eq 0
$report=[ordered]@{checked_at_utc=[DateTime]::UtcNow.ToString('o');status=if($passed){'manual_acceptance_required'}else{'artifact_checks_failed'};store_ready=$false;checks=$checks.ToArray();manual_gates=@('Silent standalone install without downloads','Clean-machine sign-in with no developer credentials or tailnet','Upgrade, rollback, reboot, uninstall and data removal','Account-isolated hosted model routing and quota enforcement','Sensor setup and benign coverage tests','Partner Center identity, privacy, age ratings, listing and certification')}
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReportPath -Encoding utf8
Write-Output ($report | ConvertTo-Json -Depth 6 -Compress)
if (-not $passed) {exit 1}

