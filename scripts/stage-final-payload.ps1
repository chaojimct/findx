param([string]$GuiRoot,[string]$Destination,[string]$ReusedCliCommit="",[long]$ReusedCliRun=0)
Set-StrictMode -Version Latest;$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS-ne'true'-or$env:RUNNER_OS-ne'Windows'-or$env:COMPUTERNAME-eq'MZ'){throw 'Disposable CI only'}
$head=git rev-parse HEAD;if($LASTEXITCODE-or$head-cne$env:GITHUB_SHA-or@(git status --porcelain --untracked-files=no).Count){throw 'Exact clean source required'}
$m=Get-Content (Join-Path $GuiRoot 'source-manifest.json') -Raw|ConvertFrom-Json
if($m.sourceCommit-cne'b72219bc844de5eb141bb99a708494c956f53073'-or$m.sha256-cne'E10DDDA898CAFE95CAF86B7F119319DCA70E0E7224ACDC28984572AD633A839E'-or$m.bytes-ne35951616){throw 'Wrong immutable GUI source/hash'}
$tree=git rev-parse HEAD:crates/findx2-gui-native;if($LASTEXITCODE-or$tree-cne$m.guiTree){throw 'Current GUI source differs; cannot reuse GUI'}
$ipc=git rev-parse HEAD:crates/findx2-ipc;if($LASTEXITCODE-or$ipc-cne$m.ipcTree){throw 'GUI IPC source differs'}
if((Get-FileHash (Join-Path $GuiRoot $m.file)).Hash-cne$m.sha256){throw 'GUI download digest mismatch'}
if(Test-Path $Destination){throw 'Never overwrite staged payload'};$null=New-Item -ItemType Directory $Destination
Copy-Item (Join-Path $GuiRoot $m.file) (Join-Path $Destination 'FindX.exe')
foreach($n in @('findx2-service.exe','findx2.exe','fx.exe')){Copy-Item (Join-Path 'target/release' $n) (Join-Path $Destination $n)}
$v=& (Join-Path $Destination 'findx2-service.exe') --version;if($LASTEXITCODE-or$v-cne'findx2-service 2.5.3'){throw 'Engine version mismatch'}
[IO.File]::WriteAllBytes((Join-Path $Destination 'FindX.installed'),[byte[]](0x31,13,10));$null=New-Item -ItemType Directory (Join-Path $Destination 'resources')
if($ReusedCliCommit){
 if($ReusedCliCommit-cne'0157cca03ea4e24c803624da968ee741393ba1b1'-or$ReusedCliRun-ne44475535981){throw 'Unknown reused CLI origin'}
 foreach($p in @('crates/findx2-core','crates/findx2-windows','crates/findx2-cli','crates/findx2-ipc','Cargo.toml','Cargo.lock')){
  $a=git rev-parse "HEAD:$p";if($LASTEXITCODE){throw 'CLI source missing'};$b=git rev-parse "${ReusedCliCommit}:$p";if($LASTEXITCODE-or$a-cne$b){throw 'Reused CLI source changed'}
 }
}
$components=@(foreach($n in @('FindX.exe','findx2-service.exe','findx2.exe','fx.exe')){[ordered]@{name=$n;sourceCommit=$(if($n-eq'FindX.exe'){$m.sourceCommit}elseif($n-ne'findx2-service.exe'-and$ReusedCliCommit){$ReusedCliCommit}else{$head});sha256=(Get-FileHash (Join-Path $Destination $n)).Hash.ToLowerInvariant()}})
[ordered]@{version='2.5.3';sourceCommit=$head;engineUnchanged=$false;engineBaseline='v2.5.2 source plus authorized fixes';components=$components;guiBuildRun=44468255654;reusedCliBuildRun=$ReusedCliRun}|ConvertTo-Json -Depth 8|Set-Content (Join-Path $Destination 'resources/component-manifest.json') -Encoding utf8
$files=@(Get-ChildItem $Destination -Recurse -File|ForEach-Object{[ordered]@{name=$_.FullName.Substring($Destination.Length+1);length=$_.Length;sha256=(Get-FileHash $_.FullName).Hash.ToLowerInvariant()}})
if($files.Count-ne6){throw 'Exactly six final payload files required'}
[ordered]@{version='2.5.3';sourceCommit=$head;engineUnchanged=$false;guiSourceCommit=$m.sourceCommit;files=$files}|ConvertTo-Json -Depth 10|Set-Content (Join-Path $Destination 'stage-manifest.json') -Encoding utf8
