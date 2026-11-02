# ET Open updates: validate a candidate before replacing the running ruleset.
param([string]$Root='D:\ThreatWarden\Suricata')
$ErrorActionPreference='Stop'
$stage=Join-Path $Root ('update-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $stage | Out-Null
try {
 $version=(& (Join-Path $Root 'suricata.exe') -V | Out-String)
 if ($version -notmatch '(8\.0\.\d+)') { throw 'Unsupported Suricata release' }
 $url='https://rules.emergingthreats.net/open/suricata-'+$Matches[1]+'/emerging.rules.tar.gz'
 Invoke-WebRequest $url -OutFile (Join-Path $stage 'rules.tar.gz')
 & tar.exe -xf (Join-Path $stage 'rules.tar.gz') -C $stage
 if ($LASTEXITCODE -ne 0) { throw 'Rules archive extraction failed' }
 $candidate=Join-Path $stage 'suricata.rules'
 $writer=New-Object IO.StreamWriter($candidate,$false,(New-Object Text.UTF8Encoding($false)))
 $skipped=0
 try {
  foreach ($file in (Get-ChildItem $stage -Recurse -Filter '*.rules' | Where-Object {$_.FullName -ne $candidate})) {
   foreach ($line in [IO.File]::ReadLines($file.FullName)) {
    if ($line -match 'file\.magic;|filemagic:') { $skipped++; continue }
    $writer.WriteLine($line)
   }
  }
 } finally { $writer.Dispose() }
 if ((Get-Item $candidate).Length -lt 10000) { throw 'Ruleset unexpectedly small' }
 & (Join-Path $Root 'suricata.exe') -T -c (Join-Path $Root 'suricata.yaml') -S $candidate -l (Join-Path $Root 'log') *> (Join-Path $Root 'log\rule-update-test.log')
 if ($LASTEXITCODE -ne 0) { throw 'Candidate rules failed validation; current rules retained' }
 $target=Join-Path $Root 'rules\suricata.rules'
 [IO.File]::Replace($candidate,$target,($target+'.previous'))
 Restart-Service Suricata
 @{updated_at=[DateTime]::UtcNow.ToString('o');source=$url;unsupported_rules=$skipped} | ConvertTo-Json | Set-Content (Join-Path $Root 'rules\update-status.json')
} finally { Remove-Item $stage -Recurse -Force }
