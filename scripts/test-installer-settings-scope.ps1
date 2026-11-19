param([ValidateSet('Primitives','SeedFormal','VerifyFormal')][string]$Mode='Primitives')
Set-StrictMode -Version Latest;$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP){throw 'Isolated CI only'}
Import-Module "$PSScriptRoot/../installer/transaction-files.psm1" -Force
function Snapshot([string]$p){$i=Get-Item -LiteralPath $p -Force;return @{sha=(Get-FileHash -LiteralPath $p).Hash;length=$i.Length;attributes=[int]$i.Attributes;sddl=(Get-Acl -LiteralPath $p).Sddl;creation=$i.CreationTimeUtc.Ticks;write=$i.LastWriteTimeUtc.Ticks}}
function Assert-Snapshot([string]$p,$before){$a=Snapshot $p;foreach($key in $a.Keys){if([string]$a[$key] -cne [string]$before.$key){throw "Untouched file changed: $p / $key"}}}
function Seed-Extras([string]$native){
 $null=New-Item -ItemType Directory -Path (Join-Path $native 'disk-usage-fixture') -Force
 $sparse=Join-Path $native 'disk-usage-fixture/sparse.bin'
 $f=[IO.File]::Open($sparse,'CreateNew','Write','None');try{$f.SetLength(16777216)}finally{$f.Dispose()}
 fsutil sparse setflag $sparse|Out-Null;if($LASTEXITCODE){throw 'Sparse fixture creation failed'}
 fsutil sparse setrange $sparse 0 16777216|Out-Null;if($LASTEXITCODE){throw 'Sparse fixture range failed'}
 [IO.File]::WriteAllText((Join-Path $native 'unknown-user-file.txt'),'unknown data must stay in place')
 [IO.File]::WriteAllText((Join-Path $native 'index.bin'),'unrelated native development index must stay in place')
 $result=@();foreach($rel in @('disk-usage-fixture/sparse.bin','unknown-user-file.txt','index.bin')){$p=Join-Path $native $rel;$result+=@{path=$p;before=(Snapshot $p)}}
 return $result
}
if($Mode -eq 'SeedFormal'){
 $native=Join-Path $env:LOCALAPPDATA 'FindX-native-dev'
 if(Test-Path $native){throw 'Formal config fixture requires pristine runner'}
 $extras=Seed-Extras $native
 $extras|ConvertTo-Json -Depth 8|Set-Content "$env:RUNNER_TEMP/formal-settings-extras.json"
 return
}
if($Mode -eq 'VerifyFormal'){
 foreach($f in @(Get-Content "$env:RUNNER_TEMP/formal-settings-extras.json" -Raw|ConvertFrom-Json)){Assert-Snapshot $f.path $f.before}
 'FORMAL_NON_FIXTURE_SPARSE_AND_UNKNOWN_UNCHANGED'|Set-Content "$env:RUNNER_TEMP/formal-settings-result.txt"
 return
}
$base=Join-Path $env:RUNNER_TEMP ('settings-scope-'+[guid]::NewGuid().ToString('N'))
$roots=[pscustomobject]@{app="$base/app";data="$base/data";nativeConfig="$base/native";legacyConfig="$base/legacy"}
foreach($p in @($roots.app,$roots.data,$roots.nativeConfig,$roots.legacyConfig)){$null=New-Item -ItemType Directory $p -Force}
[IO.File]::WriteAllText("$($roots.app)/owned.exe",'old app')
[IO.File]::WriteAllText("$($roots.data)/index.bin",'formal complete index')
[IO.File]::WriteAllText("$($roots.nativeConfig)/prefs.json",'{"version":1,"unknown":"retain preference bytes"}')
[IO.File]::WriteAllText("$($roots.legacyConfig)/findx2-gui-settings.json",'{"unknown":"retain legacy source"}')
$extras=Seed-Extras $roots.nativeConfig
$prefs=Join-Path $roots.nativeConfig 'prefs.json';$prefBefore=Snapshot $prefs
$legacy=Join-Path $roots.legacyConfig 'findx2-gui-settings.json';$legacyBefore=Snapshot $legacy
$map=Get-FxTxUpgradeBackupRoots $roots
$inventory=Get-FxTxInventory $map
if($inventory.bytes -ge 1048576 -or @($inventory.components).Count -ne 5){throw 'Backup traversed data outside owned scope'}
$backup=Join-Path $base 'backup';$evidence=Join-Path $base 'restore'
$null=New-FxTxVerifiedBackup $map $backup
# Simulate both overwritten existing and newly created owned preferences.
[IO.File]::WriteAllText($prefs,'new candidate preferences')
[IO.File]::WriteAllText("$($roots.nativeConfig)/settings.json",'new settings')
[IO.File]::WriteAllText($legacy,'changed source')
[IO.File]::WriteAllText("$($roots.data)/index.bin",'candidate index')
$lock=[IO.File]::Open($prefs,'Open','ReadWrite','None');$refused=$false
try{try{$null=Restore-FxTxVerifiedBackup $backup $evidence}catch{$refused=$true}}finally{$lock.Dispose()}
if(-not $refused){throw 'Locked owned settings must not report restored'}
$null=Test-FxTxBackup $backup (Read-FxTxVerifiedBackup $backup)
$null=Restore-FxTxVerifiedBackup $backup $evidence
$null=Restore-FxTxVerifiedBackup $backup $evidence
Assert-Snapshot $prefs $prefBefore;Assert-Snapshot $legacy $legacyBefore
if(Test-Path "$($roots.nativeConfig)/settings.json"){throw 'Originally absent owned file not restored to absence'}
foreach($f in $extras){Assert-Snapshot $f.path $f.before}
if([IO.File]::ReadAllText("$($roots.data)/index.bin") -cne 'formal complete index'){throw 'Production index protection lost'}
# Sparse remains refused when it IS a required owned settings file, with exact path.
fsutil sparse setflag $prefs|Out-Null;if($LASTEXITCODE){throw 'Protected sparse fixture setup failed'}
$refused=$false
try{$null=Get-FxTxInventory $map}catch{if($_.Exception.Message.Contains($prefs) -and $_.Exception.Message.Contains('SparseFile')){$refused=$true}else{throw}}
if(-not $refused){throw 'Protected sparse settings were not refused with actual object/reason'}
@{passed=$true;unknownSparseUntouched=$true;unknownIndexUntouched=$true;ownedPreferencesExactRestore=$true;absentSettingsRestore=$true;lockedRestoreRefusedThenReentered=$true;productionIndexRestored=$true;requiredSparseStillRefusedWithPath=$true}|ConvertTo-Json|Set-Content "$env:RUNNER_TEMP/settings-scope-result.json"
