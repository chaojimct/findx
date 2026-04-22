; FindX 安装/卸载：复制 CLI/服务、注册 Windows 服务、写安装标记供 GUI 首启读取默认项。
; 与 Tauri 2 NSIS 模板配合，通过 !macrodef NSIS_HOOK_* 注入。
; 若未找到 resources\bin\findx2-service.exe（残缺包），则跳过整段逻辑，避免误写标记。

!macro NSIS_HOOK_POSTINSTALL
  IfFileExists "$INSTDIR\resources\bin\findx2-service.exe" 0 postinstall_done
  ; 1) 将 Tauri 打进 resources 目录的二进制解到与主程序同层（与 findx_settings 约定一致）
  CopyFiles /SILENT "$INSTDIR\resources\bin\findx2.exe" "$INSTDIR\findx2.exe"
  CopyFiles /SILENT "$INSTDIR\resources\bin\fx.exe" "$INSTDIR\fx.exe"
  CopyFiles /SILENT "$INSTDIR\resources\bin\findx2-service.exe" "$INSTDIR\findx2-service.exe"

  ; 2) 本机“正式安装”标记：有该文件时 GUI 首次会采用 ProgramData 索引 + 服务模式（见 findx_settings）
  IfFileExists "$INSTDIR\FindX.installed" 0 +1
  Delete "$INSTDIR\FindX.installed"
  FileOpen $0 "$INSTDIR\FindX.installed" w
  FileWrite $0 "1"
  FileClose $0

  ; 3) 共享索引目录 + 注册并启动服务（与 findx2-service install 使用的 index 一致）
  CreateDirectory "$PROGRAMDATA\FindX"
  ReadEnvStr $9 PROGRAMDATA
  nsExec::ExecToLog "sc stop FindX2Search"
  Pop $0
  nsExec::ExecToLog '"$INSTDIR\findx2-service.exe" uninstall'
  Pop $0
  ExecWait '"$INSTDIR\findx2-service.exe" install --index $9\FindX\index.bin' $1
  nsExec::ExecToLog "sc start FindX2Search"
  Pop $0
  postinstall_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  IfFileExists "$INSTDIR\findx2-service.exe" 0 +1
  nsExec::ExecToLog "sc stop FindX2Search"
  Pop $0
  IfFileExists "$INSTDIR\findx2-service.exe" 0 +1
  nsExec::ExecToLog '"$INSTDIR\findx2-service.exe" uninstall'
  Pop $0
!macroend
