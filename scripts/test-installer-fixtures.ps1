param([string]$PublishedGui='',[switch]$ComOnly,[string]$LocalOwnedRoot='')
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if(-not $ComOnly -and ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows')){throw 'Volume selfcheck is Windows CI only'}
if($ComOnly){
 if(-not $LocalOwnedRoot -or [IO.Directory]::Exists($LocalOwnedRoot)){throw 'COM-only check needs a new exact own scratch directory'}
 $root=[IO.Path]::GetFullPath($LocalOwnedRoot)
}else{$root=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_fixture-preflight_'+[guid]::NewGuid().ToString('N'))}
$null=[IO.Directory]::CreateDirectory($root)
Import-Module (Join-Path $PSScriptRoot 'native-shortcut-fixture.psm1') -Force
$rows=@();$image=''
try{
 foreach($variant in @('ascii-cmd','unicode-cmd','unicode-published')){
  if($variant -eq 'unicode-published' -and -not $PublishedGui){continue}
  $dir=if($variant -eq 'ascii-cmd'){Join-Path $root $variant}else{Join-Path $root ($variant+' '+[char]0x4e2d+[char]0x6587+' space')}
  $null=[IO.Directory]::CreateDirectory($dir)
  $target=Join-Path $dir 'FindX.exe'
  $input=if($variant -eq 'unicode-published'){$PublishedGui}else{Join-Path $env:SystemRoot 'System32\cmd.exe'}
  Copy-Item -LiteralPath $input -Destination $target
  Test-FxWshShortcutSupport $dir $target (Join-Path $dir 'shortcut-com-diagnostic.json')
  $link=Join-Path $dir 'real-native.lnk'
  New-FxNativeShortcutFixture $dir $target $link
  $rows+=@{case=$variant;nativeComRoundtrip=$true;target=$target;targetType=$target.GetType().FullName;targetExists=[IO.File]::Exists($target);shortcutSha256=(Get-FileHash -LiteralPath $link -Algorithm SHA256).Hash}
 }
 if(-not $ComOnly){
  Import-Module (Join-Path $PSScriptRoot 'test-owned-ntfs-volume.psm1') -Force
  $image=Join-Path $root 'owned-health.vhd'
  $volume=New-FxOwnedTestVolume $image ('FXTX'+[guid]::NewGuid().ToString('N').Substring(0,8).ToUpperInvariant())
  [IO.File]::WriteAllText((Join-Path $volume.Root 'owned-proof.txt'),'private volume preflight, no host scan')
  $rows+=@{case='owned-volume';root=$volume.Root;image=$image;ownedWriteRead=([IO.File]::ReadAllText((Join-Path $volume.Root 'owned-proof.txt')) -eq 'private volume preflight, no host scan')}
 }
 [ordered]@{scope='fixture-only native COM/typed CIM preflight, no installation or SCM';host=$PSVersionTable.PSVersion.ToString();cases=$rows;passed=$true} | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 (Join-Path $root 'results.json')
 Write-Output ('FIXTURE_PREFLIGHT_PASSED '+$root)
}catch{
 [ordered]@{passed=$false;rows=$rows;error=$_.Exception.ToString();position=$_.InvocationInfo.PositionMessage;stack=$_.ScriptStackTrace} | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 (Join-Path $root 'results.json')
 throw
}finally{
 if($image -and [IO.File]::Exists($image)){$di=Get-DiskImage -ImagePath $image -ErrorAction Stop;if($di.Attached){Dismount-DiskImage -ImagePath $image -ErrorAction Stop | Out-Null}}
}
