; FindX Inno Setup 安装脚本
; 构建命令: iscc /DMyAppVersion=1.0.0 FindX.iss

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName      "FindX"
#define MyAppPublisher "FindX"
#define MyAppURL       "https://github.com/chaojimct/findx"
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
SetupIconFile=..\assets\findx-icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
MinVersion=10.0

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Components]
Name: "service"; Description: "FindX 搜索服务（后台常驻进程）"; Types: full compact custom; Flags: fixed
Name: "cli";     Description: "命令行工具 fx"; Types: full

[Tasks]
Name: "autostart_task"; Description: "任务计划程序（推荐，以管理员权限运行，支持 USN 快速索引）"; GroupDescription: "开机自动启动:"; Components: service; Flags: exclusive checkedonce
Name: "autostart_reg";  Description: "注册表启动（普通权限，索引速度较慢）"; GroupDescription: "开机自动启动:"; Components: service; Flags: exclusive unchecked
Name: "autostart_none"; Description: "不自动启动"; GroupDescription: "开机自动启动:"; Components: service; Flags: exclusive unchecked
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
; 仅在选择注册表模式时写入自启动项
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"""; Flags: uninsdeletevalue; Tasks: autostart_reg

[Run]
; 安装完成后启动服务
Filename: "{app}\{#MyAppExeName}"; Description: "启动 {#MyAppName}"; Flags: nowait postinstall skipifsilent shellexec; Components: service

[UninstallRun]
; 卸载前停止服务进程
Filename: "taskkill.exe"; Parameters: "/F /IM {#MyAppExeName}"; Flags: runhidden; RunOnceId: "KillFindX"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]

// ── 任务计划程序 ──

procedure CreateScheduledTask;
var
  ExePath, Params: String;
  ResultCode: Integer;
begin
  ExePath := ExpandConstant('{app}\{#MyAppExeName}');
  Params := '/Create /TN "FindX" /TR "\"' + ExePath + '\"" /SC ONLOGON /RL HIGHEST /F';
  Exec('schtasks.exe', Params, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

procedure RemoveScheduledTask;
var
  ResultCode: Integer;
begin
  Exec('schtasks.exe', '/Delete /TN "FindX" /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

// ── PATH 管理 ──

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

// ── 安装/卸载钩子 ──

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if IsTaskSelected('autostart_task') then
    begin
      CreateScheduledTask;
      // 从注册表模式切换过来时，清除旧的注册表自启动项
      RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#MyAppName}');
    end
    else if IsTaskSelected('autostart_reg') then
    begin
      // 从任务计划模式切换过来时，清除旧的计划任务
      RemoveScheduledTask;
    end
    else if IsTaskSelected('autostart_none') then
    begin
      RemoveScheduledTask;
      RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#MyAppName}');
    end;

    if IsTaskSelected('addpath') then
      AddToPath(ExpandConstant('{app}\cli'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveFromPath(ExpandConstant('{app}\cli'));
    RemoveScheduledTask;
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#MyAppName}');
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Result := '';
  Exec('taskkill.exe', '/F /IM ' + '{#MyAppExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;
