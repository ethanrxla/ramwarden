# RamWarden one-line installer for Windows 11.
#
#   irm https://raw.githubusercontent.com/ethanrxla/ramwarden/main/web/install.ps1 | iex
#
# Downloads the latest Windows installer from GitHub, checks its SHA-256
# against the release manifest, and runs it. The installer asks for
# administrator rights itself, because RamWarden's service runs as SYSTEM.
$ErrorActionPreference = 'Stop'
$repo = 'https://github.com/ethanrxla/ramwarden/releases/latest/download'
$file = 'ramwarden-windows-x86_64.exe'

if (-not [Environment]::Is64BitOperatingSystem) { throw 'RamWarden needs 64-bit Windows.' }

$dir = Join-Path $env:TEMP ("ramwarden-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $dir | Out-Null
$exe = Join-Path $dir $file
try {
    Write-Host "ramwarden: downloading $file"
    try { Invoke-WebRequest -UseBasicParsing "$repo/$file" -OutFile $exe }
    catch { throw "Could not download $file. If no release has been published yet, build from source: https://github.com/ethanrxla/ramwarden" }

    # The manifest lists each artifact's SHA-256. No manifest: skip the check.
    $m = $null
    try { $m = Invoke-RestMethod "$repo/manifest.json" } catch { }
    if ($m) {
        $items = if ($m.wardens) { $m.wardens } else { @($m) }
        $want = ($items.artifacts | Where-Object { $_.url -like "*/$file" } | Select-Object -First 1).sha256
        if ($want) {
            $got = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLower()
            if ($got -ne $want.ToLower()) { throw "Checksum mismatch for $file; not installing." }
            Write-Host 'ramwarden: checksum verified'
        }
    }

    Start-Process -FilePath $exe -Wait
    Write-Host 'ramwarden: installed. Open RamWarden from the tray, then Sign in for Pro, or keep it local and free.'
} finally {
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
