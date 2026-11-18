# Privileged tests run ONLY in the disposable CI runner, never on a user's machine.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true'){throw 'Native registry/privileged-security tests are restricted to isolated CI'}
$repo=Split-Path $PSScriptRoot -Parent
Import-Module (Join-Path $repo 'installer\transaction-files.psm1') -Force -Global
Import-Module (Join-Path $repo 'installer\transaction-platform.psm1') -Force -Global
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
  $map=[ordered]@{app=$app};$old=Get-FxTxInventory $map;$backup=Join-Path $d 'backup';$null=New-FxTxVerifiedBackup $map $backup -AdministratorsOnly
  [IO.File]::WriteAllText((Join-Path $app 'prefs.json'),'new');$null=Restore-FxTxVerifiedBackup $backup (Join-Path $d 'failed-new')
  Assert-FxTxSame (Get-FxTxInventory $map) $old 'Full file metadata'
  $null=Restore-FxTxVerifiedBackup $backup (Join-Path $d 'failed-new')
 }
 Case 'named file and directory streams rejected before blessing a lossy backup' {
  $d=Join-Path $root 'streams';$null=[IO.Directory]::CreateDirectory($d);[IO.File]::WriteAllText((Join-Path $d 'ordinary.txt'),'ordinary');[IO.File]::WriteAllText((Join-Path $d 'ordinary.txt:owned-stream'),'preserve')
  $refused=$false;try{$null=Get-FxTxInventory ([ordered]@{app=$d})}catch{$refused=$true};Assert $refused 'Named file stream silently dropped'
  Assert ([IO.File]::ReadAllText((Join-Path $d 'ordinary.txt:owned-stream')) -eq 'preserve') 'Original stream changed'
  $dir=Join-Path $root 'directory stream';$null=[IO.Directory]::CreateDirectory($dir);[IO.File]::WriteAllText(($dir+':owned-stream'),'directory bytes')
  $refused=$false;try{$null=Get-FxTxInventory ([ordered]@{app=$dir})}catch{$refused=$true};Assert $refused 'Named directory stream silently dropped'
  Assert ([IO.File]::ReadAllText(($dir+':owned-stream')) -eq 'directory bytes') 'Directory stream changed'
 }
}finally{$base.DeleteSubKeyTree($regPath,$false);$base.Dispose()}
[ordered]@{cases=@($rows.ToArray());passed=@($rows | Where-Object {$_.passed}).Count;failed=@($rows | Where-Object {-not $_.passed}).Count;scope='owned CI registry and NTFS fixtures only; not full installer acceptance'} | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 (Join-Path $root 'results.json')
if(@($rows | Where-Object {-not $_.passed}).Count){throw 'Native privileged platform regressions failed'}
