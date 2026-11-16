; FindX isolated-fixture setup (NOT a published installer, NOT an official upgrade)
;
; This script is the only Inno packager allowed while official upgrade gates
; remain closed. It must never be compiled with /DUpgradeGatesApproved=1, must
; never share the official AppId, must never default to Program Files, must
; never register or start FindX2Search, must never write HKCU\...\Run\FindX,
; and must never taskkill findx2-service.exe / FindX.exe.
;
; Build: powershell -File installer\pack-isolated.ps1
;   which invokes:
;   iscc /DMyAppVersion=2.5.0 /DIsolatedFixturePackaging=1
;        /DPublishDir=<checkout-stage> /DIsolatedOutputDir=<checkout-out>
;        /DIsolatedDefaultDir=<checkout-fresh-dir>
;        installer\FindX-isolated-fixture.iss
;
; Runtime: setup.exe /DIR="<isolated dir inside checkout>" /SILENT /NORESTART
; Official paths are rejected at InitializeSetup / PrepareToInstall.

#ifndef IsolatedFixturePackaging
  #error "This script is the isolated-fixture packager only. Do not compile it as a public installer. Use installer\\pack-isolated.ps1."
#endif

#ifdef UpgradeGatesApproved
  #error "Refusing to combine IsolatedFixturePackaging with UpgradeGatesApproved. Official publish gates stay closed."
#endif

#ifndef MyAppVersion
  #define MyAppVersion "2.5.1"
#endif
#ifndef PublishDir
  #define PublishDir "stage"
#endif
#ifndef IsolatedOutputDir
  #define IsolatedOutputDir "..\\target\\t156-packaging\\setup"
#endif
#ifndef IsolatedDefaultDir
  #define IsolatedDefaultDir "..\\target\\t156-packaging\\fresh-install"
#endif

#define MyAppName      "FindX Isolated Fixture"
#define MyAppPublisher "FindX"
#define MyAppExeName   "FindX.exe"
#define MyInstalledMarker "FindX.installed"

[Setup]
; Distinct from the official AppId {{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C}.
; This setup must not detect, mutate, or uninstall the official 2.4.10 product.
AppId={{C0F1A2B3-4D5E-6789-ABCD-0123456789EF}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion} (not for publication)
AppPublisher={#MyAppPublisher}
DefaultDirName={#IsolatedDefaultDir}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
DisableProgramGroupPage=yes
OutputDir={#IsolatedOutputDir}
OutputBaseFilename=FindX-{#MyAppVersion}-isolated-fixture-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
UsePreviousAppDir=no
UsePreviousGroup=no
UsePreviousSetupType=no
UsePreviousTasks=no
UsePreviousLanguage=no
CloseApplications=no
RestartApplications=no
CreateUninstallRegKey=yes
UpdateUninstallLogAppName=no
UninstallDisplayName={#MyAppName} {#MyAppVersion}
WizardStyle=modern
SetupLogging=yes
SetupIconFile=..\gui\src-tauri\icons\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
MinVersion=10.0
; Do not restart, do not close running trial/official processes.
RestartIfNeededByRun=no

[Languages]
Name: "english"; MessagesFile: "Languages\Default.isl"
Name: "chinesesimplified"; MessagesFile: "Languages\ChineseSimplified.isl"

[Types]
Name: "full"; Description: "Isolated fixture files only"

[Components]
Name: "main"; Description: "FindX program and companions (no service, no PATH, no autostart)"; Types: full; Flags: fixed

[Files]
Source: "{#PublishDir}\FindX.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\findx2.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\fx.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\findx2-service.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\FindX.installed"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: main
Source: "{#PublishDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: main
Source: "{#PublishDir}\stage-manifest.json"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: main
Source: "{#PublishDir}\resources\*"; DestDir: "{app}\resources"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist; Components: main

[Code]
function ContainsPath(const Dir, Prefix: String): Boolean;
var
  D, P: String;
begin
  D := LowerCase(AddBackslash(Dir));
  P := LowerCase(AddBackslash(Prefix));
  Result := (D = P) or (Pos(P, D) = 1);
end;

function ForbiddenInstallDir(const Dir: String): String;
var
  Pf, Pf86, Pd, Ad, Lad: String;
begin
  Result := '';
  Pf := ExpandConstant('{commonpf}');
  Pf86 := ExpandConstant('{commonpf32}');
  Pd := ExpandConstant('{commonappdata}');
  Ad := ExpandConstant('{userappdata}');
  Lad := ExpandConstant('{localappdata}');
  if ContainsPath(Dir, AddBackslash(Pf) + 'FindX') then
    Result := 'refusing official Program Files\FindX path: ' + Dir
  else if ContainsPath(Dir, AddBackslash(Pf86) + 'FindX') then
    Result := 'refusing official Program Files (x86)\FindX path: ' + Dir
  else if ContainsPath(Dir, AddBackslash(Pd) + 'FindX') then
    Result := 'refusing official ProgramData\FindX path: ' + Dir
  else if ContainsPath(Dir, AddBackslash(Ad) + 'tools.findx.gui') then
    Result := 'refusing official GUI AppData path: ' + Dir
  else if ContainsPath(Dir, AddBackslash(Lad) + 'FindX-native-dev') then
    Result := 'refusing experimental FindX-native-dev path: ' + Dir;
end;

function CmdDirParam(): String;
var
  I: Integer;
  S: String;
begin
  Result := '';
  for I := 1 to ParamCount do
  begin
    S := ParamStr(I);
    if (Length(S) >= 5) and (CompareText(Copy(S, 1, 5), '/DIR=') = 0) then
    begin
      Result := RemoveQuotes(Copy(S, 6, MaxInt));
      Exit;
    end;
  end;
end;

function InitializeSetup(): Boolean;
var
  Target, Reason: String;
begin
  Result := True;
  Target := ExpandConstant('{#IsolatedDefaultDir}');
  if CmdDirParam() <> '' then
    Target := CmdDirParam();
  Reason := ForbiddenInstallDir(Target);
  if Reason <> '' then
  begin
    Log('Isolated fixture setup aborted: ' + Reason);
    if not WizardSilent then
      MsgBox('Isolated fixture setup aborted: ' + Reason, mbError, MB_OK);
    Result := False;
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Reason: String;
begin
  Result := True;
  if CurPageID = wpSelectDir then
  begin
    Reason := ForbiddenInstallDir(WizardDirValue);
    if Reason <> '' then
    begin
      MsgBox('Isolated fixture setup aborted: ' + Reason, mbError, MB_OK);
      Result := False;
    end;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  NeedsRestart := False;
  { Intentionally no taskkill, no sc.exe, no service changes. }
  Result := ForbiddenInstallDir(ExpandConstant('{app}'));
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Marker: String;
begin
  if CurStep = ssPostInstall then
  begin
    Marker := ExpandConstant('{app}\{#MyInstalledMarker}');
    if not FileExists(Marker) then
      SaveStringToFile(Marker, '1' + #13#10, False);
  end;
end;
