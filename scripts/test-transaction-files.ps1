# Native, non-elevated, file-only regression harness. Keeps every fixture as evidence.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot '..\installer\transaction-files.psm1') -Force
$base = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ('..\target\tx-file-tests-' + [guid]::NewGuid().ToString('N'))))
foreach ($protected in @($env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:ProgramData)) {
    if ($protected -and $base.StartsWith($protected + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cannot be under a formal system install/data root' }
}
$null = [IO.Directory]::CreateDirectory($base)
$results = [Collections.Generic.List[object]]::new()
function Assert($value, [string]$why) { if (-not $value) { throw $why } }
function MustThrow([scriptblock]$action, [string]$why) {
    $threw = $false
    try { & $action | Out-Null } catch { $threw = $true }
    Assert $threw $why
}
function Case([string]$name, [scriptblock]$body) {
    try { & $body; $results.Add([ordered]@{ name = $name; passed = $true }); Write-Output ('PASS ' + $name) }
    catch { $results.Add([ordered]@{ name = $name; passed = $false; error = $_.Exception.ToString() }); Write-Output ('FAIL ' + $name + ': ' + $_.Exception.Message) }
}
function Fixture([string]$name) {
    $root = Join-Path $base $name
    $app = Join-Path $root 'app 中文 space'; $config = Join-Path $root '配置'; $data = Join-Path $root 'index state'
    foreach ($p in @($app,$config,$data,(Join-Path $app 'empty folder'),(Join-Path $data 'index.bin.v6\gen\1'))) { $null=[IO.Directory]::CreateDirectory($p) }
    [IO.File]::WriteAllBytes((Join-Path $app 'FindX.exe'), [byte[]](1,2,3,4,255))
    [IO.File]::WriteAllText((Join-Path $config 'settings.json'), '{"theme":"dark","unknown":"keep"}')
    [IO.File]::WriteAllText((Join-Path $data 'index.bin'), 'public synthetic index ONLY')
    [IO.File]::WriteAllText((Join-Path $data 'index.bin.v6\gen\1\entries.dat'), 'synthetic sidecar')
    [IO.File]::WriteAllText((Join-Path $data 'index.bin.tri.pending'), 'pending bits')
    return @{ Root=$root; App=$app; Config=$config; Data=$data; Roots=[ordered]@{app=$app;config=$config;data=$data}; Backup=(Join-Path $root 'backup') }
}
Case 'full exact bytes, all sidecars, unknown config, empty folders, Chinese/space paths' {
    $x=Fixture 'success'; $m=New-FxTxVerifiedBackup $x.Roots $x.Backup
    Assert ($m.files.Count -eq 5) 'Expected every synthetic source file'
    $r=Read-FxTxVerifiedBackup $x.Backup
    Assert ($r.files.Count -eq 5) 'Readback did not preserve complete manifest'
    Assert ([IO.Directory]::Exists((Join-Path $x.Backup 'app\empty folder'))) 'Empty folder missing'
    Assert ((Get-FxTxFileHash (Join-Path $x.App 'FindX.exe')) -eq (Get-FxTxFileHash (Join-Path $x.Backup 'app\FindX.exe'))) 'Wrong binary backup'
    Assert (@($r.files | Where-Object { -not $_.sddl }).Count -eq 0) 'Source ACL evidence absent'
}
Case 'pre-existing backup never overwritten or reused implicitly' {
    $x=Fixture 'existing'; $null=New-FxTxVerifiedBackup $x.Roots $x.Backup
    $hash=Get-FxTxFileHash (Join-Path $x.Backup 'backup.complete.json')
    MustThrow { New-FxTxVerifiedBackup $x.Roots $x.Backup } 'Existing backup accepted'
    Assert ((Get-FxTxFileHash (Join-Path $x.Backup 'backup.complete.json')) -eq $hash) 'Backup overwritten'
}
Case 'locked source fails closed with native FileShare.None, original unchanged' {
    $x=Fixture 'locked'; $f=Join-Path $x.Data 'index.bin'; $before=Get-FxTxFileHash $f
    $handle=[IO.File]::Open($f, 'Open', 'ReadWrite', 'None')
    try { MustThrow { New-FxTxVerifiedBackup $x.Roots $x.Backup } 'Locked source accepted' } finally { $handle.Dispose() }
    Assert (-not (Test-Path -LiteralPath $x.Backup)) 'Created backup despite locked source in preflight'
    Assert ((Get-FxTxFileHash $f) -eq $before) 'Source changed'
}
Case 'disk budget shortage fails before creating backup' {
    $x=Fixture 'capacity'
    MustThrow { New-FxTxVerifiedBackup $x.Roots $x.Backup -ReserveBytes ([decimal]::MaxValue / 2) } 'Impossible budget accepted'
    Assert (-not (Test-Path -LiteralPath $x.Backup)) 'Created backup with insufficient capacity'
}
Case 'corrupt backup cannot be accepted as verified' {
    $x=Fixture 'corrupt'; $null=New-FxTxVerifiedBackup $x.Roots $x.Backup
    [IO.File]::AppendAllText((Join-Path $x.Backup 'data\index.bin'), 'damage')
    MustThrow { Read-FxTxVerifiedBackup $x.Backup } 'Corruption accepted'
}
Case 'partial backup without commit record is not success' {
    $x=Fixture 'partial'; $null=[IO.Directory]::CreateDirectory($x.Backup)
    [IO.File]::WriteAllText((Join-Path $x.Backup 'inventory.pending.json'), '{}')
    MustThrow { Read-FxTxVerifiedBackup $x.Backup } 'Incomplete backup accepted'
}
Case 'traversal and alternate stream manifest paths refused' {
    $x=Fixture 'traversal'; $m=New-FxTxVerifiedBackup $x.Roots $x.Backup
    foreach ($bad in @('..\escape','C:\escape','file:stream','a\..\b','a\\b')) {
        $m.files[0].relative=$bad
        MustThrow { Test-FxTxBackup $x.Backup $m } ('Invalid path accepted: ' + $bad)
    }
}
Case 'nested backup root is refused before creating files' {
    $x=Fixture 'nested'; $bad=Join-Path $x.Data 'backup'
    MustThrow { New-FxTxVerifiedBackup $x.Roots $bad } 'Nested backup accepted'
    Assert (-not (Test-Path -LiteralPath $bad)) 'Created nested backup'
}
Case 'overlapping component roots refused' {
    $x=Fixture 'overlap'; $roots=[ordered]@{a=$x.Root;b=$x.Data}
    MustThrow { Get-FxTxInventory $roots } 'Overlapping sources accepted'
}
Case 'missing optional config is absence, not a fabricated empty file' {
    $x=Fixture 'missing'; $x.Roots['missing']=Join-Path $x.Root 'not yet created'
    $m=New-FxTxVerifiedBackup $x.Roots $x.Backup
    Assert (@($m.components | Where-Object { $_.name -eq 'missing' -and -not $_.exists }).Count -eq 1) 'Absence lost'
    Assert (-not (Test-Path -LiteralPath (Join-Path $x.Backup 'missing'))) 'Invented source state'
}
Case 'native NTFS junction is rejected, not recursively followed' {
    $x=Fixture 'junction'; $target=Join-Path $x.Root 'unrelated fixture'; $null=[IO.Directory]::CreateDirectory($target)
    $link=Join-Path $x.Data 'junction'; $null=New-Item -ItemType Junction -Path $link -Target $target
    MustThrow { New-FxTxVerifiedBackup $x.Roots $x.Backup } 'Junction traversed'
}
Case 'removed empty directory invalidates complete backup' {
    $x=Fixture 'empty-missing'; $null=New-FxTxVerifiedBackup $x.Roots $x.Backup
    [IO.Directory]::Delete((Join-Path $x.Backup 'app\empty folder'))
    MustThrow { Read-FxTxVerifiedBackup $x.Backup } 'Missing directory accepted'
}
Case 'restore exact old complete set and retain failed unknown bytes without delete' {
    $x=Fixture 'restore';$old=Get-FxTxFileHash (Join-Path $x.Data 'index.bin')
    $null=New-FxTxVerifiedBackup $x.Roots $x.Backup
    [IO.File]::WriteAllText((Join-Path $x.Data 'index.bin'),'failed new index')
    [IO.File]::WriteAllText((Join-Path $x.App 'unknown-user-file.txt'),'retain as failed evidence')
    $evidence=Join-Path $x.Root 'failed-new'
    $null=Restore-FxTxVerifiedBackup $x.Backup $evidence
    Assert ((Get-FxTxFileHash (Join-Path $x.Data 'index.bin')) -eq $old) 'Old index bytes not restored'
    Assert ([IO.File]::Exists((Join-Path $evidence 'app\unknown-user-file.txt'))) 'Unknown file deleted'
    Assert ([IO.File]::Exists((Join-Path $evidence 'restore.complete.json'))) 'Missing verified restore record'
    $null=Read-FxTxVerifiedBackup $x.Backup
}
Case 'locked destination rollback fails without deleting source or backup' {
    $x=Fixture 'restore-locked';$null=New-FxTxVerifiedBackup $x.Roots $x.Backup
    $handle=[IO.File]::Open((Join-Path $x.Data 'index.bin'),'Open','ReadWrite','None')
    try { MustThrow { Restore-FxTxVerifiedBackup $x.Backup (Join-Path $x.Root 'failed-new') } 'Locked active tree was silently accepted' } finally { $handle.Dispose() }
    Assert ([IO.File]::Exists((Join-Path $x.Data 'index.bin'))) 'Locked original disappeared'
    $null=Read-FxTxVerifiedBackup $x.Backup
    Assert (-not [IO.File]::Exists((Join-Path $x.Root 'failed-new\restore.complete.json'))) 'Incomplete restore marked complete'
}
$summary=[ordered]@{ at=(Get-Date -Format o); fixtureRoot=$base; passed=@($results | Where-Object {$_.passed}).Count; failed=@($results | Where-Object {-not $_.passed}).Count; cases=@($results.ToArray()); officialTouched=$false; processOrScmCalls=$false; installerIntegrated=$false; note='File-only primitives. NOT a proof of writer shutdown, save acknowledgment, or official installer integration; restore tests cover ordinary owned fixture files only.' }
$summary | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $base 'results.json') -Encoding UTF8
$summary | ConvertTo-Json -Depth 10
if ($summary.failed) { exit 1 }
