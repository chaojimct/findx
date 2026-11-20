; FindX native Windows Inno Setup (publication blocked until upgrade gates pass)
; 构建: 1) 在仓库根/ gui 中完成 tauri build --no-bundle
;       2) powershell -File installer\stage-inno.ps1
;       3) iscc /DUpgradeGatesApproved=1 /DMyAppVersion=2.5.8 /DPublishDir=<stage绝对路径> installer\FindX.iss
;       CI 建议再加 /DPublishDir=<仓库>\installer\stage 绝对路径，避免 ISCC 解析不到 stage\*
;
; 与 v1 的 FindX.iss 类似：多任务（服务注册、PATH、桌面快捷方式、安装后启动），无 .NET 检测。

#ifndef UpgradeGatesApproved
 #ifndef TxTestBuild
  #error "Upgrade gates are not approved. Run scripts/check_release.py --require-ready before enabling installer compilation."
 #endif
#endif

#ifndef MyAppVersion
  #define MyAppVersion "2.5.8"
#endif

#define MyAppName      "FindX"
#define MyAppPublisher "FindX"
#define MyAppURL       "https://github.com/chaojimct/findx"
#define MyAppExeName   "FindX.exe"
#ifndef PublishDir
#define PublishDir "stage"
#endif
#ifndef MyServiceName
#define MyServiceName  "FindX2Search"
#endif
#ifndef TxPipeName
#define TxPipeName "findx2"
#endif
; 与 GUI 内约定一致：存在该文件时首启用 ProgramData 索引 + 服务模式
#define MyInstalledMarker "FindX.installed"
; 「随系统启动」在 HKCU\...\Run 下的值名。改这里必须同步改 gui/src-tauri/src/autostart.rs
; 的 RUN_VALUE_NAME，否则卸载时清不掉。
#define MyRunValueName "FindX"

[Setup]
#ifdef TxTestBuild
AppId=FindXUpgradeTest-{#MyServiceName}
SetupMutex=Global\FindXUpgradeTest-{#MyServiceName}
PrivilegesRequiredOverridesAllowed=dialog
#else
AppId={{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C}
SetupMutex=Global\FindX.ProductUpgrade
#endif
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
#ifdef TxTestBuild
OutputBaseFilename=FindX-{#MyAppVersion}-fixture-setup
#else
OutputBaseFilename=FindX-{#MyAppVersion}-setup
#endif
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
WizardStyle=modern
SetupLogging=yes
CloseApplications=no
RestartApplications=no
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
Name: "servicec"; Description: "注册 Windows 服务（有索引时启动，无索引保持停止）"; Types: full custom; Flags: disablenouninstallwarning
Name: "clicfg";  Description: "为命令行工具配置 PATH/快捷方式等"; Types: full custom; Flags: fixed

[Tasks]
; 任务从属于 clicfg（UI 上仍在「全选」中）
Name: "addpath";     Description: "将 findx2、fx 所在安装目录加入系统 PATH (需要管理员)"; GroupDescription: "命令行:"; Components: clicfg; Flags: checkedonce
Name: "desktopicon";  Description: "创建桌面快捷方式";                            GroupDescription: "快捷方式:"; Components: clicfg; Flags: checkedonce
Name: "postlaunch";   Description: "安装完成后启动 FindX";                        GroupDescription: "安装结束:"; Components: main;   Flags: checkedonce

[Files]
Source: "{#PublishDir}\FindX.installed"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\stage-manifest.json"; DestDir: "{app}"; DestName: "payload-manifest.json"; Flags: ignoreversion; Components: main
Source: "transaction-files.psm1"; Flags: dontcopy
Source: "transaction-platform.psm1"; Flags: dontcopy
Source: "transaction-receipt.psm1"; Flags: dontcopy
Source: "upgrade-transaction.ps1"; Flags: dontcopy

; stage 由 stage-inno.ps1 生成。勿用单条 "{#PublishDir}\*"：Inno 编译器要求根目录至少有一个能匹配 * 的文件；
; 若仅有子目录（如 resources）而无根层文件，会报 No files found matching ...\stage\*。
Source: "{#PublishDir}\FindX.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
; resources 目录是可选项：tauri --no-bundle 不一定生成；GUI 运行时不强依赖（CLI/服务通过同目录 current_exe 解析）
Source: "{#PublishDir}\resources\*"; DestDir: "{app}\resources"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist; Components: main
Source: "{#PublishDir}\findx2.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\fx.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\findx2-service.exe"; DestDir: "{app}"; Flags: ignoreversion; Components: main
Source: "{#PublishDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: main

[Icons]
#ifdef TxTestBuild
Name: "{code:TxGroupRoot}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Components: main; Check: TxCreateGroup
Name: "{code:TxGroupRoot}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"; Components: main; Check: TxCreateGroup
Name: "{code:TxDesktopRoot}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; Components: main
#else
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Components: main
Name: "{group}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"; Components: main
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; Components: main
#endif

; GUI restart is transaction-owned and only follows commit/restoration.

[UninstallRun]
; No process-name force kill or hardcoded engine uninstall command.
Filename: "{sys}\sc.exe"; Parameters: "stop {#MyServiceName}"; Flags: runhidden; RunOnceId: "ScStop"
Filename: "{sys}\sc.exe"; Parameters: "delete {#MyServiceName}"; Flags: runhidden; RunOnceId: "ScDelete"

[Code]
#ifdef TxTestBuild
// Fault injection terminates ONLY this isolated test installer itself, never another process.
procedure TxTestExitProcess(Code: LongWord); external 'ExitProcess@kernel32.dll stdcall';
#endif
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

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then begin
    RemoveFromPath(ExpandConstant('{app}'));
    // 「随系统启动」写的是 HKCU\...\Run 下的 FindX 值（见 gui/src-tauri/src/autostart.rs）。
    // 不删的话，卸载后每次登录 Windows 都会去拉起一个已经不存在的 exe，弹找不到文件的错误。
    // RegDeleteValue 对不存在的值返回非 0，但这里是"本来就没开"的正常情况，忽略返回值。
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#MyRunValueName}');
  end;
end;


var
  TxRoot: String;
  TxStarted, TxPrepared, TxCommitted: Boolean;

function TxQuote(const Value: String): String;
begin
  if Pos('"', Value) > 0 then RaiseException('Invalid transaction argument');
  Result := '"' + Value + '"';
end;

function TxDataRoot: String;
begin
#ifdef TxTestBuild
  Result := ExpandConstant('{param:TXDATA|}');
#else
  Result := ExpandConstant('{commonappdata}\FindX');
#endif
end;

function TxNativeConfig: String;
begin
#ifdef TxTestBuild
  Result := ExpandConstant('{param:TXNATIVE|}');
#else
  Result := ExpandConstant('{localappdata}\FindX-native-dev');
#endif
end;

function TxLegacyConfig: String;
begin
#ifdef TxTestBuild
  Result := ExpandConstant('{param:TXLEGACY|}');
#else
  Result := ExpandConstant('{userappdata}\tools.findx.gui');
#endif
end;

function TxCreateGroup: Boolean;
begin
  Result := not WizardNoIcons;
end;
function TxGroupRoot(Param: String): String;
begin
#ifdef TxTestBuild
  Result := ExpandConstant('{param:TXGROUP|}');
#else
  Result := ExpandConstant('{group}');
#endif
end;
function TxDesktopRoot(Param: String): String;
begin
#ifdef TxTestBuild
  Result := ExpandConstant('{param:TXDESKTOP|}');
#else
  Result := ExpandConstant('{autodesktop}');
#endif
end;

function TxCall(const Phase: String): Boolean;
var
  Args, Fault: String;
  ErrorText: AnsiString;
  Code: Integer;
begin
  Args := '-NoProfile -NonInteractive -File ' + TxQuote(ExpandConstant('{tmp}\upgrade-transaction.ps1')) +
    ' -Phase ' + Phase + ' -TransactionRoot ' + TxQuote(TxRoot);
  if Phase = 'Prepare' then begin
    Args := Args + ' -InstallRoot ' + TxQuote(ExpandConstant('{app}')) +
      ' -DataRoot ' + TxQuote(TxDataRoot) + ' -NativeConfigRoot ' + TxQuote(TxNativeConfig) +
      ' -LegacyConfigRoot ' + TxQuote(TxLegacyConfig) + ' -ServiceName {#MyServiceName}' +
      ' -PipeName {#TxPipeName} -ExpectedVersion {#MyAppVersion} -InstallService ';
    if WizardIsComponentSelected('servicec') then Args := Args + '1' else Args := Args + '0';
    Args := Args + ' -AddPath ';
    if WizardIsTaskSelected('addpath') then Args := Args + '1' else Args := Args + '0';
#ifdef TxTestBuild
    Args := Args + ' -UninstallKey ' + TxQuote('FindXUpgradeTest-{#MyServiceName}_is1');
#else
    Args := Args + ' -UninstallKey ' + TxQuote('{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C}_is1');
#endif
    Args := Args + ' -GroupRoot ' + TxQuote(TxGroupRoot('')) + ' -DesktopRoot ' + TxQuote(TxDesktopRoot('')) + ' -CreateGroup ';
    if WizardNoIcons then Args := Args + '0' else Args := Args + '1';
#ifdef TxTestBuild
    Fault := ExpandConstant('{param:TXFAULT|none}');
    Args := Args + ' -TestBuild -Fault ' + TxQuote(Fault);
    if ExpandConstant('{param:TXIMAGE|}') <> '' then
      Args := Args + ' -TestVolumeImage ' + TxQuote(ExpandConstant('{param:TXIMAGE|}')) + ' -TimeoutSeconds 20'
    else Args := Args + ' -TimeoutSeconds 3';
#else
    Args := Args + ' -ServiceStopSeconds 180';
#endif
  end;
  Result := Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'), Args, '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
  if not Result then begin
    ErrorText := '';
    LoadStringFromFile(TxRoot + '\failure.txt', ErrorText);
    Log('FindX transaction ' + Phase + ' failed: ' + String(ErrorText));
  end;
end;

function TxFailureMessage: String;
var
  Code, Text: AnsiString;
  Kind: String;
begin
  Result := '升级没有开始，安装内容未改动。';
  Kind := '';
  if LoadStringFromFile(TxRoot + '\reason.code', Code) then Kind := Trim(String(Code));
  if Kind = 'legacy-save-unconfirmed' then
    Result := '请先正常退出 FindX、停止旧服务后重试。旧版无保存回执，本次未改动。'
  else if Kind = 'gui-quit-unavailable' then
    Result := '未能让已安装的 FindX 正常退出。请从托盘选择「退出 FindX（不停止索引服务）」后重新安装。本次未改动。'
  else if Kind = 'gui-settings-open' then
    Result := '请先关闭「FindX 设置」窗口后再安装。本次未改动。'
  else if Kind = 'upgrade-lock-held' then
    Result := '上一次升级仍在收尾，安装锁被占用。请稍后再试，不要删除锁文件。本次未改动。'
  else if Kind = 'upgrade-owner-live' then
    Result := '已有安装程序正在升级 FindX，请等待它结束后再试。本次未改动。'
  else if Kind = 'upgrade-foreign' then
    Result := '中断的升级属于其他用户或安装目录，已保留现场。本次未改动。'
  else if Kind = 'upgrade-recover-failed' then
    Result := '中断的升级没能安全回滚，已保留现场。本次未改动。'
  else if LoadStringFromFile(TxRoot + '\failure.txt', Text) then
    Result := Copy(UTF8Decode(Text), 1, 240);
  Result := Result + #13#10 + '日志: ' + TxRoot;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if TxStarted then begin Result := '这次安装已经尝试过升级。现场已保留，请查看日志后重试。'; Exit; end;
#ifdef TxTestBuild
  if GetEnv('GITHUB_ACTIONS') <> 'true' then begin Result := 'Test build runs only on the isolated CI runner.'; Exit; end;
  TxRoot := ExpandConstant('{param:TXROOT|}');
#else
  TxRoot := ExpandConstant('{commonappdata}\FindX-upgrades\') + GetDateTimeString('yyyymmddhhnnsszzz', #0, #0) ;
#endif
  ExtractTemporaryFile('transaction-files.psm1');
  ExtractTemporaryFile('transaction-platform.psm1');
  ExtractTemporaryFile('transaction-receipt.psm1');
  ExtractTemporaryFile('upgrade-transaction.ps1');
  TxStarted := True;
  TxPrepared := TxCall('Prepare');
  if not TxPrepared then begin
    Result := TxFailureMessage;
#ifndef TxTestBuild
    { MsgBox stays visible under /SUPPRESSMSGBOXES. Show one short sentence, never a stack. }
    if WizardSilent then MsgBox(Result, mbError, MB_OK);
#endif
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  if CurStep = ssPostInstall then begin
#ifdef TxTestBuild
    if ExpandConstant('{param:TXFAULT|none}') = 'interrupt-after-copy' then TxTestExitProcess(73);
#endif
    if not TxCall('Verify') then RaiseException('升级校验未通过，已要求回滚。日志: ' + TxRoot);
    { PATH remains selected behavior, but runs before commit; registration/Run rollback
      values, types and security are captured before replacement. }
    if not TxCall('Commit') then RaiseException('升级提交失败。日志: ' + TxRoot);
    TxCommitted := True;
  end;
end;

procedure DeinitializeSetup;
var
  Code: Integer;
  RestartBytes: AnsiString;
  RestartPath: String;
begin
  if TxPrepared and not TxCommitted then begin
    if not TxCall('Rollback') then Log('RECOVERY REQUIRED: retained backup at ' + TxRoot);
  end;
  { Only restart a previously-running GUI, in its original user context. }
  if TxStarted and LoadStringFromFile(TxRoot + '\restart-original-user-gui.flag', RestartBytes) then begin
    RestartPath := Utf8Decode(RestartBytes);
    if (Pos('"', RestartPath) = 0) and FileExists(RestartPath) then
      ExecAsOriginalUser(RestartPath, '', ExtractFileDir(RestartPath), SW_SHOWNORMAL, ewNoWait, Code);
  end;
end;

// CurStepChanged exceptions alone do NOT make Inno's exit code nonzero.
// Never let an updater interpret a rolled-back installation as successful.
function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if TxStarted and not TxCommitted then Result := 80;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and TxCommitted and FileExists(TxRoot + '\service-uninitialized.flag') then
    WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10 +
      '尚无索引，服务已注册并保持停止。请在 FindX 设置中初始化索引后启动。';
end;
