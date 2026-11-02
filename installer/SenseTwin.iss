; Sense Twin service installer (Inno Setup 6). Build: ISCC installer\SenseTwin.iss
;
; Separate from the viewer's per-user installer. A service's program has to sit where only
; administrators can replace it, so this always installs to Program Files.
#define AppName "Sense Twin"
#define AppVersion "0.1.0"
#define AppExe "SenseTwin.exe"
#ifndef SourceExe
  #define SourceExe "..\target\release\SenseTwin.exe"
#endif

[Setup]
AppId={{9D3C7A52-6E41-4B0F-8F27-3A1C5E90B7D4}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Sense Sim
DefaultDirName={autopf}\Sense Twin
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=SenseTwin-Setup-{#AppVersion}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
; The service is stopped below, so there's nothing for Restart Manager to do.
CloseApplications=no
RestartApplications=no

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion

[Code]
var
  ServiceInstallFailed: Boolean;

{ Runs the installed program and waits, because the [Run] section ignores exit codes. }
function RunServiceCommand(Command: String): Boolean;
var
  ExitCode: Integer;
begin
  Result := Exec(ExpandConstant('{app}\{#AppExe}'), Command, '', SW_HIDE, ewWaitUntilTerminated, ExitCode) and (ExitCode = 0);
end;

{ An upgrade can't replace the program while the service runs it. Uninstalling the service
  keeps its data and the readers group, and it comes back after the files are copied. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if FileExists(ExpandConstant('{app}\{#AppExe}')) and not RunServiceCommand('service-uninstall') then
    Result := 'The running Sense Twin service could not be stopped, so it was left as it is.';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and not RunServiceCommand('service-install') then
  begin
    ServiceInstallFailed := True;
    SuppressibleMsgBox('The files were copied, but the Sense Twin service was not set up. Run "SenseTwin service-install" from an elevated prompt to see why.', mbError, MB_OK, IDOK);
  end;
end;

{ So a silent install can tell it failed. }
function GetCustomSetupExitCode: Integer;
begin
  if ServiceInstallFailed then
    Result := 1
  else
    Result := 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usUninstall) and not RunServiceCommand('service-uninstall') then
  begin
    SuppressibleMsgBox('The Sense Twin service could not be removed, so nothing was uninstalled. Stop it and try again.', mbError, MB_OK, IDOK);
    Abort;
  end;
end;
