# Preserve read-only daemon access while ensuring rotated files remain writable
# by the SYSTEM capture service. Directory-only SYSTEM grants do not inherit.
function Repair-ThreatWardenEveWriterAccess([string]$Directory) {
    $ErrorActionPreference='Stop'
    $root=Get-Item -LiteralPath $Directory -Force
    if (-not $root.PSIsContainer) { throw 'EVE output must be a directory' }
    $items=@($root)+@(Get-ChildItem -LiteralPath $root.FullName -Recurse -Force)
    foreach ($item in $items) {
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Refusing a reparse point in EVE output' }
    }
    foreach ($item in $items) {
        $system=if($item.PSIsContainer){'(OI)(CI)M'}else{'M'}
        $admin=if($item.PSIsContainer){'(OI)(CI)F'}else{'F'}
        & icacls.exe $item.FullName /grant "*S-1-5-18:$system" "*S-1-5-32-544:$admin" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Could not restore EVE writer access: $($item.FullName)" }
    }
}
