<#
.SYNOPSIS
  Build the Windows release asset: dist\ramwarden-windows-x86_64.exe

.DESCRIPTION
  Builds the binaries, stages them with install.ps1 and the starter config,
  and compiles packaging\windows\ramwarden.iss with Inno Setup 6.

  Code signing happens only when a certificate is given, so this also runs on
  a machine without one (the result then shows "Unknown publisher" in UAC):
    $env:WINDOWS_CERT_PFX       path to the .pfx
    $env:WINDOWS_CERT_PASSWORD  its password
#>
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Version = (Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern '^version\s*=\s*"(.+)"' |
            Select-Object -First 1).Matches[0].Groups[1].Value
$Dist = Join-Path $Root 'dist'
$Payload = Join-Path $env:TEMP "ramwarden-payload-$Version"

cargo build --release -p ramwarden-daemon -p ramwarden-service -p ramwarden-debloat -p ramwarden-launcher
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
cargo build --release -p ramwarden-ui --features tray --bin ramwarden-tray
if ($LASTEXITCODE -ne 0) { throw 'tray build failed' }

if (Test-Path $Payload) { Remove-Item -Recurse -Force $Payload }
New-Item -ItemType Directory -Force $Payload, $Dist | Out-Null
foreach ($exe in 'ramwarden-daemon', 'ramwarden-service', 'ramwarden-debloat', 'ramwarden-launcher', 'ramwarden-tray') {
  Copy-Item (Join-Path $Root "target\release\$exe.exe") $Payload
}
Copy-Item (Join-Path $Root 'scripts\install.ps1') $Payload
Copy-Item (Join-Path $Root 'ramwarden.toml') $Payload

function Sign-File($Path) {
  if (-not $env:WINDOWS_CERT_PFX) { return }
  $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
              Sort-Object FullName -Descending | Select-Object -First 1
  & $signtool.FullName sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com `
      /f $env:WINDOWS_CERT_PFX /p $env:WINDOWS_CERT_PASSWORD $Path
  if ($LASTEXITCODE -ne 0) { throw "signing $Path failed" }
}
Get-ChildItem $Payload -Filter *.exe | ForEach-Object { Sign-File $_.FullName }

$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
if (-not (Test-Path $iscc)) { throw 'Inno Setup 6 not found: choco install innosetup' }
& $iscc "/DAppVersion=$Version" "/DPayload=$Payload" "/DOutDir=$Dist" (Join-Path $Root 'packaging\windows\ramwarden.iss')
if ($LASTEXITCODE -ne 0) { throw 'iscc failed' }

$Setup = Join-Path $Dist "ramwarden-windows-x86_64.exe"
Sign-File $Setup
Write-Host "built $Setup"
