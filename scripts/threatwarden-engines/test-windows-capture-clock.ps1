# Simulated service/registry failure tests. Never touches the real service or registry.
$ErrorActionPreference='Stop'
& {
    function Get-ItemProperty { param($LiteralPath)
        if ($script:clockPresent) { [pscustomobject]@{Environment=$script:clockValues} }
        else { [pscustomobject]@{} }
    }
    function Get-Item { param($LiteralPath)
        $item=New-Object PSObject
        $item | Add-Member ScriptMethod GetValueKind { param($name) 'MultiString' }
        $item
    }
    function New-Item { param($ItemType,$Path,[switch]$Force) }
    function icacls.exe { $script:LASTEXITCODE=0 }
    function Export-Clixml {
        [CmdletBinding()]param([Parameter(ValueFromPipeline)]$InputObject,$LiteralPath)
        process { $script:clockBackup=$InputObject }
    }
    function New-ItemProperty { param($LiteralPath,$Name,$PropertyType,$Value,[switch]$Force)
        if ($Name -ne 'Environment' -or $PropertyType -ne 'MultiString') { throw 'Unexpected registry write' }
        $script:clockPresent=$true
        $script:clockValues=@($Value)
    }
    function Remove-ItemProperty {
        [CmdletBinding()]param($LiteralPath,$Name)
        $script:clockPresent=$false
        $script:clockValues=@()
    }
    function Restart-Service {
        [CmdletBinding()]param($Name)
        if ($Name -ne 'Suricata') { throw 'Unexpected service' }
        $script:clockRestarts++
        if ($script:clockFail -and $script:clockRestarts -eq 1) { throw 'Simulated restart failure' }
    }
    . (Join-Path $PSScriptRoot 'windows-capture-clock.ps1')
    foreach ($present in @($false,$true)) {
        foreach ($fail in @($false,$true)) {
            $script:clockPresent=$present
            $script:clockValues=if($present){@('UNRELATED=preserve','TZ=EST5EDT')}else{@()}
            $original=@($script:clockValues)
            $script:clockFail=$fail
            $script:clockRestarts=0
            $script:clockBackup=$null
            $threw=$false
            try { Set-SuricataCaptureClock -Restart | Out-Null } catch { $threw=$true }
            if ($threw -ne $fail) { throw 'Restart failure was not propagated' }
            if ($script:clockBackup.Present -ne $present -or ($script:clockBackup.Value -join '|') -ne ($original -join '|')) { throw 'Previous environment was not backed up' }
            if ($fail) {
                if ($script:clockPresent -ne $present -or ($script:clockValues -join '|') -ne ($original -join '|')) { throw 'Previous environment was not restored' }
                if ($script:clockRestarts -ne 2) { throw 'Service was not restarted after restoration' }
            } else {
                $expected=if($present){@('UNRELATED=preserve','TZ=UTC0')}else{@('TZ=UTC0')}
                if (($script:clockValues -join '|') -ne ($expected -join '|') -or $script:clockRestarts -ne 1) { throw 'Service environment or restart is incorrect' }
            }
        }
    }
}
Write-Output 'Capture clock tests passed (existing/absent environment, success/failure, backup and rollback).'
