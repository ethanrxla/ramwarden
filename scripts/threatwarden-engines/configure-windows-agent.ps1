$ErrorActionPreference='Stop'
$f='C:\Program Files (x86)\ossec-agent\ossec.conf'
[xml]$xml=Get-Content $f
$root=$xml.DocumentElement
$ar=$root.SelectSingleNode('active-response')
if (-not $ar) { $ar=$xml.CreateElement('active-response'); $root.AppendChild($ar) | Out-Null }
$disabled=$ar.SelectSingleNode('disabled')
if (-not $disabled) { $disabled=$xml.CreateElement('disabled'); $ar.AppendChild($disabled) | Out-Null }
$disabled.InnerText='yes'
$location='C:\ProgramData\ThreatWarden\logs\clamav.json'
if (-not ($root.localfile | Where-Object {$_.location -eq $location})) {
 $lf=$xml.CreateElement('localfile')
 foreach ($entry in @(@('log_format','json'),@('location',$location))) {
  $node=$xml.CreateElement($entry[0]); $node.InnerText=$entry[1]; $lf.AppendChild($node) | Out-Null
 }
 $root.AppendChild($lf) | Out-Null
}
New-Item -ItemType Directory -Force C:/ProgramData/ThreatWarden/logs | Out-Null
if (-not (Test-Path $location)) { New-Item -ItemType File $location | Out-Null }
$xml.Save($f)
Restart-Service WazuhSvc
Get-Service WazuhSvc
