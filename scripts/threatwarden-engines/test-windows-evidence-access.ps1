# Real ACL regression in disposable files; no elevation or service changes.
$ErrorActionPreference='Stop'
$root=Join-Path ([IO.Path]::GetTempPath()) ('rw-evidence-acl-'+[Guid]::NewGuid().ToString('N'))
$guestSid='S-1-5-32-546'
try {
    New-Item -ItemType Directory -Path (Join-Path $root 'nested') -Force | Out-Null
    $file=Join-Path $root 'nested\eve.json'
    Set-Content -LiteralPath $file -Value '{}'
    $acl=Get-Acl -LiteralPath $file
    $acl.SetAccessRuleProtection($true,$true)
    Set-Acl -LiteralPath $file -AclObject $acl
    & (Join-Path $PSScriptRoot 'repair-windows-evidence-access.ps1') -ReaderSid $guestSid -EvidencePaths @($root)
    foreach ($path in @($root,(Join-Path $root 'nested'),$file)) {
        $rules=@((Get-Acl -LiteralPath $path).GetAccessRules($true,$false,[Security.Principal.SecurityIdentifier]) | Where-Object {$_.IdentityReference.Value -eq $guestSid})
        if ($rules.Count -eq 0) { throw 'Missing explicit evidence read grant' }
        foreach ($rule in $rules) {
            $allowed=[Security.AccessControl.FileSystemRights]::ReadAndExecute -bor [Security.AccessControl.FileSystemRights]::Synchronize
            if ($rule.AccessControlType -ne 'Allow' -or ($rule.FileSystemRights -band (-bnot $allowed)) -ne 0) { throw 'Evidence rights exceed read/execute' }
            if (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::ReadAndExecute) -ne [Security.AccessControl.FileSystemRights]::ReadAndExecute) { throw 'Evidence read rights missing' }
            if ($path -eq $file -and $rule.InheritanceFlags -ne 'None') { throw 'File grant incorrectly uses folder inheritance flags' }
        }
    }
    # New files must inherit read access from the repaired directory.
    $newFile=Join-Path $root 'nested\rotated.json'
    Set-Content -LiteralPath $newFile -Value '{}'
    $inherited=@((Get-Acl -LiteralPath $newFile).GetAccessRules($false,$true,[Security.Principal.SecurityIdentifier]) | Where-Object {$_.IdentityReference.Value -eq $guestSid})
    if ($inherited.Count -eq 0) { throw 'Rotated evidence has no inherited read access' }
    Write-Output 'Evidence ACL tests passed (protected existing file, nested folders, future rotation, read-only rights).'
} finally {
    $resolved=[IO.Path]::GetFullPath($root)
    $temp=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')+'\'
    if (-not $resolved.StartsWith($temp,[StringComparison]::OrdinalIgnoreCase)) { throw 'Cleanup path escaped temporary directory' }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
