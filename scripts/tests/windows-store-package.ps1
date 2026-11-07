# Compiler/PowerShell syntax regression only. These synthetic files are NOT a product.
[CmdletBinding()]
param([string]$Compiler = '')
$ErrorActionPreference='Stop'
$Root=Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
foreach($name in @('scripts\install.ps1','scripts\package-windows.ps1')) {
 $tokens=$null;$errors=$null
 [Management.Automation.Language.Parser]::ParseFile((Join-Path $Root $name),[ref]$tokens,[ref]$errors)|Out-Null
 if($errors.Count){throw ($errors|Out-String)}
}
if(-not $Compiler){
 foreach($path in @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe","$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe")){if(Test-Path $path){$Compiler=$path;break}}
}
if(-not $Compiler){throw 'Official Inno Setup compiler required'}
$temp=Join-Path ([IO.Path]::GetTempPath()) ('warden-installer-syntax-'+[Guid]::NewGuid())
New-Item -ItemType Directory -Path $temp|Out-Null
$manifest=Join-Path $Root 'build\chrome\manifest.json'
$createdManifest=-not(Test-Path $manifest)
try {
 foreach($name in @('ramwarden-daemon.exe','ramwarden-service.exe','ramwarden-debloat.exe','ramwarden-tray.exe','ramwarden.exe','ramwarden-usage-sync.exe','ramwarden-launcher.exe')){Copy-Item "$env:SystemRoot\System32\cmd.exe" (Join-Path $temp $name)}
 if($createdManifest){New-Item -ItemType Directory -Force (Split-Path -Parent $manifest)|Out-Null;Copy-Item (Join-Path $Root 'extension\manifest.json') $manifest}
 & $Compiler /Q "/DPayload=$temp" '/DReleaseVersion=0.0.1' "/DOutputRoot=$temp" '/DArtifactSuffix=-syntax-fixture-DO-NOT-INSTALL' (Join-Path $Root 'packaging\windows\RamWarden.iss')
 if($LASTEXITCODE -ne 0){throw "Inno syntax compilation failed: $LASTEXITCODE"}
 Write-Output 'PowerShell parsing and Inno compiler fixture passed. No application was installed.'
} finally {
 Remove-Item $temp -Recurse -Force
 if($createdManifest){Remove-Item $manifest -Force}
}
