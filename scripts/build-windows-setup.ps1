# Compiles an offline preview from an explicit six-file payload, never a developer profile.
param(
 [Parameter(Mandatory=$true)][string]$PayloadDirectory,
 [Parameter(Mandatory=$true)][ValidatePattern('^\d+\.\d+\.\d+$')][string]$Version,
 [Parameter(Mandatory=$true)][string]$CompilerPath,
 [Parameter(Mandatory=$true)][string]$OutputDirectory
)
$ErrorActionPreference='Stop'
$payload=(Resolve-Path -LiteralPath $PayloadDirectory).Path
$compiler=(Resolve-Path -LiteralPath $CompilerPath).Path
$names=@('ramwarden-tray.exe','ramwarden-daemon.exe','ramwarden-service.exe','ramwarden-usage-sync.exe','ramwarden-gpu.exe','ramwarden-debloat.exe')
foreach($name in $names) {if(-not (Test-Path -LiteralPath (Join-Path $payload $name) -PathType Leaf)) {throw "Missing payload: $name"}}
# Stage only the public binaries. Never recursively copy a build directory or user profile.
$stage=Join-Path ([IO.Path]::GetTempPath()) ('ramwarden-setup-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
 foreach($name in $names) {Copy-Item -LiteralPath (Join-Path $payload $name) -Destination (Join-Path $stage $name)}
 $output=[IO.Path]::GetFullPath($OutputDirectory)
 New-Item -ItemType Directory -Force -Path $output | Out-Null
 $script=Join-Path $PSScriptRoot '../packaging/windows/ramwarden.iss'
 & $compiler "/DPayloadDir=$stage" "/DAppVersion=$Version" "/DOutputDir=$output" $script
 if($LASTEXITCODE -ne 0) {throw 'Setup compiler failed'}
 $installer=Join-Path $output "RamWarden-$Version-windows-x64-setup.exe"
 Get-FileHash -LiteralPath $installer -Algorithm SHA256
 Write-Warning 'Developer preview only. Signing, upgrade/service lifecycle and clean-machine acceptance remain required before distribution.'
} finally {
 # This is the exact fresh staging directory created above, never a user-supplied directory.
 $resolved=[IO.Path]::GetFullPath($stage)
 if(([IO.Path]::GetDirectoryName($resolved) -eq [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')) -and ([IO.Path]::GetFileName($resolved) -match '^ramwarden-setup-[a-f0-9]{32}$')) {
  Remove-Item -LiteralPath $resolved -Recurse -Force
 }
}
