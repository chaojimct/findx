; FindX Inno Setup 安装脚本
; 构建命令: iscc /DMyAppVersion=1.0.0 FindX.iss

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName      "FindX"
#define MyAppPublisher "FindX"
#define MyAppURL       "https://github.com/user/findx"
#define MyAppExeName   "FindX.exe"
#define MyCliExeName   "fx.exe"
#define PublishDir     "..\publish"

[Setup]
AppId={{E4F3A1B2-7C8D-4E5F-9A6B-1D2E3F4A5B6C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
OutputBaseFilename=FindX-{#MyAppVersion}-setup
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
WizardStyle=modern
SetupLogging=yes
UninstallDisplayName={#MyAppName}
MinVersion=10.0

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Components]
Name: "service"; Description: "FindX 搜索服务（后台常驻进程）"; Types: full compact custom; Flags: fixed
Name: "cli";     Description: "命令行工具 fx"; Types: full

[Tasks]
Name: "autostart";   Description: "开机自动启动"; GroupDescription: "服务选项:"; Components: service; Flags: checkedonce
Name: "addpath";     Description: "将 fx 添加到系统 PATH"; GroupDescription: "命令行选项:"; Components: cli; Flags: checkedonce
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "快捷方式:"

[Files]
Source: "{#PublishDir}\service\*"; DestDir: "{app}"; Components: service; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#PublishDir}\cli\*"; DestDir: "{app}\cli"; Components: cli; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Components: service
Name: "{group}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
; 开机自启动
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
; 安装完成后启动服务
Filename: "{app}\{#MyAppExeName}"; Description: "启动 {#MyAppName}"; Flags: nowait postinstall skipifsilent; Components: service

[UninstallRun]
; 卸载前停止服务进程
Filename: "taskkill.exe"; Parameters: "/F /IM {#MyAppExeName}"; Flags: runhidden; RunOnceId: "KillFindX"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
procedure AddToPath(Dir: String);
var
  Path: String;
begin
  if not RegQueryStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path) then
    Path := '';
  if Pos(Lowercase(Dir), Lowercase(Path)) > 0 then
    Exit;
  if (Path <> '') and (Path[Length(Path)] <> ';') then
    Path := Path + ';';
  Path := Path + Dir;
  RegWriteStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path);
end;

procedure RemoveFromPath(Dir: String);
var
  Path, DirLower: String;
  P: Integer;
begin
  if not RegQueryStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path) then
    Exit;
  DirLower := Lowercase(Dir);
  P := Pos(DirLower, Lowercase(Path));
  if P = 0 then
    Exit;
  Delete(Path, P, Length(Dir));
  if (P <= Length(Path)) and (Path[P] = ';') then
    Delete(Path, P, 1)
  else if (P > 1) and (Path[P - 1] = ';') then
    Delete(Path, P - 1, 1);
  RegWriteStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if IsTaskSelected('addpath') then
      AddToPath(ExpandConstant('{app}\cli'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveFromPath(ExpandConstant('{app}\cli'));
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Result := '';
  Exec('taskkill.exe', '/F /IM ' + '{#MyAppExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;
