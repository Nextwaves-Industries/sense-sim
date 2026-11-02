; Sense Sim installer (Inno Setup 6). Build: ISCC installer\SenseSim.iss
#define AppName "Sense Sim"
#define AppVersion "1.7.0"
#define AppExe "SenseSim.exe"

[Setup]
AppId={{6E2B7F4C-5A1D-4E8B-9C3F-2D7A1B0E5C91}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Sense Sim
DefaultDirName={localappdata}\Programs\Sense Sim
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=SenseSim-Setup-{#AppVersion}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
WizardStyle=modern
WizardSmallImageFile=..\assets\logo_small.png
Compression=lzma2/ultra64
SolidCompression=yes
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\icon.ico"; DestDir: "{app}"; DestName: "SenseSim.ico"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion 

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\SenseSim.ico"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\SenseSim.ico"; Tasks: desktopicon

[Registry]
; Per user, like the install: double-clicking a project opens the app.
Root: HKA; Subkey: "Software\Classes\.sesim"; ValueType: string; ValueName: ""; ValueData: "SenseSim.Project"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\SenseSim.Project"; ValueType: string; ValueName: ""; ValueData: "Sense Sim project"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SenseSim.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\SenseSim.ico"
Root: HKA; Subkey: "Software\Classes\SenseSim.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent
