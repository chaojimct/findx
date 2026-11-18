# Exact installer-owned registry/shortcut scope. No arbitrary tree or credential export.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if(-not ('FxTxProcessNative' -as [type])) {Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FxTxProcessNative {
 [DllImport("shell32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr CommandLineToArgvW(string command, out int count);
 [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
 [DllImport("kernel32.dll",SetLastError=true)] public static extern bool GetNamedPipeServerProcessId(IntPtr h,out uint pid);
 public static string[] Args(string command) {int n;IntPtr p=CommandLineToArgvW(command,out n);if(p==IntPtr.Zero)throw new System.ComponentModel.Win32Exception();try{if(n>64)throw new InvalidOperationException("Service argument bound");var a=new string[n];for(int i=0;i<n;i++)a[i]=Marshal.PtrToStringUni(Marshal.ReadIntPtr(p,i*IntPtr.Size));return a;}finally{LocalFree(p);}}
}
'@}
function Assert-FxTxServiceScope([string]$Command,[string]$Image,[string]$Index,[string]$PipeName) {
 $args=[FxTxProcessNative]::Args($Command)
 if($args.Count -lt 2 -or $args[0] -ine $Image){throw 'Unexpected service executable; unchanged'}
 $seen=@{}
 for($i=1;$i -lt $args.Count;$i++){
  $key=$args[$i]
  if($seen.ContainsKey($key)){throw 'Duplicate service argument; unchanged'}
  if($key -in @('--service','--no-watch','--no-backfill','--no-everything-ipc')){$seen[$key]=$true;continue}
  if($key -notin @('--index','--pipe','--volume','--save-interval-secs') -or $i+1 -ge $args.Count){throw 'Unsupported service arguments; unchanged'}
  $i++;$seen[$key]=$args[$i]
 }
 if(-not $seen.ContainsKey('--service') -or -not $seen.ContainsKey('--index') -or -not $seen.ContainsKey('--pipe')){throw 'Service index/pipe is not explicit; unchanged'}
 if((Get-FxTxPath $seen['--index']) -ine (Get-FxTxPath $Index) -or $seen['--pipe'] -cne $PipeName){throw 'Service index/pipe lies outside the verified backup scope; unchanged'}
 if($seen.ContainsKey('--volume') -and $seen['--volume'] -notmatch '^[A-Za-z]:$'){throw 'Unsupported volume scope; unchanged'}
 if($seen.ContainsKey('--save-interval-secs') -and $seen['--save-interval-secs'] -notmatch '^[0-9]{1,7}$'){throw 'Unsupported save interval; unchanged'}
}
function Read-FxTxServiceStatus([string]$PipeName,[int]$ExpectedPid,[int]$TimeoutMs=1000) {
 $pipe=[IO.Pipes.NamedPipeClientStream]::new('.',$PipeName,[IO.Pipes.PipeDirection]::InOut,[IO.Pipes.PipeOptions]::Asynchronous)
 $reader=$null
 try {
  $pipe.Connect($TimeoutMs);[uint32]$server=0
  if(-not [FxTxProcessNative]::GetNamedPipeServerProcessId($pipe.SafePipeHandle.DangerousGetHandle(),[ref]$server) -or $server -ne $ExpectedPid){throw 'Health pipe is not owned by the exact service process'}
  $bytes=[Text.Encoding]::UTF8.GetBytes('{"type":"status"}'+"`n");$pipe.Write($bytes,0,$bytes.Length);$pipe.Flush()
  $reader=[IO.StreamReader]::new($pipe,[Text.Encoding]::UTF8,$false,4096,$true)
  $task=$reader.ReadLineAsync();if(-not $task.Wait($TimeoutMs)){throw 'Status response timeout'}
  $line=$task.Result;if(-not $line -or $line.Length -gt 65536){throw 'Invalid bounded status response'}
  $status=$line | ConvertFrom-Json
  if($status.type -ne 'status_result'){throw 'Unexpected IPC status response'}
  return $status
 }finally{if($reader){$reader.Dispose()};$pipe.Dispose()}
}
function Set-FxTxPrivateDirectory([string]$Path) {
 $path=Get-FxTxPath $Path
 if(Test-Path -LiteralPath $path){Assert-FxTxPrivateDirectory $path;return}
 $acl=[Security.AccessControl.DirectorySecurity]::new();$acl.SetAccessRuleProtection($true,$false)
 $admins=[Security.Principal.SecurityIdentifier]::new('S-1-5-32-544');$acl.SetOwner($admins)
 foreach($sid in @($admins,[Security.Principal.SecurityIdentifier]::new('S-1-5-18'))){$acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'))}
 $null=[IO.Directory]::CreateDirectory($path,$acl)
 Assert-FxTxPrivateDirectory $path
}
function Assert-FxTxPrivateDirectory([string]$Path) {
 $acl=Get-Acl -LiteralPath (Get-FxTxPath $Path)
 if($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -notin @('S-1-5-18','S-1-5-32-544') -or -not $acl.AreAccessRulesProtected){throw 'Upgrade store is not protected and administrator-owned'}
 foreach($a in $acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])){if($a.IdentityReference.Value -notin @('S-1-5-18','S-1-5-32-544') -or $a.AccessControlType -ne 'Allow'){throw 'Unexpected upgrade-store access rule; unchanged'}}
}
function Open-FxTxRegistry($Spec,[bool]$Write=$false) {
 if($Spec.hive -notin @('LocalMachine','CurrentUser') -or $Spec.path -match '(^|\\)\.\.(\\|$)' -or $Spec.path.StartsWith('\')){throw 'Invalid exact registry scope'}
 $base=[Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]$Spec.hive,[Microsoft.Win32.RegistryView]::Registry64)
 try{return $base.OpenSubKey($Spec.path,$Write)}finally{$base.Dispose()}
}
function Read-FxTxRegistryValue($Key,[string]$Name) {
 $exists=$null -ne $Key -and $Name -in $Key.GetValueNames()
 if(-not $exists){return [ordered]@{name=$Name;exists=$false;kind='';value=$null}}
 $kind=$Key.GetValueKind($Name).ToString();$value=$Key.GetValue($Name,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
 if($kind -in @('Binary','None')){$value=[Convert]::ToBase64String([byte[]]$value)}
 return [ordered]@{name=$Name;exists=$true;kind=$kind;value=$value}
}
function Read-FxTxRegistryTree($Spec) {
 $key=Open-FxTxRegistry $Spec
 if(-not $key){return [ordered]@{exists=$false;values=@();children=@();sddl=''}}
 try {
  $values=@();foreach($n in @($key.GetValueNames() | Sort-Object)){$values+=Read-FxTxRegistryValue $key $n}
  $children=@();foreach($n in @($key.GetSubKeyNames() | Sort-Object)){if($n.Contains('\')){throw 'Invalid registry child'};$children += [ordered]@{name=$n;tree=(Read-FxTxRegistryTree ([pscustomobject]@{hive=$Spec.hive;path=$Spec.path+'\'+$n}))}}
  $r=[ordered]@{exists=$true;values=@($values);children=@($children);sddl=$key.GetAccessControl().GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::All)}
  if(($r | ConvertTo-Json -Depth 30 -Compress).Length -gt 4194304){throw 'Exact registry snapshot exceeds bound'}
  return $r
 }finally{$key.Dispose()}
}
function Set-FxTxRegistryValue($Key,$Record) {
 if(-not $Record.exists){$Key.DeleteValue($Record.name,$false);return}
 $v=$Record.value
 switch($Record.kind){'Binary'{$v=[Convert]::FromBase64String([string]$v)} 'None'{$v=[Convert]::FromBase64String([string]$v)} 'DWord'{$v=[int]$v} 'QWord'{$v=[long]$v} 'MultiString'{$v=[string[]]@($v)} default{$v=[string]$v}}
 $Key.SetValue($Record.name,$v,[Microsoft.Win32.RegistryValueKind]$Record.kind);$Key.Flush()
}
function Restore-FxTxRegistryTree($Spec,$Tree) {
 $base=[Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]$Spec.hive,[Microsoft.Win32.RegistryView]::Registry64)
 try {
  if(-not $Tree.exists){$base.DeleteSubKeyTree($Spec.path,$false);return}
  $key=$base.OpenSubKey($Spec.path,$true)
  if(-not $key){throw 'Original registration key disappeared; refusing to fabricate its unknown audit security'}
  try{
   # Inno never changes an existing registration ACL. Do not convert a legacy
   # ACL inheritance model merely to restore values; preserve its audit ACL too.
   if($key.GetAccessControl().GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::All) -cne $Tree.sddl){throw 'Existing registry security changed externally; no unsafe ACL rewrite'}
   foreach($name in @($key.GetValueNames())){if($name -notin @($Tree.values | ForEach-Object {$_.name})){$key.DeleteValue($name,$false)}}
   foreach($v in $Tree.values){Set-FxTxRegistryValue $key $v}
   foreach($name in @($key.GetSubKeyNames())){if($name -notin @($Tree.children | ForEach-Object {$_.name})){$key.DeleteSubKeyTree($name,$false)}}
   foreach($child in $Tree.children){Restore-FxTxRegistryTree ([pscustomobject]@{hive=$Spec.hive;path=$Spec.path+'\'+$child.name}) $child.tree}
   $key.Flush()
  }finally{$key.Dispose()}
 }finally{$base.Dispose()}
}
function Assert-FxTxSame($Actual,$Expected,[string]$Context) {
 if(($Actual | ConvertTo-Json -Depth 40 -Compress) -cne ($Expected | ConvertTo-Json -Depth 40 -Compress)){throw ($Context+' changed or did not restore exactly')}
}
function Get-FxTxAppendedPath([string]$Current,[string]$InstallRoot,[bool]$AddPath) {
 if(-not $AddPath -or @($Current.Split(';') | Where-Object {$_.Trim().Trim('"').TrimEnd('\').Equals($InstallRoot.TrimEnd('\'),[StringComparison]::OrdinalIgnoreCase)}).Count){return $Current}
 if($Current.Length){return $Current+';'+$InstallRoot};return $InstallRoot
}
function Get-FxTxPlatformSnapshot([string]$UninstallKey,[string]$ServiceName,[string[]]$ShortcutPaths,[string]$InstallRoot,[bool]$AddPath) {
 if($UninstallKey -notmatch '^(\{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C\}|FindXUpgradeTest-FindXUpgradeTest_[a-f0-9]{32})_is1$'){throw 'Unrecognized exact uninstall registration'}
 $uninstall=[pscustomobject]@{hive='LocalMachine';path='SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\'+$UninstallKey}
 $service=[pscustomobject]@{hive='LocalMachine';path='SYSTEM\CurrentControlSet\Services\'+$ServiceName}
 $envspec=[pscustomobject]@{hive='LocalMachine';path='SYSTEM\CurrentControlSet\Control\Session Manager\Environment'}
 $runspec=[pscustomobject]@{hive='CurrentUser';path='Software\Microsoft\Windows\CurrentVersion\Run'}
 $key=Open-FxTxRegistry $envspec;try{$path=Read-FxTxRegistryValue $key 'Path'}finally{if($key){$key.Dispose()}}
 $key=Open-FxTxRegistry $runspec;try{$run=Read-FxTxRegistryValue $key 'FindX'}finally{if($key){$key.Dispose()}}
 $shortcuts=@();foreach($p in @($ShortcutPaths | Where-Object {$_} | Sort-Object -Unique)){
  $p=Get-FxTxPath $p;if(-not $p.EndsWith('.lnk',[StringComparison]::OrdinalIgnoreCase)){throw 'Only exact product shortcuts permitted'}
  $present=[IO.File]::Exists($p);$entry=[ordered]@{path=$p;exists=$present;bytes='';metadata=$null}
  if($present){if((Get-Item -LiteralPath $p).Length -gt 2097152){throw 'Shortcut exceeds bound'};$entry.metadata=Get-FxTxMetadata $p;$entry.bytes=[Convert]::ToBase64String([IO.File]::ReadAllBytes($p))};$shortcuts+=$entry
 }
 if($path.exists -and $path.kind -notin @('String','ExpandString')){throw 'Unexpected PATH type; unchanged'}
 $newPath=Get-FxTxAppendedPath ([string]$path.value) $InstallRoot $AddPath
 return [ordered]@{schema=1;uninstallSpec=$uninstall;uninstall=(Read-FxTxRegistryTree $uninstall);serviceSpec=$service;service=(Read-FxTxRegistryTree $service);environmentSpec=$envspec;path=$path;addPath=$AddPath;newPath=$newPath;runSpec=$runspec;run=$run;shortcuts=@($shortcuts)}
}
function Apply-FxTxPlatformPath($Snapshot) {
 if(-not $Snapshot.addPath){return}
 $key=Open-FxTxRegistry $Snapshot.environmentSpec $true
 try{
  Assert-FxTxSame (Read-FxTxRegistryValue $key 'Path') $Snapshot.path 'Concurrent PATH edit'
  $kind=if($Snapshot.path.exists){$Snapshot.path.kind}else{'ExpandString'}
  Set-FxTxRegistryValue $key ([pscustomobject]@{exists=$true;name='Path';value=$Snapshot.newPath;kind=$kind})
 }finally{$key.Dispose()}
}
function Restore-FxTxPlatform($Snapshot,[string]$EvidenceRoot) {
 # Existing service configuration was NEVER rewritten. Only a transaction-created
 # service is normally stopped/deleted by the executor before this exact assertion.
 Assert-FxTxSame (Read-FxTxRegistryTree $Snapshot.serviceSpec) $Snapshot.service 'Original SCM registration/security'
 if(((Read-FxTxRegistryTree $Snapshot.uninstallSpec) | ConvertTo-Json -Depth 40 -Compress) -cne ($Snapshot.uninstall | ConvertTo-Json -Depth 40 -Compress)){Restore-FxTxRegistryTree $Snapshot.uninstallSpec $Snapshot.uninstall}
 Assert-FxTxSame (Read-FxTxRegistryTree $Snapshot.uninstallSpec) $Snapshot.uninstall 'Uninstall registration/security'
 $key=Open-FxTxRegistry $Snapshot.environmentSpec $true
 try{
  $now=Read-FxTxRegistryValue $key 'Path'
  if(($now | ConvertTo-Json -Compress) -cne ($Snapshot.path | ConvertTo-Json -Compress)){
   $kind=if($Snapshot.path.exists){$Snapshot.path.kind}else{'ExpandString'}
   $expected=[pscustomobject]@{name='Path';exists=$true;kind=$kind;value=$Snapshot.newPath}
   if(-not $Snapshot.addPath){throw 'External PATH edit; not overwritten'}
   Assert-FxTxSame $now $expected 'Concurrent PATH edit, refusing to overwrite'
   Set-FxTxRegistryValue $key $Snapshot.path
  }
 }finally{$key.Dispose()}
 $key=Open-FxTxRegistry $Snapshot.runSpec
 try{Assert-FxTxSame (Read-FxTxRegistryValue $key 'FindX') $Snapshot.run 'User autostart (installer does not mutate it)'}finally{if($key){$key.Dispose()}}
 $n=0;foreach($item in $Snapshot.shortcuts){
  $identity=$n;$n++ # Stable leaf identity, even when an earlier leaf is already restored.
  $null=Get-FxTxPath $item.path
  $same=([IO.File]::Exists($item.path) -eq [bool]$item.exists)
  if($same -and $item.exists){$same=([Convert]::ToBase64String([IO.File]::ReadAllBytes($item.path)) -ceq $item.bytes)}
  if(-not $same){
   if([IO.File]::Exists($item.path)){
    $null=[IO.Directory]::CreateDirectory($EvidenceRoot)
    if([IO.Directory]::GetFiles($EvidenceRoot).Count -ge 128){throw 'Shortcut recovery evidence bound reached; originals retained'}
    $dest=Join-Path $EvidenceRoot (([string]$identity)+'.lnk')
    if([IO.File]::Exists($dest)){$dest=Join-Path $EvidenceRoot (([string]$identity)+'.interrupted-'+[guid]::NewGuid().ToString('N')+'.lnk')}
    [IO.File]::Move($item.path,$dest)
   }
   if($item.exists){
    $null=[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($item.path))
    $f=[IO.File]::Open($item.path,'CreateNew','Write','None')
    try{$b=[Convert]::FromBase64String($item.bytes);$f.Write($b,0,$b.Length);$f.Flush($true)}finally{$f.Dispose()}
   }
  }
  if($item.exists){
   # Bytes may already match after interruption BEFORE security/timestamps restored.
   Set-FxTxMetadata $item.path $item.metadata $false
   [FxTxNativeStreams]::RestoreLegacyDacl($item.path,$item.metadata.sddl)
   Assert-FxTxSame (Get-FxTxMetadata $item.path) $item.metadata 'Shortcut metadata'
   if([Convert]::ToBase64String([IO.File]::ReadAllBytes($item.path)) -cne $item.bytes){throw 'Shortcut byte restoration failed'}
  }elseif([IO.File]::Exists($item.path)){throw 'New shortcut still active after rollback'}
 }
}
Export-ModuleMember -Function Get-FxTxAppendedPath,Assert-FxTxServiceScope,Read-FxTxServiceStatus,Set-FxTxPrivateDirectory,Assert-FxTxPrivateDirectory,Read-FxTxRegistryTree,Restore-FxTxRegistryTree,Assert-FxTxSame,Get-FxTxPlatformSnapshot,Apply-FxTxPlatformPath,Restore-FxTxPlatform
