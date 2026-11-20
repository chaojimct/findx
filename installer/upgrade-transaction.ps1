# Called by the official Inno script. No force-stop or process-name termination.
[CmdletBinding()]
param(
 [Parameter(Mandatory=$true)][ValidateSet('Prepare','Verify','Commit','Rollback','RequestQuit')][string]$Phase,
 [Parameter(Mandatory=$true)][string]$TransactionRoot,
 [string]$InstallRoot,[string]$DataRoot,[string]$NativeConfigRoot,[string]$LegacyConfigRoot,
 [string]$ServiceName='FindX2Search',[string]$PipeName='findx2',[string]$ExpectedVersion,
 [int]$InstallService=0,[int]$TimeoutSeconds=12,[int]$ServiceStopSeconds=0,[int]$AddPath=0,[string]$UninstallKey,[string]$GroupRoot,[string]$DesktopRoot,[int]$CreateGroup=0,
 [string]$TestVolumeImage='',
 [switch]$TestBuild,[switch]$QuitReporter,[string]$QuitReport,[ValidateSet('none','backup','health','stop-timeout','interrupt-after-copy','post-platform')][string]$Fault='none'
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
# Must stay identical to installer/FindX.iss Source/ExtractTemporaryFile helper set.
$script:FxTxPackagedHelpers=@('stop-mode.ps1','transaction-files.psm1','transaction-platform.psm1','transaction-receipt.psm1','upgrade-transaction.ps1')
function Write-FxTxBootstrapFailure($Record){
 # Runs before any transaction directory exists. Keep the original exception;
 # a later generic dialog is how 22:53 lost this parse error.
 $message=[string]$Record.Exception.Message
 if(-not $message){$message=[string]$Record}
 $detail=$Record|Out-String
 $utf8=[Text.UTF8Encoding]::new($false)
 try{
  if($TransactionRoot -and -not [IO.Directory]::Exists($TransactionRoot)){[void][IO.Directory]::CreateDirectory($TransactionRoot)}
  if($TransactionRoot -and [IO.Directory]::Exists($TransactionRoot)){
   [IO.File]::WriteAllText((Join-Path $TransactionRoot 'failure.txt'),$message,$utf8)
   [IO.File]::WriteAllText((Join-Path $TransactionRoot 'failure.detail.txt'),$detail,$utf8)
   [IO.File]::WriteAllText((Join-Path $TransactionRoot 'reason.code'),'bootstrap',$utf8)
  }
 }catch{}
 try{
  if($QuitReport){
   $qdir=[IO.Path]::GetDirectoryName($QuitReport)
   if($qdir -and [IO.Directory]::Exists($qdir)){
    [IO.File]::WriteAllText((Join-Path $qdir 'failure.txt'),$message,$utf8)
    [IO.File]::WriteAllText((Join-Path $qdir 'failure.detail.txt'),$detail,$utf8)
    [IO.File]::WriteAllText((Join-Path $qdir 'reason.code'),'bootstrap',$utf8)
    $payload=[ordered]@{exited=$false;requested=$false;reason='bootstrap';detail=$message}
    [IO.File]::WriteAllText($QuitReport,($payload|ConvertTo-Json -Compress),$utf8)
   }
  }
 }catch{}
}
try{
 Add-Type -AssemblyName System.ServiceProcess
 Import-Module (Join-Path $PSScriptRoot 'transaction-files.psm1') -Force -Global
 Import-Module (Join-Path $PSScriptRoot 'transaction-platform.psm1') -Force -Global
 Import-Module (Join-Path $PSScriptRoot 'transaction-receipt.psm1') -Force -Global
}catch{ Write-FxTxBootstrapFailure $_; exit 1 }
$script:s=$null; $script:lock=$null; $script:reasonCode=''; $script:quitReporter=[bool]$QuitReporter
function Save-State {
 if($script:quitReporter){return}
 $temp=Join-Path $TransactionRoot ('journal.'+[guid]::NewGuid().ToString('N')+'.tmp')
 $bytes=[Text.UTF8Encoding]::new($false).GetBytes(($script:s | ConvertTo-Json -Depth 60))
 $f=[IO.File]::Open($temp,'CreateNew','Write','None'); try{$f.Write($bytes,0,$bytes.Length);$f.Flush($true)}finally{$f.Dispose()}
 $dest=Join-Path $TransactionRoot 'journal.json'
 if([IO.File]::Exists($dest)){[IO.File]::Replace($temp,$dest,(Join-Path $TransactionRoot ('journal.previous.'+[guid]::NewGuid().ToString('N')+'.json')))}else{[IO.File]::Move($temp,$dest)}
}
function Set-State([string]$name){$script:s.state=$name;Save-State}
function Get-InstallerOwner {
 $parent=(Get-CimInstance Win32_Process -Filter "ProcessId=$PID").ParentProcessId
 $p=Get-Process -Id $parent -ErrorAction Stop
 return [pscustomobject]@{pid=[int]$p.Id;startTicks=$p.StartTime.ToUniversalTime().Ticks;sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value}
}
function Test-InstallerOwnerAlive($Owner) {
 $p=Get-Process -Id $Owner.pid -ErrorAction SilentlyContinue
 return $p -and $p.StartTime.ToUniversalTime().Ticks -eq [long]$Owner.startTicks
}
function Open-ExclusiveFile([string]$Path){
 # PowerShell wraps a sharing violation as MethodInvocationException. Only an
 # IOException means the file is busy. Never delete it and never kill the owner.
 try{return [IO.File]::Open($Path,'OpenOrCreate','ReadWrite','None')}catch{
  $ex=$_.Exception
  while($ex){ if($ex -is [IO.IOException]){ return $null }; $ex=$ex.InnerException }
  throw
 }
}
function Open-PhaseLock([string]$Root){ return Open-ExclusiveFile (Join-Path $Root 'phase.lock') }
function Resolve-UpgradeLease {
 param([string]$State,[bool]$OwnerAlive,[bool]$SameIdentity,[bool]$LockBusy)
 if($State -in @('committed','rolled_back')){return 'replace'}
 if(-not $SameIdentity){return 'refuse-foreign'}
 if($OwnerAlive){return 'refuse-live-owner'}
 if($LockBusy){return 'refuse-lock-held'}
 return 'recover-and-continue'
}
function Get-LeaseRefusalMessage([string]$Action){
 switch($Action){
  'refuse-foreign' {return '中断的升级属于其他用户或安装目录，已保留现场。本次未改动。'}
  'refuse-live-owner' {return '已有安装程序正在升级 FindX，请等待它结束后再试。本次未改动。'}
  'refuse-lock-held' {return '上一次升级仍在收尾，安装锁被占用。请稍后再试，不要删除锁文件。本次未改动。'}
  'recover-failed' {return '中断的升级没能安全回滚，已保留现场。本次未改动。'}
  default {return '升级准备已停止。本次未改动。'}
 }
}
function Set-LeaseReason([string]$Action){
 switch($Action){
  'refuse-foreign' {$script:reasonCode='upgrade-foreign'}
  'refuse-live-owner' {$script:reasonCode='upgrade-owner-live'}
  'refuse-lock-held' {$script:reasonCode='upgrade-lock-held'}
  'recover-failed' {$script:reasonCode='upgrade-recover-failed'}
  'gui-quit' {$script:reasonCode='gui-quit-unavailable'}
  'gui-settings' {$script:reasonCode='gui-settings-open'}
 }
}
function Get-ShippedQuitContract {
 [pscustomobject]@{
  trayCaption='退出 FindX（不停止索引服务）'
  normalQuitMessage='FindX.Native.NormalQuit.v1'
  trayMessage='FindX.Native.Tray.v1'
  settingsTitle='FindX 设置'
  mainTitle='FindX'
  published252='392CEA03063D4B1AC57E62D5EDCBA45291FFE36E16C49BAD472298D2B51C9A02'
 }
}
function Get-NormalQuitPlan([string]$ProductVersion,[string]$ImageHash){
 $contract=Get-ShippedQuitContract
 $steps=New-Object System.Collections.Generic.List[string]
 if($ProductVersion -eq '2.5.2' -and $ImageHash -eq $contract.published252){$steps.Add('legacy-252-close');return @($steps)}
 if($ProductVersion -ne '2.5.2'){$steps.Add('normal-quit-message');$steps.Add('shipped-tray-menu')}
 if($steps.Count -eq 0){$steps.Add('refuse')}
 return @($steps)
}
function Find-QuitMenuIndex([string[]]$Items){
 $want=(Get-ShippedQuitContract).trayCaption
 $hits=@()
 for($i=0;$i -lt @($Items).Count;$i++){ if([string]$Items[$i] -ceq $want){ $hits+=$i } }
 if($hits.Count -ne 1){ return -1 }
 return [int]$hits[0]
}
function Get-MenuActivationKeys([int]$Index){
 if($Index -lt 0){ throw 'no quit item' }
 $keys=New-Object System.Collections.Generic.List[string]
 for($i=0;$i -le $Index;$i++){ $keys.Add('DOWN') }
 $keys.Add('RETURN')
 return @($keys)
}
function Enter-InstallerLease {
 $leasePath=Join-Path $script:leaseStore 'active.json'
 $guard=Open-ExclusiveFile (Join-Path $script:leaseStore 'active.lock')
 if(-not $guard){ Set-LeaseReason 'refuse-lock-held'; throw (Get-LeaseRefusalMessage 'refuse-lock-held') }
 try {
  $me=Get-InstallerOwner
  $savedNew=$script:s
  if([IO.File]::Exists($leasePath)){
   $lease=Get-Content -LiteralPath $leasePath -Raw -Encoding UTF8 | ConvertFrom-Json
   if($lease.schema -ne 1){Set-LeaseReason 'recover-failed'; throw (Get-LeaseRefusalMessage 'recover-failed')}
   $oldRoot=Get-FxTxPath $lease.root;Assert-FxTxPrivateDirectory $oldRoot
   $journal=Join-Path $oldRoot 'journal.json'
   if(-not [IO.File]::Exists($journal) -or (Get-Item -LiteralPath $journal).Length -gt 67108864){Set-LeaseReason 'recover-failed'; throw (Get-LeaseRefusalMessage 'recover-failed')}
   $old=Get-Content -LiteralPath $journal -Raw -Encoding UTF8 | ConvertFrom-Json
   if($old.schema -ne 2){Set-LeaseReason 'recover-failed'; throw (Get-LeaseRefusalMessage 'recover-failed')}
   $oldLock=$null
   $action='replace'
   if([string]$old.state -notin @('committed','rolled_back')){
    $same=([string]$lease.owner.sid -ceq [string]$me.sid) -and ([string]$old.roots.app -ieq [string]$script:s.roots.app)
    $alive=Test-InstallerOwnerAlive $lease.owner
    $oldLock=Open-PhaseLock $oldRoot
    $action=Resolve-UpgradeLease ([string]$old.state) ([bool]$alive) ([bool]$same) ($null -eq $oldLock)
   }
   if($action -eq 'recover-and-continue'){
    $recovered=$false
    $requested=$TransactionRoot
    try{
     $TransactionRoot=$oldRoot;$script:s=$old;$script:s.installerOwner=$me
     Write-FxTxUpdateJson $leasePath ([ordered]@{schema=1;root=$oldRoot;owner=$me})
     Save-State;Invoke-Rollback
     if([string]$script:s.state -ne 'rolled_back'){Set-LeaseReason 'recover-failed'; throw (Get-LeaseRefusalMessage 'recover-failed')}
     $restart=Join-Path $oldRoot 'restart-original-user-gui.flag'
     if([IO.File]::Exists($restart)){[IO.File]::Copy($restart,(Join-Path $requested 'restart-original-user-gui.flag'),$false)}
     $recovered=$true
    }finally{
     if($oldLock){$oldLock.Dispose()}
     $TransactionRoot=$requested
     if(-not $recovered){$script:s=$savedNew}
    }
    $script:s=$savedNew
   }elseif($action -ne 'replace'){
    if($oldLock){$oldLock.Dispose()}
    Set-LeaseReason $action
    throw (Get-LeaseRefusalMessage $action)
   }elseif($oldLock){$oldLock.Dispose()}
  }
  $script:s.installerOwner=$me;Save-State
  Write-FxTxUpdateJson $leasePath ([ordered]@{schema=1;root=$TransactionRoot;owner=$me})
 }finally{if($guard){$guard.Dispose()}}
}
function Assert-InstallerLease {
 $lease=Get-Content -LiteralPath (Join-Path $script:leaseStore 'active.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 $me=Get-InstallerOwner
 if($lease.schema -ne 1 -or $lease.root -ine $TransactionRoot -or $lease.owner.pid -ne $me.pid -or [long]$lease.owner.startTicks -ne $me.startTicks){throw 'This installer does not own the active transaction'}
}

function Get-ExactService {
 $svc=Get-CimInstance Win32_Service -Filter ("Name='"+$script:s.serviceName+"'")
 if($svc){
  # This release does not migrate an unexpected service command or account.
  $expected=Join-Path $script:s.roots.app 'findx2-service.exe'
  if(-not $svc.PathName.StartsWith('"'+$expected+'" ',[StringComparison]::OrdinalIgnoreCase)){throw 'Unexpected service image/arguments: refusing to change it'}
  if($svc.ServiceType -ne 'Own Process'){throw 'Shared-process service is unsupported'}
  Assert-FxTxServiceScope $svc.PathName $expected (Join-Path $script:s.roots.data 'index.bin') $script:s.pipeName
 }
 return $svc
}
function Wait-ServiceStopped([int]$oldPid) {
 $until=[DateTime]::UtcNow.AddSeconds($script:s.timeoutSeconds)
 do{
  $v=Get-ExactService
  $process=if($oldPid){Get-Process -Id $oldPid -ErrorAction SilentlyContinue}else{$null}
  if(((-not $v) -or $v.State -eq 'Stopped') -and -not $process){return}
  Start-Sleep -Milliseconds 100
 }while([DateTime]::UtcNow -lt $until)
 throw 'Graceful stop timeout; no force-stop was attempted'
}
function Stop-ExactService {
 $v=Get-ExactService
 if($v -and $v.State -ne 'Stopped'){
  $controller=[ServiceProcess.ServiceController]::new($script:s.serviceName)
  try{if($v.State -ne 'Stop Pending'){$controller.Stop()}}finally{$controller.Dispose()}
  Wait-ServiceStopped ([int]$v.ProcessId)
 }
}
function Resolve-UpgradeServiceStart {
 param([bool]$Exists,[bool]$WasRunning,[string]$StartMode,[string]$State)
 if(-not $Exists){ return 'skip-absent' }
 if($WasRunning){
  if($State -ne 'Stopped'){ throw 'Original running-service handoff changed before candidate start' }
  return 'start-prior-running'
 }
 # A successful upgrade of the standard automatic service must not preserve a
 # pre-install STOPPED state. Manual and disabled services stay stopped.
 if(($StartMode -eq 'Auto' -or $StartMode -eq 'Automatic') -and $State -eq 'Stopped'){ return 'start-automatic' }
 return 'leave-stopped'
}
function Resolve-RollbackRuntime {
 param([bool]$WasRunning,[string]$State)
 if($WasRunning -and $State -eq 'Stopped'){ return 'start' }
 if($WasRunning -and $State -ne 'Running'){ throw 'Original service is still transitioning; runtime recovery not confirmed' }
 # Includes an automatic service this upgrade started and rollback already stopped.
 return 'leave-stopped'
}
function Restore-OriginalRuntime {
 $v=Get-ExactService
 if($script:s.original.serviceExists -and -not $v){throw 'Original service registration missing; runtime recovery not confirmed'}
 if($script:s.PSObject.Properties.Name -contains 'terminalHandoff' -and $script:s.terminalHandoff -and $script:s.terminalHandoff.stage -notin @('stopped','cancelled')){throw 'Terminal handoff remains uncertain/committed and sealed; RAM retained, runtime restoration NOT claimed'}
 $state=if($v){[string]$v.State}else{'Missing'}
 $runtime=Resolve-RollbackRuntime ([bool]$script:s.original.serviceRunning) $state
 if($runtime -eq 'start'){
  Start-Service -Name $script:s.serviceName
  $controller=[ServiceProcess.ServiceController]::new($script:s.serviceName)
  try{$controller.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Running,[TimeSpan]::FromSeconds($script:s.timeoutSeconds))}finally{$controller.Dispose()}
  Wait-NewServiceReady
 }
 $v=Get-ExactService
 if($script:s.original.serviceRunning -and $v -and $v.State -ne 'Running'){throw 'Original service is still transitioning; runtime recovery not confirmed'}
 if($script:s.original.guiRunning -and $script:s.original.guiExitRequested -and (Get-InstalledGui).Count -eq 0){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'restart-original-user-gui.flag'),(Join-Path $script:s.roots.app 'FindX.exe'))}
}
function Get-InstalledGui {
 $path=Join-Path $script:s.roots.app 'FindX.exe'
 return ,@(Get-CimInstance Win32_Process -Filter "Name='FindX.exe'" | Where-Object {$_.ExecutablePath -and $_.ExecutablePath.Equals($path,[StringComparison]::OrdinalIgnoreCase)})
}
function Request-Legacy252NormalClose($Target) {
 # The shipped 2.5.2 native GUI has no tray/Quit menu. Its last normal window
 # close calls cx.quit (v2.5.2 ui/mod.rs96-101). Never apply this to newer GUIs
 # whose Close button may only hide to tray. Exact published-build allowlist.
 $image=Join-Path $script:s.roots.app 'FindX.exe'
 if((Get-Item -LiteralPath $image).VersionInfo.ProductVersion -cne '2.5.2' -or
    (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash -ine '392cea03063d4b1ac57e62d5edcba45291ffe36e16c49bad472298d2b51c9a02'){return $false}
 $process=Get-Process -Id $Target.ProcessId -ErrorAction Stop
 $null=$process.Handle # retain this instance; do not confuse PID reuse with exit
 $until=[DateTime]::UtcNow.AddSeconds($script:s.timeoutSeconds)
 $sent=@{}
 while(-not $process.HasExited -and [DateTime]::UtcNow -lt $until){
  $fresh=Get-CimInstance Win32_Process -Filter ('ProcessId='+$Target.ProcessId)
  if(-not $fresh){break}
  if($fresh.CreationDate -ne $Target.CreationDate -or $fresh.SessionId -ne $Target.SessionId -or
     -not $fresh.ExecutablePath -or -not $fresh.ExecutablePath.Equals($image,[StringComparison]::OrdinalIgnoreCase)){
   throw '已安装的 FindX 进程已变化，拒绝关闭。本次未改动。'
  }
  $process.Refresh()
  $window=$process.MainWindowHandle.ToInt64()
  if($window -ne 0 -and -not $sent.ContainsKey($window)){
   $script:s.original.guiExitRequested=$true;Save-State
   if(-not $process.CloseMainWindow()){throw '已安装的 FindX 没有接受正常关闭。请手动关闭窗口后重试。本次未改动。'}
   $sent[$window]=$true
  }
  Start-Sleep -Milliseconds 100
 }
 if(-not $process.WaitForExit(100)){throw '已安装的 FindX 没有在限时内退出。本次未改动，不会强制结束。'}
 if($process.ExitCode -ne 0){throw ('已安装的 FindX 在关闭时异常退出，安装内容未改动。代码='+$process.ExitCode)}
 return $true
}
function Enable-FxTxGuiQuit {
 if('FxTxGuiQuit' -as [type]){return}
 Add-Type -ReferencedAssemblies Accessibility -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using Accessibility;
public static class FxTxGuiQuit {
  public delegate bool CB(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern bool EnumWindows(CB c, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out int p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder b, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder b, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern uint RegisterWindowMessage(string s);
  [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("oleacc.dll")] static extern int AccessibleObjectFromWindow(IntPtr h, uint id, ref Guid g, [MarshalAs(UnmanagedType.Interface)] out object o);
  static IntPtr Menu(int pid){ IntPtr found=IntPtr.Zero; EnumWindows((h,l)=>{ int id; GetWindowThreadProcessId(h,out id); if(id==pid){ var b=new StringBuilder(100); GetClassName(h,b,100); if(b.ToString()=="#32768") found=h; } return true; }, IntPtr.Zero); return found; }
  public static int CountTitle(int pid, string title){ int n=0; EnumWindows((h,l)=>{ int id; GetWindowThreadProcessId(h,out id); if(id==pid){ var b=new StringBuilder(260); GetWindowText(h,b,260); if(b.ToString()==title) n++; } return true; }, IntPtr.Zero); return n; }
  public static IntPtr FindTitle(int pid, string title){ IntPtr found=IntPtr.Zero; int n=0; EnumWindows((h,l)=>{ int id; GetWindowThreadProcessId(h,out id); if(id==pid){ var b=new StringBuilder(260); GetWindowText(h,b,260); if(b.ToString()==title){ found=h; n++; } } return true; }, IntPtr.Zero); return n==1?found:IntPtr.Zero; }
  public static bool PostRegistered(IntPtr owner, string name){ uint msg=RegisterWindowMessage(name); return owner!=IntPtr.Zero && msg!=0 && PostMessage(owner,msg,IntPtr.Zero,IntPtr.Zero); }
  public static void Cancel(IntPtr owner){ if(owner!=IntPtr.Zero) PostMessage(owner,0x1F,IntPtr.Zero,IntPtr.Zero); }
  public static int ActivateTrayQuit(int pid, IntPtr owner, string trayMessage, string caption, int waitMs){
    if(owner==IntPtr.Zero) return 3;
    uint msg=RegisterWindowMessage(trayMessage);
    if(msg==0 || !PostMessage(owner,msg,new IntPtr(1),new IntPtr(0x7B))) return 3;
    IntPtr menu=IntPtr.Zero; var start=DateTime.UtcNow;
    while((DateTime.UtcNow-start).TotalMilliseconds<waitMs){ menu=Menu(pid); if(menu!=IntPtr.Zero) break; System.Threading.Thread.Sleep(40); }
    if(menu==IntPtr.Zero){ PostMessage(owner,0x1F,IntPtr.Zero,IntPtr.Zero); return 1; }
    Guid g=new Guid("618736e0-3c3d-11cf-810c-00aa00389b71"); object o;
    if(AccessibleObjectFromWindow(menu,0xFFFFFFFC,ref g,out o)!=0 || o==null){ PostMessage(owner,0x1F,IntPtr.Zero,IntPtr.Zero); return 2; }
    var a=(IAccessible)o;
    string name=""; try{ name=(string)a.get_accName(2); }catch{ name=""; }
    if(a.accChildCount!=2 || name!=caption){ PostMessage(owner,0x1F,IntPtr.Zero,IntPtr.Zero); return 2; }
    a.accDoDefaultAction(2);
    return 0;
  }
}
public static class FxTxUserProcess {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct STARTUPINFO { public int cb; public string r,d,t; public int x,y,xs,ys,xc,yc,f,fl; public short sw,r2; public IntPtr res,si,so,se; }
  [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr hProcess,hThread; public int pid,tid; }
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint a, bool i, int pid);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr h);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr p, uint a, out IntPtr t);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool DuplicateTokenEx(IntPtr t, uint a, IntPtr s, int level, int type, out IntPtr d);
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool CreateProcessWithTokenW(IntPtr token, int logon, string app, StringBuilder cmd, int flags, IntPtr env, string dir, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
  public static int Start(int explorerPid, string exe, string args){
    IntPtr proc=OpenProcess(0x0400|0x1000,false,explorerPid);
    if(proc==IntPtr.Zero) throw new InvalidOperationException("无法打开当前用户桌面进程。请从托盘退出 FindX 后重试。本次未改动。");
    try{
      IntPtr token; if(!OpenProcessToken(proc,0x0002|0x0008,out token)) throw new InvalidOperationException("无法读取当前用户令牌。请从托盘退出 FindX 后重试。本次未改动。");
      try{
        IntPtr primary; uint access=0x0001|0x0002|0x0008|0x0080|0x0100;
        if(!DuplicateTokenEx(token,access,IntPtr.Zero,2,1,out primary)) throw new InvalidOperationException("无法复制当前用户令牌。请从托盘退出 FindX 后重试。本次未改动。");
        try{
          var si=new STARTUPINFO(); si.cb=Marshal.SizeOf(typeof(STARTUPINFO)); PROCESS_INFORMATION pi;
          var cmd=new StringBuilder("\""+exe+"\" "+args);
          if(!CreateProcessWithTokenW(primary,0,exe,cmd,0x08000000|0x00000400,IntPtr.Zero,null,ref si,out pi)) throw new InvalidOperationException("无法在当前用户会话中请求退出。请从托盘退出 FindX 后重试。本次未改动。");
          int pid=pi.pid; CloseHandle(pi.hProcess); CloseHandle(pi.hThread); return pid;
        }finally{ CloseHandle(primary); }
      }finally{ CloseHandle(token); }
    }finally{ CloseHandle(proc); }
  }
}
'@
}
function Test-FxTxElevated {
 $principal=New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
 return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}
function Test-GuiStillSame($Target){
 $fresh=Get-CimInstance Win32_Process -Filter ('ProcessId='+$Target.ProcessId) -ErrorAction SilentlyContinue
 if(-not $fresh){return $false}
 if($fresh.CreationDate -ne $Target.CreationDate -or $fresh.SessionId -ne $Target.SessionId){ throw 'FindX 进程在退出过程中被替换，已停止安装。本次未改动。' }
 $image=Join-Path $script:s.roots.app 'FindX.exe'
 if(-not $fresh.ExecutablePath -or -not $fresh.ExecutablePath.Equals($image,[StringComparison]::OrdinalIgnoreCase)){ throw 'FindX 进程路径已变化，已停止安装。本次未改动。' }
 return $true
}
function Wait-GuiExit($Target,[datetime]$Until){
 do { if(-not (Test-GuiStillSame $Target)){ return $true }; if([DateTime]::UtcNow -ge $Until){ return $false }; Start-Sleep -Milliseconds 80 } while($true)
}
function Invoke-GuiQuitAttempts {
 $deadline=[DateTime]::UtcNow.AddSeconds([Math]::Max(3,[int]$script:s.timeoutSeconds))
 $contract=Get-ShippedQuitContract
 Enable-FxTxGuiQuit
 foreach($target in @(Get-InstalledGui)){
  if(-not (Test-GuiStillSame $target)){ continue }
  if([FxTxGuiQuit]::CountTitle([int]$target.ProcessId,$contract.settingsTitle) -gt 0){ Set-LeaseReason 'gui-settings'; throw '请先关闭「FindX 设置」窗口后再安装。本次未改动。' }
  $image=Join-Path $script:s.roots.app 'FindX.exe'
  $version=(Get-Item -LiteralPath $image).VersionInfo.ProductVersion
  $hash=(Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash
  $owner=[FxTxGuiQuit]::FindTitle([int]$target.ProcessId,$contract.mainTitle)
  $done=$false
  foreach($step in @(Get-NormalQuitPlan $version $hash)){
   if([DateTime]::UtcNow -ge $deadline){ break }
   if($step -eq 'normal-quit-message'){
    if($owner -ne [IntPtr]::Zero){ [void][FxTxGuiQuit]::PostRegistered($owner,$contract.normalQuitMessage) }
    $slice=[DateTime]::UtcNow.AddSeconds(2); if($slice -gt $deadline){$slice=$deadline}
    if(Wait-GuiExit $target $slice){$done=$true;break}
   }elseif($step -eq 'shipped-tray-menu'){
    if($owner -eq [IntPtr]::Zero){ continue }
    $remain=[int][Math]::Max(1,($deadline-[DateTime]::UtcNow).TotalMilliseconds)
    $code=[FxTxGuiQuit]::ActivateTrayQuit([int]$target.ProcessId,$owner,$contract.trayMessage,$contract.trayCaption,[Math]::Min(3000,$remain))
    if($code -eq 0 -and (Wait-GuiExit $target $deadline)){$done=$true;break}
    [FxTxGuiQuit]::Cancel($owner)
   }elseif($step -eq 'legacy-252-close'){
    if(Request-Legacy252NormalClose $target){$done=$true;break}
   }
  }
  if(-not $done){ if(-not $script:reasonCode){ Set-LeaseReason 'gui-quit' }; throw '已安装的 FindX 没有完成正常退出。请从托盘选择「退出 FindX（不停止索引服务）」后重试。本次未改动。' }
 }
 return [pscustomobject]@{exited=$true;requested=$true;reason='';detail='exited'}
}
function New-FxTxMediumWorkspace {
 # ProgramData\FindX-upgrades is private to the elevated installer, and a High
 # integrity copy cannot be read by the medium user. The quit helper therefore
 # runs from a Medium copy under the same user's LocalAppData.
 $local=[Environment]::GetFolderPath('LocalApplicationData')
 if(-not $local){ Set-LeaseReason 'gui-quit'; throw '找不到当前用户的本地目录，不能代为退出 FindX。请从托盘退出后重试。本次未改动。' }
 $work=Join-Path (Join-Path $local 'Temp') ('findx-quit-' + [guid]::NewGuid().ToString('N'))
 $null=[IO.Directory]::CreateDirectory($work)
 foreach($name in $script:FxTxPackagedHelpers){
  $src=Join-Path $PSScriptRoot $name
  if(-not [IO.File]::Exists($src)){ Set-LeaseReason 'gui-quit'; throw ('退出助手缺少已打包文件: '+$name) }
  [IO.File]::Copy($src,(Join-Path $work $name),$false)
  if(-not [IO.File]::Exists((Join-Path $work $name))){ Set-LeaseReason 'gui-quit'; throw ('退出助手未能复制: '+$name) }
 }
 $icacls=Join-Path $env:SystemRoot 'System32\icacls.exe'
 & $icacls $work '/setintegritylevel' '(OI)(CI)M' '/T' | Out-Null
 if($LASTEXITCODE){ Set-LeaseReason 'gui-quit'; throw '无法把退出请求放到当前用户可读取的位置。请从托盘退出 FindX 后重试。本次未改动。' }
 return $work
}
function Start-FxTxOriginalUser([string]$Exe,[string]$Arguments){
 if(-not (Test-FxTxElevated)){ return Start-Process -FilePath $Exe -ArgumentList $Arguments -PassThru -WindowStyle Hidden }
 Enable-FxTxGuiQuit
 $session=(Get-Process -Id $PID).SessionId
 $me=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
 $explorer=@(Get-CimInstance Win32_Process -Filter "Name='explorer.exe' AND SessionId=$session" | Where-Object { (Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid).Sid -eq $me })
 if($explorer.Count -ne 1){ Set-LeaseReason 'gui-quit'; throw '找不到唯一的当前用户桌面进程，不能代为退出 FindX。请从托盘退出后重试。本次未改动。' }
 $childPid=[FxTxUserProcess]::Start([int]$explorer[0].ProcessId,$Exe,$Arguments)
 return Get-Process -Id $childPid
}
function Request-NormalGuiExit {
 if(-not (Get-InstalledGui).Count){return}
 if($InstallRoot -match '"' -or $TransactionRoot -match '"'){ Set-LeaseReason 'gui-quit'; throw '安装路径含有引号，已拒绝代为退出。请从托盘退出 FindX 后重试。本次未改动。' }
 if(-not (Test-FxTxElevated)){ Invoke-GuiQuitAttempts | Out-Null; $script:s.original.guiExitRequested=$true; $script:s.original.guiStopped=$true; Save-State; return }
 $work=$null
 try{
  $work=New-FxTxMediumWorkspace
  $reportPath=Join-Path $work 'gui-quit.json'
  $exe=Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
  $childArgs='-NoProfile -NonInteractive -WindowStyle Hidden -File "'+(Join-Path $work 'upgrade-transaction.ps1')+'" -Phase RequestQuit -QuitReporter -TransactionRoot "'+$TransactionRoot+'" -InstallRoot "'+$script:s.roots.app+'" -TimeoutSeconds '+[int]$script:s.timeoutSeconds+' -QuitReport "'+$reportPath+'"'
  $child=Start-FxTxOriginalUser $exe $childArgs
  $budget=[Math]::Max(5,[int]$script:s.timeoutSeconds)+8
  if(-not $child.WaitForExit($budget*1000)){
   try{ if(-not $child.HasExited){ $child.Kill() } }catch{}
   Set-LeaseReason 'gui-quit'
   throw '请求 FindX 正常退出超时。请从托盘选择「退出 FindX（不停止索引服务）」后重试。本次未改动。'
  }
  $exitCode=[int]$child.ExitCode
  try{ [IO.File]::WriteAllText((Join-Path $TransactionRoot 'gui-quit-helper.exit'),([string]$exitCode),[Text.UTF8Encoding]::new($false)) }catch{}
  foreach($name in @('gui-quit.json','failure.txt','failure.detail.txt','reason.code')){
   $src=Join-Path $work $name
   if([IO.File]::Exists($src)){ try{ [IO.File]::Copy($src,(Join-Path $TransactionRoot $name),$true) }catch{} }
  }
  $report=$null
  if([IO.File]::Exists($reportPath)){
   try{ $report=Get-Content -LiteralPath $reportPath -Raw -Encoding UTF8 | ConvertFrom-Json }catch{ $report=$null }
  }
  if($report -and $report.reason){ $script:reasonCode=[string]$report.reason }
  if($exitCode -eq 0 -and $report -and $report.exited){ $script:s.original.guiExitRequested=$true; $script:s.original.guiStopped=$true; Save-State; return }
  if(-not $script:reasonCode){ Set-LeaseReason 'gui-quit' }
  $detail=$null
  if($report -and $report.detail){ $detail=[string]$report.detail }
  elseif([IO.File]::Exists((Join-Path $work 'failure.txt'))){ $detail=[IO.File]::ReadAllText((Join-Path $work 'failure.txt')) }
  if(-not $detail){ $detail='已安装的 FindX 没有完成正常退出。请从托盘选择「退出 FindX（不停止索引服务）」后重试。本次未改动。' }
  if($exitCode -ne 0){ $detail=($detail+' (quit helper exit '+[string]$exitCode+')') }
  throw $detail
 }finally{ if($work){ Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue } }
}
function Verify-PayloadAndIndex {
 $path=Join-Path $script:s.roots.app 'payload-manifest.json'
 if(-not [IO.File]::Exists($path) -or (Get-Item -LiteralPath $path).Length -gt 16777216){throw 'Missing/bounded payload manifest'}
 $manifest=Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
 if($manifest.version -ne $script:s.expectedVersion){throw 'Payload manifest version mismatch'}
 foreach($f in $manifest.files){
  Assert-FxTxRelative $f.name
  $full=Join-Path $script:s.roots.app $f.name
  if(-not [IO.File]::Exists($full) -or (Get-Item -LiteralPath $full).Length -ne $f.length -or (Get-FxTxFileHash $full) -ne $f.sha256){throw ('Payload hash mismatch: '+$f.name)}
 }
 $indexes=@((Join-Path $script:s.roots.data 'index.bin'),(Join-Path $TransactionRoot 'backup\data\index.bin'))
 foreach($index in $indexes){
 if([IO.File]::Exists($index) -or [IO.Directory]::Exists($index+'.v6')){
  # This verifies already-stopped index readability/integrity, NEVER latest save ACK.
  $cli=Join-Path $script:s.roots.app 'findx2.exe'
  $info=[Diagnostics.ProcessStartInfo]::new($cli,('status -i "'+$index+'"'));$info.UseShellExecute=$false;$info.CreateNoWindow=$true
  $info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true;$info.StandardOutputEncoding=[Text.UTF8Encoding]::new($false);$info.StandardErrorEncoding=[Text.UTF8Encoding]::new($false)
  $child=[Diagnostics.Process]::Start($info)
  $out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync()
  if(-not $child.WaitForExit(30000)){throw 'Read-only index verification timeout; no force-stop'}
  [IO.File]::WriteAllText((Join-Path $TransactionRoot ('index-health-'+[array]::IndexOf($indexes,$index)+'.log')),($out.Result+"`n"+$err.Result))
  if($child.ExitCode -ne 0 -or ($out.Result+"`n"+$err.Result) -match '回退|failed integrity|走全量重建'){throw 'Stored index did not validate without fallback; original state/evidence retained'}
 }}
 $null=Read-FxTxVerifiedBackup (Join-Path $TransactionRoot 'backup')
}
function Verify-GuiHealth {
 $root=Join-Path $TransactionRoot 'gui-health'
 $null=[IO.Directory]::CreateDirectory($root)
 $exe=Join-Path $script:s.roots.app 'FindX.exe'
 $info=[Diagnostics.ProcessStartInfo]::new($exe,('--installer-health "'+$root+'"'));$info.UseShellExecute=$false
 $info.CreateNoWindow=$true;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
 $started=[DateTime]::UtcNow
 $child=[Diagnostics.Process]::Start($info)
 $out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync()
 if(-not $child.WaitForExit(30000)){throw 'Candidate GUI health timed out; no force exit; backup retained until normal exit'}
 [IO.File]::WriteAllText((Join-Path $root 'stdout.log'),$out.Result);[IO.File]::WriteAllText((Join-Path $root 'stderr.log'),$err.Result)
 if($child.ExitCode -ne 0){
  try{$events=Get-WinEvent -FilterHashtable @{LogName='Application';StartTime=$started.ToLocalTime()} -MaxEvents 50 -ErrorAction Stop | Where-Object {$_.Message -match 'FindX.exe'} | Select-Object TimeCreated,Id,ProviderName,Message;[IO.File]::WriteAllText((Join-Path $root 'windows-events.log'),($events | Format-List | Out-String))}catch{[IO.File]::WriteAllText((Join-Path $root 'windows-events.log'),('Own GUI event diagnostic unavailable: '+$_.Exception.Message))}
  throw ('Candidate GUI startup/normal exit failed: '+$child.ExitCode)
 }
 $render=Get-Content -LiteralPath (Join-Path $root 'render-health.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 $exit=Get-Content -LiteralPath (Join-Path $root 'normal-exit.json') -Raw -Encoding UTF8 | ConvertFrom-Json
 if($render.version -ne $script:s.expectedVersion -or $render.frames -lt 3 -or -not $render.storage_ok -or -not $render.normal_quit_requested -or -not $render.isolated -or -not $exit.normal_event_loop_return -or -not $exit.passed){throw 'GUI did not confirm real frame/persistence/normal-exit health'}
}
function Wait-NewServiceReady {
 $until=[DateTime]::UtcNow.AddSeconds($script:s.timeoutSeconds);$last='not ready'
 do {
  $svc=Get-ExactService
  if($svc -and $svc.State -eq 'Running' -and $svc.ProcessId){
   try {
    $status=Read-FxTxServiceStatus $script:s.pipeName ([int]$svc.ProcessId)
    [IO.File]::WriteAllText((Join-Path $TransactionRoot 'service-status-last.json'),(@{scmPid=$svc.ProcessId;scmState=$svc.State;status=$status} | ConvertTo-Json -Depth 8))
    if(-not $status.loading -and $status.healthy){
     [IO.File]::WriteAllText((Join-Path $TransactionRoot 'service-health.json'),($status | ConvertTo-Json -Depth 8));return
    }
    $last='Index loading or health failure: '+($status | ConvertTo-Json -Depth 8 -Compress)
   }catch{$last=$_.Exception.Message}
  }else{$last='Service is not running'}
  Start-Sleep -Milliseconds 100
 }while([DateTime]::UtcNow -lt $until)
 throw ('Real service IPC readiness failed: '+$last)
}
function Invoke-Rollback {
 if($script:s.state -eq 'committed' -or $script:s.state -eq 'rolled_back'){return}
 if($script:s.backupVerified){
  # An existing originally-running service may now be the candidate writer.
  # Obtain its own fresh receipt before restoring ANY old files; never overwrite
  # a running writer or reuse the old instance's pre-upgrade receipt.
  if($script:s.PSObject.Properties.Name -contains 'candidateServiceStarted' -and $script:s.candidateServiceStarted){
   $candidate=Get-ExactService
   if($candidate -and $candidate.State -ne 'Stopped'){
    $proof=Join-Path $TransactionRoot 'candidate-rollback-stop';$null=[IO.Directory]::CreateDirectory($proof)
    $scm=[int]$script:s.timeoutSeconds
    if(@($script:s.PSObject.Properties.Name) -contains 'serviceStopSeconds' -and [int]$script:s.serviceStopSeconds -gt 0){$scm=[int]$script:s.serviceStopSeconds}
    $null=Invoke-FxTxPreparedServiceStop -ServiceName $script:s.serviceName -PipeName $script:s.pipeName -Image (Join-Path $script:s.roots.app 'findx2-service.exe') -Index (Join-Path $script:s.roots.data 'index.bin') -Cli (Join-Path $script:s.roots.app 'findx2.exe') -ExpectedVersion $script:s.expectedVersion -Evidence $proof -TimeoutSeconds $script:s.timeoutSeconds -ScmTimeoutSeconds $scm -OnState {param($Handoff) $script:s.terminalHandoff=$Handoff;Save-State}
   }
  }
  # Each installer phase is a NEW PS process. Audit-aware verification must be
  # re-enabled during rollback/recovery, not just during initial backup.
  Enable-FxTxFullSecurity
  Set-State 'rollback_requested'
  if($script:s.createdService){Stop-ExactService}else{
   $original=Get-ExactService
   if($original -and $original.State -ne 'Stopped'){
    # journal 之外的运行态差异不再拒绝整个恢复：中断升级期间外部可能把服务重新拉起
    # （用户在两次尝试之间手动启动了旧服务，或上一事务收尾未完成）。恢复死事务不能因
    # 「世界已经往前走」而永远卡死——统一走 SCM 优雅停止（超时仍在 Wait-ServiceStopped
    # 抛错保底，不强杀）。恢复完成后是否重启由 original.serviceRunning 决定，
    # 与 journal 快照一致（原本停着的恢复后保持停止）。
    Stop-ExactService
    [IO.File]::WriteAllText((Join-Path $TransactionRoot 'rollback-external-service-stop.txt'),('state='+$original.State),[Text.UTF8Encoding]::new($false))
   }
  }
  if((Get-InstalledGui).Count){throw 'Candidate GUI still active; retained backup, refusing unsafe restore'}
  if($script:s.createdService){
   $v=Get-ExactService
   if($v){& "$env:SystemRoot\System32\sc.exe" delete $script:s.serviceName | Out-Null;if($LASTEXITCODE){throw 'Cannot remove this transaction service'}}
   $until=[DateTime]::UtcNow.AddSeconds($script:s.timeoutSeconds)
   while((Get-CimInstance Win32_Service -Filter ("Name='"+$script:s.serviceName+"'")) -and [DateTime]::UtcNow -lt $until){Start-Sleep -Milliseconds 100}
   if(Get-CimInstance Win32_Service -Filter ("Name='"+$script:s.serviceName+"'")){throw 'Transaction service deletion not confirmed'}
  }
  Set-State 'restoring'
  $null=Restore-FxTxVerifiedBackup (Join-Path $TransactionRoot 'backup') (Join-Path $TransactionRoot 'failed-new')
  Set-State 'restoration_verified'
 }
 if($script:s.platform){Restore-FxTxPlatform $script:s.platform (Join-Path $TransactionRoot 'failed-shortcuts')}
 Restore-OriginalRuntime; Set-State 'rolled_back'
}
if($Phase -eq 'RequestQuit'){
 $root=Get-FxTxPath $InstallRoot
 $script:s=[pscustomobject]@{timeoutSeconds=[Math]::Max(3,$TimeoutSeconds);roots=[pscustomobject]@{app=$root};original=[pscustomobject]@{guiExitRequested=$false;guiStopped=$false;guiRunning=$true}}
 $payload=[ordered]@{exited=$false;requested=$false;reason='';detail=''}
 $code=2
 try { $payload=Invoke-GuiQuitAttempts; $code=0 } catch { $payload.detail=$_.Exception.Message; $payload.reason=[string]$script:reasonCode; $payload.requested=$true }
 $dest=$QuitReport
 if(-not $dest){ $dir=Get-FxTxPath $TransactionRoot; if([IO.Directory]::Exists($dir)){ $dest=Join-Path $dir 'gui-quit.json' } }
 if($dest){ [IO.File]::WriteAllText($dest,($payload|ConvertTo-Json -Compress),[Text.UTF8Encoding]::new($false)) }
 exit $code
}
try{
 $TransactionRoot=Get-FxTxPath $TransactionRoot
 $script:leaseStore=if($TestBuild){Join-Path ([IO.Path]::GetDirectoryName($TransactionRoot)) '.findx-upgrade-store'}else{[IO.Path]::GetDirectoryName($TransactionRoot)}
 if($Phase -eq 'Prepare'){
  $roots=[ordered]@{app=(Get-FxTxPath $InstallRoot);data=(Get-FxTxPath $DataRoot);nativeConfig=(Get-FxTxPath $NativeConfigRoot);legacyConfig=(Get-FxTxPath $LegacyConfigRoot)}
  if($ServiceName -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,100}$' -or $PipeName -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,100}$'){throw 'Invalid exact endpoint name'}
  if($TestBuild){
   if($env:GITHUB_ACTIONS -ne 'true' -or $ServiceName -notmatch '^FindXUpgradeTest_[a-f0-9]{32}$' -or $PipeName -notmatch '^findx-upgrade-test-[a-f0-9]{32}$'){throw 'Test package is restricted to isolated CI namespace'}
   $allowed=Get-FxTxPath $env:RUNNER_TEMP
   foreach($path in @($roots.Values)+@($TransactionRoot)){if(-not $path.StartsWith($allowed+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Test path outside runner temp'}}
  }elseif($ServiceName -ne 'FindX2Search' -or $PipeName -ne 'findx2' -or $Fault -ne 'none'){throw 'Production endpoints/fault policy cannot be overridden'}
  Set-FxTxPrivateDirectory $script:leaseStore
  if(Test-Path -LiteralPath (Join-Path $TransactionRoot 'journal.json')){throw 'Unfinished/existing transaction requires recovery, not overwrite'}
  if(Test-Path -LiteralPath $TransactionRoot){throw 'Transaction root must be new; prior evidence retained'}
  Set-FxTxPrivateDirectory $TransactionRoot
  $effectiveScm=if($ServiceStopSeconds -gt 0){$ServiceStopSeconds}else{$TimeoutSeconds}
  $script:s=[pscustomobject]@{schema=2;installerOwner=$null;state='created';roots=[pscustomobject]$roots;serviceName=$ServiceName;pipeName=$PipeName;expectedVersion=$ExpectedVersion;installService=($InstallService -eq 1);timeoutSeconds=$TimeoutSeconds;serviceStopSeconds=$effectiveScm;testVolume='C:';ownedTestVolume=$false;testBuild=[bool]$TestBuild;fault=$Fault;platform=$null;backupVerified=$false;createdService=$false;candidateServiceStarted=$false;terminalHandoff=$null;original=[pscustomobject]@{serviceExists=$false;serviceRunning=$false;guiRunning=$false;guiStopped=$false;guiExitRequested=$false;serviceCommand='';serviceStartMode=''};error=''}
  $script:lock=[IO.File]::Open((Join-Path $TransactionRoot 'phase.lock'),'OpenOrCreate','ReadWrite','None')
  if($TestVolumeImage){
   if(-not $TestBuild){throw 'Test volume is forbidden in production'}
   $image=Get-FxTxPath $TestVolumeImage
   if($image -ine (Join-Path ([IO.Path]::GetDirectoryName($TransactionRoot)) 'owned-health.vhd')){throw 'Test image must be the exact owned case image'}
   $diskImage=Get-DiskImage -ImagePath $image -ErrorAction Stop
   $disk=$diskImage | Get-Disk -ErrorAction Stop
   $rawDisks=@(Get-CimInstance -Namespace 'root/Microsoft/Windows/Storage' -ClassName MSFT_Disk -Filter ("Number="+[uint32]$disk.Number) -ErrorAction Stop)
   if($rawDisks.Count -ne 1){throw 'Exact image disk did not resolve to one raw CIM disk'}
   $rawDisk=$rawDisks[0];$bus=$rawDisk.CimInstanceProperties['BusType']
   if($bus.CimType.ToString() -ne 'UInt16' -or $bus.Value -isnot [uint16] -or $bus.Value -ne 15 -or $rawDisk.CimInstanceProperties['UniqueId'].Value -cne $disk.UniqueId -or $rawDisk.CimInstanceProperties['Number'].Value -ne $disk.Number){throw 'Raw CIM disk identity/type mismatch; no host disk fallback'}
   if(-not $diskImage.Attached -or $disk.IsBoot -or $disk.IsSystem -or $disk.Size -ne 67108864 -or $bus.Value -ne 15){throw 'Only the owned 64MiB virtual disk is permitted'}
   $parts=@($disk | Get-Partition | Where-Object {$_.DriveLetter})
   if($parts.Count -ne 1){throw 'Owned test volume must have one letter'}
   $vol=$parts[0] | Get-Volume
   if($vol.FileSystem -ne 'NTFS' -or $vol.FileSystemLabel -notmatch '^FXTX[0-9A-F]{8}$'){throw 'Owned NTFS label mismatch'}
   $script:s.testVolume=([string]$parts[0].DriveLetter)+':';$script:s.ownedTestVolume=$true
  }
  Save-State;Enter-InstallerLease
  $svc=Get-ExactService
  if($svc){$script:s.original.serviceExists=$true;$script:s.original.serviceRunning=($svc.State -eq 'Running');$script:s.original.serviceCommand=$svc.PathName;$script:s.original.serviceStartMode=$svc.StartMode;if($svc.State -notin @('Running','Stopped')){throw 'Service in transition; unchanged'}}
  $script:s.original.guiRunning=((Get-InstalledGui).Count -gt 0); Save-State
  if(-not $script:s.testBuild){
   $me=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;$session=(Get-Process -Id $PID).SessionId
   $owners=@(Get-CimInstance Win32_Process -Filter "Name='explorer.exe' AND SessionId=$session" | ForEach-Object {(Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid).Sid} | Sort-Object -Unique)
   if($owners.Count -ne 1 -or $owners[0] -ne $me){throw 'Original user profile cannot be verified in this session; no profile or running application changed'}
  }
  foreach($p in $roots.Values){if([IO.Path]::GetPathRoot($p) -ine [IO.Path]::GetPathRoot($TransactionRoot)){throw 'Cross-volume atomic recovery is unsupported; refused before replacement'}}
  # Quit the installed GUI before touching the service. 2.5.5 uses the shipped
  # tray caption; quitting does not itself stop the index service.
  Request-NormalGuiExit
  if($script:s.original.serviceRunning){
   # --version only proves this image is findx2-service. It is not a protocol2
   # receipt and is not compared to the installer version. Authenticated
   # capabilities choose the strict receipt. Source errors or metadata that is
   # not ready use the accepted SCM save and stay marked as not protocol2.
   if($script:s.testBuild -and $script:s.fault -eq 'stop-timeout'){Stop-ExactService;throw 'Expected isolated stop-timeout fixture did not time out'}
   $null=Invoke-FxTxPreparedServiceStop -ServiceName $ServiceName -PipeName $PipeName -Image (Join-Path $roots.app 'findx2-service.exe') -Index (Join-Path $roots.data 'index.bin') -Cli (Join-Path $roots.app 'findx2.exe') -ExpectedVersion $ExpectedVersion -Evidence $TransactionRoot -TimeoutSeconds $TimeoutSeconds -ScmTimeoutSeconds $effectiveScm -OnState {param($Handoff) $script:s.terminalHandoff=$Handoff;Save-State}
  }
  Enable-FxTxFullSecurity
  Set-State 'quiesced'
  $links=@();if($CreateGroup -eq 1){$links+=@(Join-Path $GroupRoot 'FindX.lnk');$links+=@(Join-Path $GroupRoot '卸载 FindX.lnk')};if($DesktopRoot){$links+=@(Join-Path $DesktopRoot 'FindX.lnk')}
  if($script:s.testBuild){foreach($link in $links){if(-not (Get-FxTxPath $link).StartsWith($allowed+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Test shortcut outside runner temp'}}}
  $script:s.platform=Get-FxTxPlatformSnapshot $UninstallKey $ServiceName $links $roots.app ($AddPath -eq 1);Save-State
  if($script:s.fault -eq 'backup'){throw 'Injected CI backup failure before any replacement'}
  $map=Get-FxTxUpgradeBackupRoots $script:s.roots
  # Reserve a second old-set copy for rollback, plus bounded candidate staging.
  $estimate=Get-FxTxInventory $map
  $null=New-FxTxVerifiedBackup $map (Join-Path $TransactionRoot 'backup') -CandidateBytes ([decimal]$estimate.bytes + 268435456) -AdministratorsOnly
  $script:s.backupVerified=$true;Set-State 'backup_verified'
 }else{
  $script:lock=[IO.File]::Open((Join-Path $TransactionRoot 'phase.lock'),'Open','ReadWrite','None')
  $script:s=Get-Content -LiteralPath (Join-Path $TransactionRoot 'journal.json') -Raw -Encoding UTF8 | ConvertFrom-Json
  $script:leaseStore=if($script:s.testBuild){Join-Path ([IO.Path]::GetDirectoryName($TransactionRoot)) '.findx-upgrade-store'}else{[IO.Path]::GetDirectoryName($TransactionRoot)}
  Assert-FxTxPrivateDirectory $TransactionRoot;Assert-InstallerLease
  if($script:s.schema -ne 2){throw 'Unsupported transaction schema'}
  if($Phase -eq 'Verify'){
   if($script:s.state -ne 'backup_verified'){throw 'Invalid verification transition'}
   Set-State 'installed'
   $gui=Join-Path $script:s.roots.app 'FindX.exe'
   if(-not [IO.File]::Exists($gui) -or (Get-Item -LiteralPath $gui).VersionInfo.ProductVersion -ne $script:s.expectedVersion){throw 'Installed GUI version mismatch'}
   # Never start a candidate writer before stored data and backup validate.
   Verify-PayloadAndIndex
   if($script:s.installService -and -not $script:s.original.serviceExists){
    # Registration is exact-name and happens ONLY after verified backup/file copy.
    $exe=Join-Path $script:s.roots.app 'findx2-service.exe'
    $cmd='"'+$exe+'" --service --index "'+(Join-Path $script:s.roots.data 'index.bin')+'" --volume '+$script:s.testVolume+' --pipe '+$script:s.pipeName
    if($script:s.testBuild){$cmd+=' --no-backfill --no-everything-ipc';if(-not $script:s.ownedTestVolume){$cmd+=' --no-watch'}}
    # Persist ownership only AFTER successful creation; never delete a concurrently
    # created service merely because our New-Service call failed.
    $index=Join-Path $script:s.roots.data 'index.bin'
    $hasIndex=[IO.File]::Exists($index) -or [IO.Directory]::Exists($index+'.v6')
    $startMode=if($hasIndex){'Automatic'}else{'Manual'}
    $null=New-Service -Name $script:s.serviceName -BinaryPathName $cmd -StartupType $startMode
    $script:s.createdService=$true;Save-State
    $index=Join-Path $script:s.roots.data 'index.bin'
    if($hasIndex){
     Start-Service -Name $script:s.serviceName
     Wait-NewServiceReady
    }else{
     # Unchanged252engine cannot start without an index. Do not fabricate data or
     # initiate a full-volume scan. Explicitly registered but NOT initialized/running.
     [IO.File]::WriteAllText((Join-Path $TransactionRoot 'service-uninitialized.flag'),'RegisteredStopped: no index; initialize explicitly in FindX settings')
    }
   }
   if($script:s.original.serviceExists){
    $existing=Get-ExactService
    $state=if($existing){[string]$existing.State}else{'Missing'}
    $decision=Resolve-UpgradeServiceStart $true ([bool]$script:s.original.serviceRunning) ([string]$script:s.original.serviceStartMode) $state
    if($decision -eq 'start-prior-running'){
     $script:s.candidateServiceStarted=$true;Save-State
     Start-Service -Name $script:s.serviceName
     Wait-NewServiceReady
    }elseif($decision -eq 'start-automatic'){
     # Index bytes were already validated offline. Reach SCM Running, then commit.
     # A still-loading index is not a failed upgrade. Rollback stops this candidate
     # and leaves the exact prior stopped state.
     $script:s.candidateServiceStarted=$true;Save-State
     Start-Service -Name $script:s.serviceName
     $controller=[ServiceProcess.ServiceController]::new($script:s.serviceName)
     try{$controller.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Running,[TimeSpan]::FromSeconds($script:s.timeoutSeconds))}finally{$controller.Dispose()}
     $started=Get-ExactService
     if(-not $started -or $started.State -ne 'Running'){throw 'Automatic service did not reach Running; previous stopped state will be restored'}
    }
   }
   Verify-GuiHealth
   if($script:s.fault -eq 'health'){if($script:s.testBuild){[IO.File]::WriteAllText((Join-Path $script:s.roots.data 'index.bin'),'deliberately damaged CI fixture only')};throw 'Injected CI post-copy health failure'}
   Apply-FxTxPlatformPath $script:s.platform
   if($script:s.fault -eq 'post-platform'){
    if($script:s.testBuild){
     $observed=@();foreach($link in $script:s.platform.shortcuts){$exists=[IO.File]::Exists($link.path);$observed+=@{path=$link.path;exists=$exists;bytes=$(if($exists){[Convert]::ToBase64String([IO.File]::ReadAllBytes($link.path))}else{''})}}
     $envkey=[Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Control\Session Manager\Environment')
     try{$appliedPath=$envkey.GetValue('Path',$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)}finally{$envkey.Dispose()}
     [IO.File]::WriteAllText((Join-Path $TransactionRoot 'platform-applied.json'),(@{path=$appliedPath;shortcuts=$observed} | ConvertTo-Json -Depth 8))
    }
    throw 'Injected CI failure after actual PATH and shortcut changes'
   }
   Set-State 'health_verified'
  }elseif($Phase -eq 'Commit'){
   if($script:s.state -ne 'health_verified'){throw 'Cannot commit without verified health'}
   Set-State 'committed';if($script:s.original.guiRunning -and $script:s.original.guiExitRequested -and (Get-InstalledGui).Count -eq 0){[IO.File]::WriteAllText((Join-Path $TransactionRoot 'restart-original-user-gui.flag'),(Join-Path $script:s.roots.app 'FindX.exe'))}
  }else{Invoke-Rollback}
 }
 exit 0
}catch{
 $raw=$_.Exception.ToString()
 $message=[string]$_.Exception.Message
 if($raw -match 'Legacy running service'){ $script:reasonCode='legacy-save-unconfirmed' }
 if($message -match 'being used by another process|正由另一进程使用|另一个进程'){ if(-not $script:reasonCode){ $script:reasonCode='upgrade-lock-held' }; $message=(Get-LeaseRefusalMessage 'refuse-lock-held') }
 if($message -match 'MethodInvocationException|RuntimeException|^\s*at ' -or $message.Length -gt 240){ $message='升级准备失败。安装内容未改动。请查看日志后重试。' }
 if($script:s){
  $script:s.error=$message
  try{Save-State;if($Phase -eq 'Prepare'){Invoke-Rollback}}catch{$message+=' 回滚也未完成，现场已保留。' }
 }
 if([IO.Directory]::Exists($TransactionRoot)){
  $utf8=[Text.UTF8Encoding]::new($false)
  [IO.File]::WriteAllText((Join-Path $TransactionRoot 'failure.txt'),$message,$utf8)
  [IO.File]::WriteAllText((Join-Path $TransactionRoot 'failure.detail.txt'),$raw,$utf8)
  if($script:reasonCode){ [IO.File]::WriteAllText((Join-Path $TransactionRoot 'reason.code'),$script:reasonCode,$utf8) }
 }
 Write-Error $message -ErrorAction Continue
 exit 1
}finally{if($script:lock){$script:lock.Dispose()}}
