; Offline candidate. Production builds require all payload PE files + installer signed.
#ifndef Payload
  #error Payload directory required
#endif
#ifndef ReleaseVersion
  #error ReleaseVersion required
#endif
#ifndef OutputRoot
  #define OutputRoot "."
#endif
#ifndef ArtifactSuffix
  #define ArtifactSuffix ""
#endif
[Setup]
AppId={{6D917773-EB82-4E3D-AC26-43A174B9BE57}
AppName=RamWarden
AppVersion={#ReleaseVersion}
AppPublisher=RamWarden
AppPublisherURL=https://ramwarden.com/
AppSupportURL=https://ramwarden.com/setup.html
DefaultDirName={autopf}\RamWarden
DisableDirPage=yes
DefaultGroupName=RamWarden
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
WizardStyle=modern
WizardResizable=yes
SetupIconFile=..\..\assets\brand\ramwarden.ico
UninstallDisplayIcon={app}\ramwarden.ico
LicenseFile=..\..\LICENSE
OutputDir={#OutputRoot}
OutputBaseFilename=RamWarden-Setup-{#ReleaseVersion}-x64{#ArtifactSuffix}
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
CloseApplicationsFilter=ramwarden*.exe
RestartApplications=no
UninstallDisplayName=RamWarden
VersionInfoVersion={#ReleaseVersion}
VersionInfoDescription=RamWarden installer
; The production packaging script supplies an external signing command and signs
; the generated uninstaller too. An unsigned candidate is never store-ready.
#ifdef SigningCommand
SignTool=release
SignedUninstaller=yes
#endif
[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
[Files]
Source: "{#Payload}\ramwarden-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\ramwarden-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\ramwarden-debloat.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\ramwarden-tray.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\ramwarden.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\ramwarden-usage-sync.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Payload}\ramwarden-launcher.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\scripts\install.ps1"; DestName: "prepare.ps1"; Flags: dontcopy
Source: "..\..\scripts\install.ps1"; DestDir: "{app}\setup"; Flags: ignoreversion
Source: "config.toml"; DestDir: "{app}"; DestName: "ramwarden.toml"; Flags: ignoreversion
Source: "..\..\assets\brand\ramwarden.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\extension\*.js"; DestDir: "{app}\extension"; Flags: ignoreversion
Source: "..\..\extension\popup.html"; DestDir: "{app}\extension"; Flags: ignoreversion
Source: "..\..\build\chrome\manifest.json"; DestDir: "{app}\extension"; Flags: ignoreversion
Source: "..\..\extension\icons\*.png"; DestDir: "{app}\extension\icons"; Flags: ignoreversion
[Icons]
Name: "{autoprograms}\RamWarden"; Filename: "{app}\ramwarden-tray.exe"; IconFilename: "{app}\ramwarden.ico"
Name: "{autodesktop}\RamWarden"; Filename: "{app}\ramwarden-tray.exe"; IconFilename: "{app}\ramwarden.ico"; Tasks: desktopicon
[Run]
Filename: "{app}\ramwarden-tray.exe"; Description: "Open RamWarden"; Flags: nowait postinstall skipifsilent runasoriginaluser
[Code]
function RunLifecycle(Script, Arguments: String): Boolean;
var ExitCode: Integer;
begin
  Result := Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + Script + '" ' + Arguments,
    '', SW_HIDE, ewWaitUntilTerminated, ExitCode);
  Result := Result and (ExitCode = 0);
end;
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  ExtractTemporaryFile('prepare.ps1');
  if not RunLifecycle(ExpandConstant('{tmp}\prepare.ps1'), '-StopOnly') then
    Result := 'RamWarden could not stop the previous components. Restart Windows and retry. No new components were registered.';
end;
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    if not RunLifecycle(ExpandConstant('{app}\setup\install.ps1'), '-Source "' + ExpandConstant('{app}') + '" -NoTrayStart') then
      RaiseException('RamWarden could not register its background components. Setup did not finish. See the setup log, then repair or uninstall.');
end;
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    if not RunLifecycle(ExpandConstant('{app}\setup\install.ps1'), '-UnregisterOnly') then
      RaiseException('RamWarden could not stop its components. Restart Windows and retry uninstall.');
end;
