# Privileged tests run ONLY in the disposable CI runner, never on a user's machine.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true'){throw 'Native registry/privileged-security tests are restricted to isolated CI'}
$repo=Split-Path $PSScriptRoot -Parent
Import-Module (Join-Path $repo 'installer\transaction-files.psm1') -Force -Global
Import-Module (Join-Path $repo 'installer\transaction-platform.psm1') -Force -Global
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class FxTxOwnedFixtureStream {
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern SafeFileHandle CreateFileW(string p,uint a,uint s,IntPtr x,uint d,uint f,IntPtr t);
 public static void Write(string p,string value){using(var h=CreateFileW(p,0x40000000,0,IntPtr.Zero,2,0x02000000,IntPtr.Zero)){if(h.IsInvalid)throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());using(var f=new FileStream(h,FileAccess.Write)){byte[] b=new UTF8Encoding(false).GetBytes(value);f.Write(b,0,b.Length);f.Flush(true);}}}
 public static string Read(string p){using(var h=CreateFileW(p,0x80000000,1,IntPtr.Zero,3,0x02000000,IntPtr.Zero)){if(h.IsInvalid)throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());using(var f=new FileStream(h,FileAccess.Read)){using(var r=new StreamReader(f,new UTF8Encoding(false))){return r.ReadToEnd();}}}}
}
'@
$id=[guid]::NewGuid().ToString('N');$root=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_platform_'+$id)
$null=[IO.Directory]::CreateDirectory($root);$rows=[Collections.Generic.List[object]]::new()
function Assert($Ok,[string]$Message){if(-not $Ok){throw $Message}}
function Case([string]$Name,[scriptblock]$Body){try{& $Body;$rows.Add([ordered]@{name=$Name;passed=$true});Write-Output ('PASS '+$Name)}catch{$rows.Add([ordered]@{name=$Name;passed=$false;error=$_.Exception.ToString()});Write-Output ('FAIL '+$Name+' '+$_.Exception.Message)}}
Case 'store is administrator-owned with only administrator/System DACL from creation' {
 $p=Join-Path $root 'protected store';Set-FxTxPrivateDirectory $p;Assert-FxTxPrivateDirectory $p
 Assert ((Get-Acl $p).GetOwner([Security.Principal.SecurityIdentifier]).Value -eq 'S-1-5-32-544') 'Wrong store owner'
}
$regPath='Software\FindXUpgradeTest_'+$id;$spec=[pscustomobject]@{hive='CurrentUser';path=$regPath}
$base=[Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser,[Microsoft.Win32.RegistryView]::Registry64)
try {
 Case 'typed unknown registry values, child values and existing security restored' {
  $key=$base.CreateSubKey($regPath)
  try{$key.SetValue('string','%TEMP%\中文',[Microsoft.Win32.RegistryValueKind]::ExpandString);$key.SetValue('multi',[string[]]@('one','二'),[Microsoft.Win32.RegistryValueKind]::MultiString);$key.SetValue('binary',[byte[]](0,255,7),[Microsoft.Win32.RegistryValueKind]::Binary);$key.SetValue('none',[byte[]](8,9),[Microsoft.Win32.RegistryValueKind]::None);$key.SetValue('qword',[long]9223372036854775806,[Microsoft.Win32.RegistryValueKind]::QWord)}finally{$key.Dispose()}
  $old=Read-FxTxRegistryTree $spec;$key=$base.OpenSubKey($regPath,$true);try{$key.SetValue('string','changed');$key.SetValue('candidate-only',1);$child=$key.CreateSubKey('candidate');$child.SetValue('new',2);$child.Dispose()}finally{$key.Dispose()}
  Restore-FxTxRegistryTree $spec $old;Assert-FxTxSame (Read-FxTxRegistryTree $spec) $old 'Typed registry'
 }
 Case 'complete root/directory/file security and timestamps survive restore' {
  Enable-FxTxFullSecurity
  $d=Join-Path $root 'metadata';$app=Join-Path $d 'app';$null=[IO.Directory]::CreateDirectory((Join-Path $app 'empty 中文'));[IO.File]::WriteAllText((Join-Path $app 'prefs.json'),'old')
  # Exercise NONEMPTY explicit SACLs, not merely a descriptor with no audit entries.
  foreach($target in @($app,(Join-Path $app 'empty 中文'),(Join-Path $app 'prefs.json'))){
   $acl=Get-Acl -LiteralPath $target -Audit
   $acl.AddAuditRule([Security.AccessControl.FileSystemAuditRule]::new([Security.Principal.WindowsIdentity]::GetCurrent().User,[Security.AccessControl.FileSystemRights]::ReadAndExecute,[Security.AccessControl.AuditFlags]::Success))
   Set-Acl -LiteralPath $target -AclObject $acl
   Assert ((Get-Acl -LiteralPath $target -Audit).Audit.Count -gt 0) 'No positive audit rule present'
  }
  $map=[ordered]@{app=$app};$old=Get-FxTxInventory $map;$backup=Join-Path $d 'backup';$null=New-FxTxVerifiedBackup $map $backup -AdministratorsOnly
  [IO.File]::WriteAllText((Join-Path $app 'prefs.json'),'new');$null=Restore-FxTxVerifiedBackup $backup (Join-Path $d 'failed-new')
  Assert-FxTxSame (Get-FxTxInventory $map) $old 'Full file metadata'
  $null=Restore-FxTxVerifiedBackup $backup (Join-Path $d 'failed-new')
 }
 Case 'named file and directory streams rejected before blessing a lossy backup' {
  $d=Join-Path $root 'streams';$null=[IO.Directory]::CreateDirectory($d);[IO.File]::WriteAllText((Join-Path $d 'ordinary.txt'),'ordinary');[FxTxOwnedFixtureStream]::Write((Join-Path $d 'ordinary.txt:owned-stream'),'preserve')
  $refused=$false;try{$null=Get-FxTxInventory ([ordered]@{app=$d})}catch{$refused=$true};Assert $refused 'Named file stream silently dropped'
  Assert ([FxTxOwnedFixtureStream]::Read((Join-Path $d 'ordinary.txt:owned-stream')) -eq 'preserve') 'Original stream changed'
  $dir=Join-Path $root 'directory stream';$null=[IO.Directory]::CreateDirectory($dir);[FxTxOwnedFixtureStream]::Write(($dir+':owned-stream'),'directory bytes')
  $refused=$false;try{$null=Get-FxTxInventory ([ordered]@{app=$dir})}catch{$refused=$true};Assert $refused 'Named directory stream silently dropped'
  Assert ([FxTxOwnedFixtureStream]::Read(($dir+':owned-stream')) -eq 'directory bytes') 'Directory stream changed'
 }
 function Open-FixtureEnvironment($Snapshot) {return $base.OpenSubKey($Snapshot.environmentSpec.path,$true)}
 function New-PrivatePlatform([string]$Suffix,[string]$Original,[string[]]$Links) {
  $prefix=$regPath+'\'+$Suffix
  $environment=[pscustomobject]@{hive='CurrentUser';path=$prefix+'\Environment'}
  $key=$base.CreateSubKey($environment.path);try{$key.SetValue('Path',$Original,[Microsoft.Win32.RegistryValueKind]::ExpandString)}finally{$key.Dispose()}
  $app=Join-Path $root ('app '+$Suffix)
  $snap=Get-FxTxPlatformSnapshot ('FindXUpgradeTest-FindXUpgradeTest_'+$id+'_is1') ('FindXUpgradeTest_'+$id) $Links $app $true
  $snap.environmentSpec=$environment;$snap.path=[ordered]@{name='Path';exists=$true;kind='ExpandString';value=$Original};$snap.newPath=Get-FxTxAppendedPath $Original $app $true
  foreach($name in @('service','uninstall')){$spec=[pscustomobject]@{hive='CurrentUser';path=$prefix+'\'+$name};$snap[($name+'Spec')]=$spec;$snap[$name]=Read-FxTxRegistryTree $spec}
  $snap.runSpec=[pscustomobject]@{hive='CurrentUser';path=$prefix+'\Run'};$snap.run=[ordered]@{name='FindX';exists=$false;kind='';value=$null}
  return $snap
 }
 Case 'PATH preserves every preexisting byte and type, duplicate append and conflict fail closed' {
  $original=';C:\Existing;;%SystemRoot%;'
  $snap=New-PrivatePlatform 'path' $original @()
  Assert ($snap.newPath.StartsWith($original,[StringComparison]::Ordinal)) 'Original PATH empties/bytes rewritten'
  Assert ((Get-FxTxAppendedPath $snap.newPath (Join-Path $root 'app path') $true) -ceq $snap.newPath) 'Duplicate append'
  Apply-FxTxPlatformPath $snap;Restore-FxTxPlatform $snap (Join-Path $root 'path-evidence')
  $key=Open-FixtureEnvironment $snap
  try{Assert ($key.GetValue('Path',$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) -ceq $original) 'PATH not exactly restored';Assert ($key.GetValueKind('Path') -eq 'ExpandString') 'PATH type lost'}finally{$key.Dispose()}
  Apply-FxTxPlatformPath $snap;$key=Open-FixtureEnvironment $snap
  try{$key.SetValue('Path','external-edit',[Microsoft.Win32.RegistryValueKind]::ExpandString)}finally{$key.Dispose()}
  $refused=$false;try{Restore-FxTxPlatform $snap (Join-Path $root 'path-evidence')}catch{$refused=$true};Assert $refused 'Concurrent PATH edit overwritten'
  $key=Open-FixtureEnvironment $snap;try{Assert ($key.GetValue('Path') -ceq 'external-edit') 'External PATH edit lost'}finally{$key.Dispose()}
 }
 Case 'shortcut recovery resumes stable leaf identities and metadata, retains partial evidence and is idempotent' {
  $dir=Join-Path $root 'shortcut fixtures';$null=[IO.Directory]::CreateDirectory($dir)
  $links=@((Join-Path $dir 'first.lnk'),(Join-Path $dir 'second.lnk'))
  foreach($link in $links){[IO.File]::WriteAllText($link,'original '+$link)}
  $snap=New-PrivatePlatform 'links' 'unchanged-path' $links
  $evidence=Join-Path $root 'shortcut evidence';$null=[IO.Directory]::CreateDirectory($evidence)
  [IO.File]::WriteAllText((Join-Path $evidence '0.lnk'),'first failed bytes');[IO.File]::WriteAllText((Join-Path $evidence '1.lnk'),'second failed bytes')
  [IO.File]::SetLastWriteTimeUtc($links[0],[datetime]::UtcNow.AddDays(-2))
  [IO.File]::WriteAllText($links[1],'partial old restoration')
  Restore-FxTxPlatform $snap $evidence
  foreach($entry in $snap.shortcuts){Assert-FxTxSame (Get-FxTxMetadata $entry.path) $entry.metadata 'Shortcut exact metadata';Assert ([Convert]::ToBase64String([IO.File]::ReadAllBytes($entry.path)) -ceq $entry.bytes) 'Shortcut bytes mismatch'}
  Assert ([IO.File]::ReadAllText((Join-Path $evidence '0.lnk')) -ceq 'first failed bytes') 'First failed bytes overwritten'
  Assert ([IO.File]::ReadAllText((Join-Path $evidence '1.lnk')) -ceq 'second failed bytes') 'Second failed bytes overwritten'
  $count=[IO.Directory]::GetFiles($evidence).Count;Assert ($count -eq 3) 'Partial failed restore bytes not quarantined'
  Restore-FxTxPlatform $snap $evidence;Assert ([IO.Directory]::GetFiles($evidence).Count -eq $count) 'Idempotent restore grew evidence'
 }
 Case 'service command exact index and endpoint scope, no arbitrary extra writes or duplicate args' {
  $exe=Join-Path $root 'app 中文 space\findx2-service.exe';$index=Join-Path $root 'data\index.bin';$pipe='findx-upgrade-test-'+$id
  $good='"'+$exe+'" --service --index "'+$index+'" --volume C: --pipe '+$pipe+' --save-interval-secs 30'
  Assert-FxTxServiceScope $good $exe $index $pipe
  foreach($bad in @($good.Replace($index,(Join-Path $root 'outside\index.bin')),($good+' --index "'+$index+'"'),($good+' --shutdown-file unexpected'),$good.Replace($pipe,'another-pipe'))){
   $refused=$false;try{Assert-FxTxServiceScope $bad $exe $index $pipe}catch{$refused=$true};Assert $refused 'Unexpected service argument scope accepted'
  }
 }
}finally{$base.DeleteSubKeyTree($regPath,$false);$base.Dispose()}
[ordered]@{cases=@($rows.ToArray());passed=@($rows | Where-Object {$_.passed}).Count;failed=@($rows | Where-Object {-not $_.passed}).Count;scope='owned CI registry and NTFS fixtures only; not full installer acceptance'} | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 (Join-Path $root 'results.json')
if(@($rows | Where-Object {-not $_.passed}).Count){throw 'Native privileged platform regressions failed'}
