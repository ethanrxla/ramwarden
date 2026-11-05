; Offline developer preview. Release only after WINDOWS_STORE.md gates pass.
#ifndef PayloadDir
  #error PayloadDir is required
#endif
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef OutputDir
  #error OutputDir is required
#endif
[Setup]
AppId={{7C363A32-917A-49B7-B06E-29DF62720883}
AppName=RamWarden
AppVersion={#AppVersion}
AppPublisher=RamWarden
AppPublisherURL=https://ramwarden.com
AppSupportURL=https://ramwarden.com
DefaultDirName={autopf}\RamWarden
DefaultGroupName=RamWarden
DisableProgramGroupPage=yes
DisableWelcomePage=no
WizardStyle=modern
SetupIconFile=..\..\assets\brand\ramwarden.ico
UninstallDisplayIcon={app}\ramwarden-tray.exe
LicenseFile=..\..\LICENSE
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
OutputDir={#OutputDir}
OutputBaseFilename=RamWarden-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
Uninstallable=yes
[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; Flags: unchecked
[Files]
Source: "{#PayloadDir}\ramwarden-tray.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\ramwarden-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\ramwarden-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\ramwarden-usage-sync.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\ramwarden-gpu.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\ramwarden-debloat.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
[Icons]
Name: "{group}\RamWarden"; Filename: "{app}\ramwarden-tray.exe"
Name: "{autodesktop}\RamWarden"; Filename: "{app}\ramwarden-tray.exe"; Tasks: desktopicon
[Run]
Filename: "{app}\ramwarden-tray.exe"; Description: "Launch RamWarden"; Flags: nowait postinstall skipifsilent runasoriginaluser
