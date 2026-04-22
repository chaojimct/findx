; FindX 2 (Tauri) Inno Setup
; 构建: 1) 在仓库根/ gui 中完成 tauri build --no-bundle
;       2) powershell -File installer\stage-inno.ps1
;       3) iscc /DMyAppVersion=x.y.z installer\FindX.iss
;
; 与 v1 的 FindX.iss 类似：多任务（服务注册、PATH、桌面快捷方式、安装后启动），无 .NET 检测。

#ifndef MyAppVersion
  #define MyAppVersion "2.0.4"
#endif

#define MyAppName      "FindX"
#define MyAppPublisher "FindX"
#define MyAppURL       "https://github.com/chaojimct/findx"
#define MyAppExeName   "FindX.exe"
#define PublishDir     "stage"
#define MyServiceName  "FindX2Search"
; 与 GUI 内约定一致：存在该文件时首启用 ProgramData 索引 + 服务模式
#define MyInstalledMarker "FindX.installed"

[Setup]
AppId={{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C}
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
; 本脚本相对路径以 installer/ 为基准
SetupIconFile=..\gui\src-tauri\icons\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
MinVersion=10.0

[Languages]
; 随仓库提供 .isl，避免 CI（如 choco innosetup）未带 Languages 子目录导致 ISCC 找不到 compiler: 下文件
Name: "chinesesimplified"; MessagesFile: "Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "Languages\Default.isl"

[Types]
Name: "full"; Description: "完全安装 (推荐)"
; 仅 Flags: iscustom 为官方语法；勿写 IsCustom: true（旧版 ISCC 会报 Unrecognized parameter name）
Name: "custom"; Description: "自定义"; Flags: iscustom

[Components]
; 与 v1 一样保留「主程序 / 服务 / CLI 路径」三段的语义，实际文件同一目录一次拷入
Name: "main";     Description: "FindX 程序、资源与 findx2 / fx 命令行工具"; Types: full custom; Flags: fixed
; 官方标志为 disablenouninstallwarning，勿写 disablenouninstall（旧版 ISCC 报 unknown flag）
Name: "servicec"; Description: "注册并启动 Windows 服务 {#MyServiceName}（推荐；索引在 ProgramData\FindX）"; Types: full custom; Flags: disablenouninstallwarning
Name: "clicfg";  Description: "为命令行工具配置 PATH/快捷方式等"; Types: full custom; Flags: fixed

[Tasks]
; 任务从属于 clicfg（UI 上仍在「全选」中）
Name: "addpath";     Description: "将 findx2、fx 所在安装目录加入系统 PATH (需要管理员)"; GroupDescription: "命令行:"; Components: clicfg; Flags: checkedonce
Name: "desktopicon";  Description: "创建桌面快捷方式";                            GroupDescription: "快捷方式:"; Components: clicfg; Flags: checkedonce
Name: "postlaunch";   Description: "安装完成后启动 FindX";                        GroupDescription: "安装结束:"; Components: main;   Flags: checkedonce

[Files]
; stage 由 stage-inno.ps1 从 target\release 生成
Source: "{#PublishDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; Components: main

[Icons]
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"; Components: main
Name: "{group}\卸载 {#MyAppName}";     Filename: "{uninstallexe}";    Components: main
Name: "{autodesktop}\{#MyAppName}";   Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; Components: main

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "启动 {#MyAppName}"; Flags: nowait postinstall skipifsilent shellexec; Tasks: postlaunch; Components: main
Filename: "{app}\{#MyAppExeName}";   Flags: nowait postinstall shellexec; Check: WizardSilent; Tasks: postlaunch; Components: main

[UninstallRun]
; 先停服务再杀进程、最后 uninstall（卸载器仍应能找到 {app}\findx2-service.exe）
Filename: "{sys}\sc.exe";            Parameters: "stop {#MyServiceName}";  Flags: runhidden; RunOnceId: "ScStop"
Filename: "{sys}\taskkill.exe";     Parameters: "/F /IM findx2-service.exe"; Flags: runhidden; RunOnceId: "KillSvc"
Filename: "{sys}\taskkill.exe";     Parameters: "/F /IM {#MyAppExeName}"; Flags: runhidden; RunOnceId: "KillGui"
Filename: "{app}\findx2-service.exe"; Parameters: "uninstall";      Flags: runhidden; RunOnceId: "SvcUninstall"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
// ── 系统 PATH（与 v1 风格一致，HKLM）──
procedure AddToPath(const Dir: String);
var
  Path: String;
begin
  if not RegQueryStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path) then
    Path := '';
  if Pos(LowerCase(Dir), LowerCase(Path)) > 0 then
    Exit;
  if (Path <> '') and (Path[Length(Path)] <> ';') then
    Path := Path + ';';
  Path := Path + Dir;
  RegWriteStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path);
end;

procedure RemoveFromPath(const Dir: String);
var
  Path, Dl: String;
  P: Integer;
begin
  if not RegQueryStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path) then
    Exit;
  Dl := LowerCase(Dir);
  P := Pos(Dl, LowerCase(Path));
  if P = 0 then
    Exit;
  Delete(Path, P, Length(Dir));
  if (P <= Length(Path)) and (Path[P] = ';') then
    Delete(Path, P, 1)
  else if (P > 1) and (Path[P - 1] = ';') then
    Delete(Path, P - 1, 1);
  RegWriteStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Path);
end;

// ── 安装结束：服务 + 标记 + PATH ──
procedure CurStepChanged(CurStep: TSetupStep);
var
  R: Integer;
  IndexPath, AppDir, Marker, Params: String;
begin
  if CurStep = ssPostInstall then
  begin
    AppDir := ExpandConstant('{app}');
    if IsTaskSelected('addpath') then
      AddToPath(AppDir);

    if IsComponentSelected('servicec') then
    begin
      ForceDirectories(ExpandConstant('{commonappdata}') + '\FindX');
      IndexPath := ExpandConstant('{commonappdata}') + '\FindX\index.bin';
      Exec(ExpandConstant('{app}\findx2-service.exe'), 'uninstall', '', SW_HIDE, ewWaitUntilTerminated, R);
      Params := 'install --index ' + #34 + IndexPath + #34;
      Exec(ExpandConstant('{app}\findx2-service.exe'), Params, '', SW_HIDE, ewWaitUntilTerminated, R);
      Exec(ExpandConstant('{sys}\cmd.exe'), '/c sc start FindX2Search', '', SW_HIDE, ewWaitUntilTerminated, R);
    end;

    { 与 GUI findx_settings 约定：仅在选择安装服务时写入，使首启用 ProgramData 索引 + 服务模式 }
    if IsComponentSelected('servicec') then
    begin
      Marker := AppDir + '\{#MyInstalledMarker}';
      SaveStringToFile(Marker, '1' + #13#10);
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;

// 与 v1 一样：有旧版本占用时强杀
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#MyAppExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM findx2-service.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
