<# Offline EXE build. Unsigned candidates cannot pass the Store validation gate. #>
[CmdletBinding()]
param(
  [string]$Source = '',
  [string]$Output = '',
  [string]$Compiler = '',
  [string]$SigningCommand = '',
  [switch]$AllowUnsignedCandidate
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
if (-not $Source) { $Source = Join-Path $Root 'target\release' }
if (-not $Output) { $Output = Join-Path $Root 'dist\microsoft-store' }
$Source = (Resolve-Path $Source).Path
$versionLine = Get-Content (Join-Path $Root 'Cargo.toml') | Where-Object { $_ -match '^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"' } | Select-Object -First 1
if (-not $versionLine -or $versionLine -notmatch '"([0-9]+\.[0-9]+\.[0-9]+)"') { throw 'No package version' }
$Version = $Matches[1]
$Names = @('ramwarden-daemon.exe','ramwarden-service.exe','ramwarden-debloat.exe','ramwarden-tray.exe','ramwarden.exe','ramwarden-usage-sync.exe','ramwarden-launcher.exe')
$inventory = @()
foreach ($name in $Names) {
  $file = Join-Path $Source $name
  if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing required payload: $name" }
  $signature = Get-AuthenticodeSignature -LiteralPath $file
  if (-not $AllowUnsignedCandidate -and $signature.Status -ne 'Valid') { throw "Store build requires a trusted signature: $name" }
  $inventory += [ordered]@{ name=$name; sha256=(Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant(); signature=[string]$signature.Status }
}
# Both compiled trust roots are mandatory even for development packages.
foreach ($entry in @(@('ramwarden-daemon.exe','RAMWARDEN_ENTITLEMENT_PUBLIC_KEY'),@('ramwarden-launcher.exe','RAMWARDEN_UPDATE_PUBLIC_KEY'))) {
  $key = [Environment]::GetEnvironmentVariable($entry[1])
  if (-not $key) { throw "Missing build verification setting $($entry[1])" }
  $bytes = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $Source $entry[0])))
  if (-not $bytes.Contains($key)) { throw "Rebuild $($entry[0]) with its release verification key" }
}
if (-not $AllowUnsignedCandidate -and -not $SigningCommand) { throw 'Production build requires SigningCommand for installer and uninstaller' }
if (-not (Test-Path (Join-Path $Root 'build\chrome\manifest.json'))) { throw 'Build the browser package first with scripts/build-ext.sh' }
if (-not $Compiler) {
  $found = Get-Command ISCC.exe -ErrorAction SilentlyContinue
  if ($found) { $Compiler = $found.Source }
  foreach ($candidate in @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 7\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 7\ISCC.exe")) {
    if (-not $Compiler -and (Test-Path $candidate)) { $Compiler=$candidate }
  }
}
if (-not $Compiler -or -not (Test-Path $Compiler)) { throw 'Install the official Inno Setup compiler before building the Windows installer' }
New-Item -ItemType Directory -Force $Output | Out-Null
$Output = (Resolve-Path $Output).Path
$suffix = if ($AllowUnsignedCandidate) { '-unsigned-candidate' } else { '' }
$arguments = @('/Qp', "/DPayload=$Source", "/DReleaseVersion=$Version", "/DOutputRoot=$Output", "/DArtifactSuffix=$suffix")
if ($SigningCommand) { $arguments += "/DSigningCommand=1"; $arguments += "/Srelease=$SigningCommand" }
& $Compiler @arguments (Join-Path $Root 'packaging\windows\RamWarden.iss')
if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed: $LASTEXITCODE" }
$artifact = Join-Path $Output "RamWarden-Setup-$Version-x64$suffix.exe"
if (-not (Test-Path $artifact)) { throw 'Compiler did not produce the expected installer' }
$signature = Get-AuthenticodeSignature $artifact
if (-not $AllowUnsignedCandidate -and $signature.Status -ne 'Valid') { throw 'Installer is not validly signed; do not submit' }
$hash=(Get-FileHash -Algorithm SHA256 $artifact).Hash.ToLowerInvariant()
[ordered]@{ schema=1; version=$Version; unsignedCandidate=[bool]$AllowUnsignedCandidate; storeReady=$false; installer=(Split-Path -Leaf $artifact); sha256=$hash; signature=[string]$signature.Status; payload=$inventory; remaining=@('Partner Center identity','immutable public HTTPS URL','clean-machine silent install, upgrade and uninstall','public privacy/support URLs','certification') } | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 (Join-Path $Output 'submission-report.json')
"$hash  $(Split-Path -Leaf $artifact)" | Set-Content -Encoding ascii "$artifact.sha256"
Write-Output "Built $artifact"
Write-Output 'Build complete; clean-machine and publisher verification are still required before Store submission.'
