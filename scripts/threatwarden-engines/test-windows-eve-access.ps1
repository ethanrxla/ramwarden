$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'windows-eve-access.ps1')
$root=Join-Path ([IO.Path]::GetTempPath()) ('rw-eve-writer-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
# Match the regression: only the non-admin reader entry propagates to files.
$acl=Get-Acl -LiteralPath $root
$acl.SetAccessRuleProtection($true,$false)
$me=[Security.Principal.WindowsIdentity]::GetCurrent().User
$acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($me,'FullControl','None','None','Allow')))
$readerSid=New-Object Security.Principal.SecurityIdentifier('S-1-5-32-546')
$acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($readerSid,'ReadAndExecute','ContainerInherit,ObjectInherit','None','Allow')))
Set-Acl -LiteralPath $root -AclObject $acl
$existing=Join-Path $root 'existing.json'
# Give this process access to create the test fixture before adding writer rights.
Set-Content -LiteralPath $existing -Value '{}'
Repair-ThreatWardenEveWriterAccess $root
$rotated=Join-Path $root 'rotated.json'
Set-Content -LiteralPath $rotated -Value '{}'
foreach ($path in @($existing,$rotated)) {
    $rules=(Get-Acl -LiteralPath $path).GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])
    $writer=@($rules | Where-Object {$_.IdentityReference.Value -eq 'S-1-5-18' -and $_.AccessControlType -eq 'Allow' -and ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::Modify) -eq [Security.AccessControl.FileSystemRights]::Modify})
    if ($writer.Count -eq 0) { throw "SYSTEM cannot write $path" }
    $reader=@($rules | Where-Object {$_.IdentityReference.Value -eq 'S-1-5-32-546'})
    if ($reader.Count -eq 0 -or @($reader | Where-Object {($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::Write) -ne 0}).Count -ne 0) { throw 'Reader permissions changed' }
}
Write-Output 'EVE ACL tests passed: existing and rotated files writable by SYSTEM; reader remains read-only.'
# Disposable evidence is retained for inspection; no real service was changed.
