# Service-scoped workaround for Suricata 8.0.7 WinStrftime's wrong DST offset.
# The machine/user timezone and other services' environments stay unchanged.
function Set-SuricataCaptureClock([switch]$Restart) {
    $ErrorActionPreference='Stop'
    $key='HKLM:\SYSTEM\CurrentControlSet\Services\Suricata'
    $item=Get-ItemProperty -LiteralPath $key
    $present=$null -ne $item.PSObject.Properties['Environment']
    $previous=@($item.Environment)
    if ($present -and (Get-Item -LiteralPath $key).GetValueKind('Environment') -ne 'MultiString') { throw 'Unexpected service Environment registry type; retained' }
    $next=@($previous | Where-Object {$_ -and $_ -notmatch '^TZ='})+@('TZ=UTC0')
    $backupDir='C:\ProgramData\ThreatWarden\backups'
    New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
    & icacls.exe $backupDir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot secure service environment backup' }
    $backup=Join-Path $backupDir ('suricata-environment-'+[DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffZ')+'.clixml')
    @{Present=$present;Value=$previous} | Export-Clixml -LiteralPath $backup
    try {
        New-ItemProperty -LiteralPath $key -Name Environment -PropertyType MultiString -Value $next -Force | Out-Null
        if ($Restart) { Restart-Service Suricata -ErrorAction Stop }
    } catch {
        if ($present) { New-ItemProperty -LiteralPath $key -Name Environment -PropertyType MultiString -Value $previous -Force | Out-Null }
        else { Remove-ItemProperty -LiteralPath $key -Name Environment -ErrorAction SilentlyContinue }
        if ($Restart) { Restart-Service Suricata -ErrorAction Continue }
        throw
    }
    Write-Output "Suricata service timezone set to UTC; previous environment saved at $backup"
}
