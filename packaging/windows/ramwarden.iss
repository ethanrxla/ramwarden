; RamWarden installer for Windows (Inno Setup 6).
;
; Built by scripts\package-windows.ps1, which passes the version and the
; staged payload directory:
;   iscc /DAppVersion=0.3.0 /DPayload=C:\...\payload /DOutDir=C:\...\dist packaging\windows\ramwarden.iss
;
; The installer does not lay files out itself. It unpacks the payload to a
; temporary folder and runs scripts\install.ps1 on it, the same script a
; source build uses, so a fresh install, an update from the launcher
; (`setup.exe /VERYSILENT`) and a developer install all take one path.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef Payload
  #error Pass /DPayload=<staged payload directory>
#endif
#ifndef OutDir
  #define OutDir "."
#endif

[Setup]
; Never change AppId: Windows matches upgrades and uninstall entries by it.
AppId={{6E2B6A2D-6E0B-4B4E-9D5B-1C7A0F3B9A11}
AppName=RamWarden
AppVersion={#AppVersion}
AppPublisher=RamWarden
AppPublisherURL=https://github.com/ethanrxla/ramwarden
DefaultDirName={autopf}\RamWarden
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; install.ps1 stops the running pieces itself; Restart Manager prompts would
; only block a silent update.
CloseApplications=no
OutputDir={#OutDir}
OutputBaseFilename=ramwarden-windows-x86_64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=RamWarden
UninstallDisplayIcon={app}\ramwarden-tray.exe
MinVersion=10.0.22000

[Files]
Source: "{#Payload}\*"; DestDir: "{tmp}\payload"; Flags: ignoreversion recursesubdirs deleteafterinstall
; Kept for the uninstaller.
Source: "{#Payload}\install.ps1"; DestDir: "{app}"; Flags: ignoreversion

[UninstallRun]
Filename: "powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\install.ps1"" -Uninstall"; \
  Flags: runhidden waituntilterminated; RunOnceId: "RemoveRamWarden"

[Code]
// Run install.ps1 and fail the setup if it fails, so neither a person nor the
// launcher (which reads the exit code) is told an install worked when it did not.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
  Payload: String;
begin
  if CurStep = ssPostInstall then
  begin
    Payload := ExpandConstant('{tmp}\payload');
    WizardForm.StatusLabel.Caption := 'Registering the RamWarden service and tasks...';
    if not Exec('powershell.exe',
        '-NoProfile -ExecutionPolicy Bypass -File "' + Payload + '\install.ps1" -Source "' + Payload + '"',
        Payload, SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
    begin
      MsgBox('RamWarden could not finish installing (install.ps1 exit code ' + IntToStr(Code) + ').',
        mbError, MB_OK);
      Abort;
    end;
  end;
end;
