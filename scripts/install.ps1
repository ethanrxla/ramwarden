<#
.SYNOPSIS
  Install (or remove) RamWarden on Windows.

.DESCRIPTION
  Must run elevated. Installs:
    * binaries to %ProgramFiles%\RamWarden (administrator-writable only: the
      service runs as SYSTEM, so a user-writable copy would be a privilege
      escalation);
    * ramwarden-service as an automatic LocalSystem service;
    * the daemon as a logon task for the installing user, run headless through
      `conhost --headless` so it has no window but still receives the console
      close/logoff/shutdown events it uses to restore Tidy hints;
    * the tray (when built) at logon;
    * the launcher's update check (when built) at logon and every six hours;
    * a starter config at %APPDATA%\ramwarden\config.toml if none exists.

.EXAMPLE
  cargo build --release -p ramwarden-daemon -p ramwarden-service -p ramwarden-debloat
  cargo build --release -p ramwarden-ui --features tray --bin ramwarden-tray
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1

.EXAMPLE
  # With the GPU worker (needs the CUDA toolkit). The daemon itself has no CUDA
  # feature; it starts ramwarden-gpu when there is GPU work.
  scripts\cargo-cuda.cmd build --release -p ramwarden-ml --features cuda,embed --bin ramwarden-gpu
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1

.EXAMPLE
  # What the release installer (packaging\windows\ramwarden.iss) runs, on its
  # unpacked payload. The launcher updates by running that installer.
  powershell -ExecutionPolicy Bypass -File install.ps1 -Source <payload>

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Uninstall
#>
[CmdletBinding()]
param(
  # Resolved below: Windows PowerShell 5.1 leaves $PSScriptRoot empty while
  # parameter defaults are evaluated.
  [string]$Source = '',
  [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Source) { $Source = Join-Path $Root '..\target\release' }

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'Run this from an elevated PowerShell (Run as administrator).'
}

$Dest     = Join-Path $env:ProgramFiles 'RamWarden'
$TaskName = 'RamWarden daemon'
$TrayTask = 'RamWarden tray'
$UpdateTask = 'RamWarden updates'
$Service  = Join-Path $Dest 'ramwarden-service.exe'
$Shortcut = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\RamWarden.lnk'

function Remove-RamWarden {
  foreach ($t in @($TaskName, $TrayTask, $UpdateTask)) {
    if (Get-ScheduledTask -TaskName $t -ErrorAction SilentlyContinue) {
      Stop-ScheduledTask -TaskName $t -ErrorAction SilentlyContinue
      Unregister-ScheduledTask -TaskName $t -Confirm:$false
    }
  }
  Get-Process ramwarden-daemon, ramwarden-tray, ramwarden-launcher -ErrorAction SilentlyContinue | Stop-Process -Force
  if (Get-Service RamWarden -ErrorAction SilentlyContinue) {
    if (Test-Path $Service) { & $Service uninstall } else { sc.exe delete RamWarden | Out-Null }
    Start-Sleep -Seconds 1
  }
}

if ($Uninstall) {
  Remove-RamWarden
  if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
  if (Test-Path $Shortcut) { Remove-Item -Force $Shortcut }
  Write-Host 'RamWarden removed. Your config and journals were left in %APPDATA%, %LOCALAPPDATA% and %ProgramData%.'
  Write-Host 'Revert any applied profiles first with: ramwarden-debloat revert all'
  return
}

$Source = (Resolve-Path $Source).Path
foreach ($exe in 'ramwarden-daemon.exe', 'ramwarden-service.exe', 'ramwarden-debloat.exe') {
  if (-not (Test-Path (Join-Path $Source $exe))) {
    throw "$exe not found in $Source. Build first: cargo build --release -p ramwarden-daemon -p ramwarden-service -p ramwarden-debloat"
  }
}

Remove-RamWarden
New-Item -ItemType Directory -Force $Dest | Out-Null
Get-ChildItem $Source -Filter 'ramwarden-*.exe' | Copy-Item -Destination $Dest -Force

# The daemon starts through a launcher that puts the CUDA runtime on PATH: a
# daemon built with `--features cuda` imports cublas and curand at load time,
# and the logon task's environment does not include CUDA's bin\x64. Harmless
# for a CPU-only build. Lives in Program Files, so only admins can edit it.
$Launcher = Join-Path $Dest 'ramwarden-daemon.cmd'
@'
@echo off
if defined CUDA_PATH set "PATH=%CUDA_PATH%\bin\x64;%CUDA_PATH%\bin;%PATH%"
"%~dp0ramwarden-daemon.exe" %*
'@ | Set-Content -Encoding ascii $Launcher

# Service: registered from Program Files, which it insists on.
& $Service install
if ($LASTEXITCODE -ne 0) { throw 'service install failed' }

# Config, once.
$ConfigDir = Join-Path $env:APPDATA 'ramwarden'
$Config    = Join-Path $ConfigDir 'config.toml'
if (-not (Test-Path $Config)) {
  New-Item -ItemType Directory -Force $ConfigDir | Out-Null
  # A release payload carries the starter config beside the binaries; a source
  # checkout has it at the repository root.
  $Template = Join-Path $Source 'ramwarden.toml'
  if (-not (Test-Path $Template)) { $Template = Join-Path $Root '..\ramwarden.toml' }
  Copy-Item $Template $Config
  Write-Host "Wrote starter config to $Config"
}

# Daemon: at logon, as this user, not elevated, headless.
$user    = "$env:USERDOMAIN\$env:USERNAME"
$log     = Join-Path $env:LOCALAPPDATA 'ramwarden\daemon.log'
$action  = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\conhost.exe" `
             -Argument "--headless `"$Launcher`" --log-file `"$log`"" `
             -WorkingDirectory $ConfigDir
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
             -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
$task    = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Settings $settings -Principal $task | Out-Null
Start-ScheduledTask -TaskName $TaskName

$tray = Join-Path $Dest 'ramwarden-tray.exe'
if (Test-Path $tray) {
  $a = New-ScheduledTaskAction -Execute $tray -Argument '--hidden'
  Register-ScheduledTask -TaskName $TrayTask -Action $a -Trigger $trigger -Settings $settings -Principal $task | Out-Null
  Start-ScheduledTask -TaskName $TrayTask

  # Start menu entry. Launching the app while the tray runs opens the window.
  $shell = New-Object -ComObject WScript.Shell
  $lnk = $shell.CreateShortcut($Shortcut)
  $BrandIcon = Join-Path $Root '..\assets\brand\ramwarden.ico'
  if (Test-Path $BrandIcon) {
    Copy-Item $BrandIcon (Join-Path $Dest 'ramwarden.ico') -Force
    $lnk.IconLocation = (Join-Path $Dest 'ramwarden.ico')
  }
  $lnk.TargetPath = $tray
  $lnk.WorkingDirectory = $Dest
  $lnk.Description = 'RamWarden memory manager'
  $lnk.Save()
}

# Update checks: as this user, not elevated. The check only downloads and
# verifies; installing a Windows update raises a UAC prompt, so it is done
# from the tray or `ramwarden-launcher update`, never unattended.
$launcherExe = Join-Path $Dest 'ramwarden-launcher.exe'
if (Test-Path $launcherExe) {
  $a = New-ScheduledTaskAction -Execute $launcherExe -Argument 'check --background' -WorkingDirectory $ConfigDir
  $t1 = New-ScheduledTaskTrigger -AtLogOn -User $user
  $t1.Delay = 'PT10M'
  $t2 = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(30) -RepetitionInterval (New-TimeSpan -Hours 6)
  $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
         -ExecutionTimeLimit (New-TimeSpan -Minutes 30)
  Register-ScheduledTask -TaskName $UpdateTask -Action $a -Trigger @($t1, $t2) -Settings $s -Principal $task | Out-Null
}

Write-Host "RamWarden installed to $Dest"
Write-Host "  app     : Start menu -> RamWarden (or the ring icon in the system tray)"
Write-Host "  service : $((Get-Service RamWarden).Status)"
Write-Host "  daemon  : http://127.0.0.1:7823  (log: $log)"
Write-Host "  debloat : & '$Dest\ramwarden-debloat.exe' status"
if (Test-Path $launcherExe) { Write-Host "  updates : & '$launcherExe' status" }
