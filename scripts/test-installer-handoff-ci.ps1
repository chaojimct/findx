# Isolated GitHub Actions handoff probe. Not a production install and not UAC consent proof.
# Uses the published 2.5.3 FindX.exe bytes. Refuses the owner's machine and FindX2Search.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') { throw 'Disposable Windows Actions only' }
if ($env:COMPUTERNAME -eq 'MZ' -or $env:USERPROFILE -eq 'C:\Users\chaoj') { throw 'Owner machine refused' }
if (Get-Service -Name 'FindX2Search' -ErrorAction SilentlyContinue) { throw 'Production FindX2Search is present; refusing' }
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$txScript = Join-Path $repo 'installer\upgrade-transaction.ps1'
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$evidence = Join-Path $env:RUNNER_TEMP 'handoff-evidence'
$null = New-Item -ItemType Directory -Force -Path $evidence
$publishedSetup = '9f6663cd13e13d711f98564a2d3e69c218296023162d0abab4a4af62fd17010e'
$publishedGui = 'e10ddda898cafe95caf86b7f119319dca70e0e7224acdc28984572ad633a839e'
$unpackerHash = 'f8e45c9676bebc7d7570108ab572673b541fcf7a7d3d24a04319ac6b5315b4e8'
$id = [guid]::NewGuid().ToString('N')
$service = 'FindXUpgradeTest_' + $id
$pipe = 'findx-upgrade-test-' + $id
$uninstall = 'FindXUpgradeTest-' + $service + '_is1'
$base = Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_' + $id)
$app = Join-Path $base 'app'
$other = Join-Path $base 'other-app'
$data = Join-Path $base 'data'
$native = Join-Path $base 'native'
$legacy = Join-Path $base 'legacy'
$group = Join-Path $base 'group'
$desktop = Join-Path $base 'desktop'
$txParent = Join-Path $base 'tx'
$limits = New-Object System.Collections.Generic.List[string]
$result = [ordered]@{
  kind = 'isolated-actions-handoff'
  productionAcceptance = $false
  uacConsentClicked = $false
  productionServiceTouched = $false
  publishedSetupSha256 = $publishedSetup
  expectedGuiSha256 = $publishedGui
  powershell = $PSVersionTable.PSVersion.ToString()
  powershellEdition = [string]$PSVersionTable.PSEdition
  computer = $env:COMPUTERNAME
  runId = [string]$env:GITHUB_RUN_ID
  runUrl = ('https://github.com/chaojimct/findx/actions/runs/' + $env:GITHUB_RUN_ID)
  sha = [string]$env:GITHUB_SHA
  serviceNamespace = $service
  pipeNamespace = $pipe
  trayQuitVerified = $false
  mediumHelperVerified = $false
  timeoutSelfExit = $false
  fixtureCompiled = $false
  fixtureHandoffVerified = $false
}
function Save-Result {
  $result.limits = @($limits)
  ($result | ConvertTo-Json -Depth 8) | Set-Content -LiteralPath (Join-Path $evidence 'result.json') -Encoding UTF8
}
function Assert($ok, $why) { if (-not $ok) { throw $why } }
function Read-Text([string]$Path) {
  if (-not [IO.File]::Exists($Path)) { return '' }
  return [IO.File]::ReadAllText($Path)
}
if (-not ('FxHandoffProbe' -as [type])) {
  Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class FxHandoffProbe {
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint a, bool i, int pid);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr h);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr p, uint a, out IntPtr t);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool GetTokenInformation(IntPtr t, int cls, IntPtr info, int len, out int ret);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertSidToStringSid(IntPtr sid, out IntPtr s);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
  [DllImport("ntdll.dll")] static extern int NtSuspendProcess(IntPtr h);
  [DllImport("ntdll.dll")] static extern int NtResumeProcess(IntPtr h);
  public static string Integrity(int pid) {
    IntPtr proc = OpenProcess(0x1000, false, pid);
    if (proc == IntPtr.Zero) return "";
    try {
      IntPtr token;
      if (!OpenProcessToken(proc, 0x0008, out token)) return "";
      try {
        int needed;
        GetTokenInformation(token, 25, IntPtr.Zero, 0, out needed);
        if (needed <= 0) return "";
        IntPtr buf = Marshal.AllocHGlobal(needed);
        try {
          int got;
          if (!GetTokenInformation(token, 25, buf, needed, out got)) return "";
          IntPtr sid = Marshal.ReadIntPtr(buf);
          IntPtr str;
          if (!ConvertSidToStringSid(sid, out str)) return "";
          try { return Marshal.PtrToStringUni(str); } finally { LocalFree(str); }
        } finally { Marshal.FreeHGlobal(buf); }
      } finally { CloseHandle(token); }
    } finally { CloseHandle(proc); }
  }
  static int Act(int pid, bool resume) {
    IntPtr proc = OpenProcess(0x0800, false, pid);
    if (proc == IntPtr.Zero) return Marshal.GetLastWin32Error();
    try { return resume ? NtResumeProcess(proc) : NtSuspendProcess(proc); } finally { CloseHandle(proc); }
  }
  public static int Suspend(int pid) { return Act(pid, false); }
  public static int Resume(int pid) { return Act(pid, true); }
}
'@
}
$gui = $null
$locker = $null
$stay = $null
try {
  $result.installerElevated = [bool]([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
  $result.installerIntegrity = [FxHandoffProbe]::Integrity($PID)
  $session = (Get-Process -Id $PID).SessionId
  $result.explorerCount = @(Get-CimInstance Win32_Process -Filter "Name='explorer.exe' AND SessionId=$session").Count
  $result.sessionId = $session
  foreach ($dir in @($app, $other, $data, $native, $legacy, $group, $desktop, $txParent)) { $null = New-Item -ItemType Directory -Force -Path $dir }
  $fetch = Join-Path $evidence 'fetch'
  $null = New-Item -ItemType Directory -Force -Path $fetch
  $setupZip = Join-Path $fetch 'FindX-2.5.3-setup.exe'
  Invoke-WebRequest -UseBasicParsing 'https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-setup.exe' -OutFile $setupZip
  $setupHash = (Get-FileHash -LiteralPath $setupZip -Algorithm SHA256).Hash.ToLowerInvariant()
  Assert ($setupHash -eq $publishedSetup) ('Published setup bytes changed: ' + $setupHash)
  $unpackerZip = Join-Path $fetch 'innounp.zip'
  Invoke-WebRequest -UseBasicParsing 'https://raw.githubusercontent.com/jrathlev/InnoUnpacker-Windows-GUI/refs/heads/master/innounp-2/bin/innounp-2.zip' -OutFile $unpackerZip
  Assert ((Get-FileHash -LiteralPath $unpackerZip -Algorithm SHA256).Hash.ToLowerInvariant() -eq $unpackerHash) 'Unpacker bytes changed'
  Expand-Archive -LiteralPath $unpackerZip -DestinationPath (Join-Path $fetch 'extractor')
  & (Join-Path $fetch 'extractor\innounp.exe') -x -y ('-d' + (Join-Path $fetch 'old')) $setupZip *> (Join-Path $evidence 'unpack.log')
  Assert ($LASTEXITCODE -eq 0) 'Published setup unpack failed'
  $published = @(Get-ChildItem -LiteralPath (Join-Path $fetch 'old') -Recurse -Filter 'FindX.exe')
  Assert ($published.Count -eq 1) 'Published setup did not contain exactly one FindX.exe'
  $guiHash = (Get-FileHash -LiteralPath $published[0].FullName -Algorithm SHA256).Hash.ToLowerInvariant()
  $guiVersion = [string]$published[0].VersionInfo.ProductVersion
  $result.guiSha256 = $guiHash
  $result.guiProductVersion = $guiVersion
  Assert ($guiHash -eq $publishedGui) ('Published GUI is not the sealed 2.5.3 image: ' + $guiHash)
  Assert ($guiVersion -eq '2.5.3') ('Published GUI version is ' + $guiVersion)
  Copy-Item -LiteralPath $published[0].FullName -Destination (Join-Path $app 'FindX.exe')
  Copy-Item -LiteralPath $published[0].FullName -Destination (Join-Path $other 'FindX.exe')
  $publish = Join-Path $fetch 'publish'
  $null = New-Item -ItemType Directory -Force -Path $publish
  Copy-Item -Path (Join-Path $published[0].DirectoryName '*') -Destination $publish -Recurse -Force
  $payloadManifest = Join-Path $publish 'payload-manifest.json'
  if ((Test-Path -LiteralPath $payloadManifest) -and -not (Test-Path -LiteralPath (Join-Path $publish 'stage-manifest.json'))) {
    Copy-Item -LiteralPath $payloadManifest -Destination (Join-Path $publish 'stage-manifest.json')
  }
  function New-Tx { Join-Path $txParent ([guid]::NewGuid().ToString('N')) }
  function Read-Outcome([string]$Tx) {
    $reason = (Read-Text (Join-Path $Tx 'reason.code')).Trim()
    $failure = Read-Text (Join-Path $Tx 'failure.txt')
    $state = ''
    $journal = Join-Path $Tx 'journal.json'
    if ([IO.File]::Exists($journal)) { $state = [string](Get-Content -LiteralPath $journal -Raw -Encoding UTF8 | ConvertFrom-Json).state }
    return [pscustomobject]@{ reason = $reason; failure = $failure; state = $state }
  }
  function Start-Prepare([string]$Tx, [string]$App, [int]$Timeout, [string]$Fault) {
    $arg = @(
      '-NoProfile', '-NonInteractive', '-File', $txScript, '-Phase', 'Prepare', '-TransactionRoot', $Tx,
      '-InstallRoot', $App, '-DataRoot', $data, '-NativeConfigRoot', $native, '-LegacyConfigRoot', $legacy,
      '-ServiceName', $service, '-PipeName', $pipe, '-ExpectedVersion', '2.5.3', '-InstallService', '0',
      '-TimeoutSeconds', "$Timeout", '-AddPath', '0', '-UninstallKey', $uninstall, '-GroupRoot', $group,
      '-DesktopRoot', $desktop, '-CreateGroup', '0', '-TestBuild', '-Fault', $Fault
    )
    return Start-Process -FilePath $powershell -ArgumentList $arg -PassThru -WindowStyle Hidden
  }
  function Wait-Prepare($Proc, [int]$Seconds) {
    if (-not $Proc.WaitForExit($Seconds * 1000)) {
      $kids = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.ParentProcessId -eq $Proc.Id -and $_.CommandLine -match 'RequestQuit' })
      foreach ($kid in $kids) { Stop-Process -Id $kid.ProcessId -Force -ErrorAction SilentlyContinue }
      Stop-Process -Id $Proc.Id -Force
      throw 'Prepare exceeded its bounded wait; installer process was stopped and the published GUI was left alone'
    }
    $Proc.Refresh()
  }
  function Start-OwnedPrepare([string]$Tx, [string]$App, [int]$Timeout, [string]$Fault, [switch]$Stay) {
    $parentPath = Join-Path $evidence ('parent-' + [IO.Path]::GetFileName($Tx) + '.ps1')
    $stayFile = Join-Path $evidence ('stay-' + [IO.Path]::GetFileName($Tx) + '.flag')
    $lines = @(
      "`$p = Start-Process -FilePath '$powershell' -ArgumentList @('-NoProfile','-NonInteractive','-File','$txScript','-Phase','Prepare','-TransactionRoot','$Tx','-InstallRoot','$App','-DataRoot','$data','-NativeConfigRoot','$native','-LegacyConfigRoot','$legacy','-ServiceName','$service','-PipeName','$pipe','-ExpectedVersion','2.5.3','-InstallService','0','-TimeoutSeconds','$Timeout','-AddPath','0','-UninstallKey','$uninstall','-GroupRoot','$group','-DesktopRoot','$desktop','-CreateGroup','0','-TestBuild','-Fault','$Fault') -PassThru -WindowStyle Hidden",
      'Wait-Process -Id $p.Id',
      "Set-Content -LiteralPath '$stayFile' `$p.ExitCode"
    )
    if ($Stay) { $lines += "while (-not (Test-Path -LiteralPath '$stayFile.release')) { Start-Sleep -Milliseconds 200 }" }
    $lines += 'exit $p.ExitCode'
    $lines | Set-Content -LiteralPath $parentPath -Encoding ASCII
    $proc = Start-Process -FilePath $powershell -ArgumentList @('-NoProfile', '-File', $parentPath) -PassThru -WindowStyle Hidden
    return [pscustomobject]@{ process = $proc; stayFile = $stayFile }
  }
  $firstTx = New-Tx
  $stay = Start-OwnedPrepare $firstTx $app 20 'none' -Stay
  $until = [DateTime]::UtcNow.AddSeconds(180)
  do { if ((Read-Outcome $firstTx).state -eq 'backup_verified' -and (Test-Path -LiteralPath $stay.stayFile)) { break }; Start-Sleep -Milliseconds 300 } while ([DateTime]::UtcNow -lt $until)
  $first = Read-Outcome $firstTx
  Assert ($first.state -eq 'backup_verified') ('First isolated prepare failed: ' + $first.failure)
  Assert (-not $stay.process.HasExited) 'Lease owner exited before the live-owner probe'
  $liveTx = New-Tx
  $live = Start-Prepare $liveTx $app 12 'none'
  Wait-Prepare $live 60
  $liveOutcome = Read-Outcome $liveTx
  Assert ($live.ExitCode -ne 0 -and $liveOutcome.reason -eq 'upgrade-owner-live') ('Live owner was not refused: ' + $liveOutcome.reason + ' ' + $liveOutcome.failure)
  Assert ($liveOutcome.failure.Length -lt 240 -and $liveOutcome.failure -notmatch 'Exception') 'Live-owner refusal exposed a stack'
  $result.liveOwnerRefused = $true
  Stop-Process -Id $stay.process.Id -Force
  $stay = $null
  $lockPath = Join-Path $firstTx 'phase.lock'
  Assert ([IO.File]::Exists($lockPath)) 'Completed prepare removed phase.lock'
  $lockerScript = Join-Path $evidence 'hold-lock.ps1'
  "param([string]`$Path); `$fs=[IO.File]::Open(`$Path,'Open','ReadWrite','None'); try { Wait-Event -Timeout 180 } finally { `$fs.Dispose() }" | Set-Content -LiteralPath $lockerScript -Encoding ASCII
  $locker = Start-Process -FilePath $powershell -ArgumentList @('-NoProfile', '-File', $lockerScript, $lockPath) -PassThru -WindowStyle Hidden
  $until = [DateTime]::UtcNow.AddSeconds(15)
  $busy = $false
  do {
    try { $probe = [IO.File]::Open($lockPath, 'Open', 'ReadWrite', 'None'); $probe.Dispose() } catch { $busy = $true }
    if (-not $busy) { Start-Sleep -Milliseconds 100 }
  } while (-not $busy -and [DateTime]::UtcNow -lt $until)
  Assert ($busy -and -not $locker.HasExited) 'Could not hold the real phase.lock'
  $heldTx = New-Tx
  $held = Start-Prepare $heldTx $app 12 'none'
  Wait-Prepare $held 60
  $heldOutcome = Read-Outcome $heldTx
  Assert ($held.ExitCode -ne 0 -and $heldOutcome.reason -eq 'upgrade-lock-held') ('Dead owner with a busy lock was not refused: ' + $heldOutcome.reason + ' ' + $heldOutcome.failure)
  Assert ($heldOutcome.failure.Length -lt 240 -and $heldOutcome.failure -notmatch 'Exception') 'Lock refusal exposed a stack'
  Assert ([IO.File]::Exists($lockPath) -and -not $locker.HasExited) 'Lock refusal deleted the lock or stopped the holder'
  $result.lockHeldRefused = $true
  $result.lockFileRemained = $true
  $foreignTx = New-Tx
  $foreign = Start-Prepare $foreignTx $other 12 'none'
  Wait-Prepare $foreign 60
  $foreignOutcome = Read-Outcome $foreignTx
  Assert ($foreign.ExitCode -ne 0 -and $foreignOutcome.reason -eq 'upgrade-foreign') ('Foreign install root was not refused: ' + $foreignOutcome.reason)
  Assert ([IO.File]::Exists($lockPath)) 'Foreign refusal deleted the lock'
  $result.foreignRefused = $true
  Stop-Process -Id $locker.Id -Force
  $locker = $null
  Assert ([IO.File]::Exists($lockPath)) 'Releasing the holder deleted phase.lock'
  $resetTx = New-Tx
  $reset = Start-OwnedPrepare $resetTx $app 20 'backup'
  Assert ($reset.process.WaitForExit(180000)) 'Terminal reset prepare hung'
  $reset.process.Refresh()
  $resetOutcome = Read-Outcome $resetTx
  Assert ($reset.process.ExitCode -ne 0 -and $resetOutcome.state -eq 'rolled_back') ('Could not leave a terminal lease: ' + $resetOutcome.failure)
  Assert ((Read-Outcome $firstTx).state -eq 'rolled_back') 'Interrupted transaction was not rolled back'
  $result.recoveryContinued = $true
  $gui = Start-Process -FilePath (Join-Path $app 'FindX.exe') -PassThru
  $null = $gui.Handle
  $until = [DateTime]::UtcNow.AddSeconds(25)
  do {
    $gui.Refresh()
    if ($gui.HasExited -or $gui.MainWindowHandle -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 200
  } while ([DateTime]::UtcNow -lt $until)
  $gui.Refresh()
  $result.trayWindow = (-not $gui.HasExited -and $gui.MainWindowHandle -ne [IntPtr]::Zero)
  if (-not $result.trayWindow) {
    $limits.Add('Published 2.5.3 process did not open a FindX window on this hosted runner, so the tray menu and medium-integrity helper were not exercised.')
    if (-not $gui.HasExited) { Stop-Process -Id $gui.Id -Force; $gui = $null }
  } else {
    $suspended = ([FxHandoffProbe]::Suspend($gui.Id) -eq 0)
    $result.guiSuspended = $suspended
    if ($suspended) {
      $timeoutTx = New-Tx
      $timeoutOwned = Start-OwnedPrepare $timeoutTx $app 3 'none'
      $started = [DateTime]::UtcNow
      $timedOut = -not $timeoutOwned.process.WaitForExit(45000)
      $elapsed = ([DateTime]::UtcNow - $started).TotalSeconds
      if ($timedOut) {
        $kids = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine -match 'RequestQuit' })
        foreach ($kid in $kids) { Stop-Process -Id $kid.ProcessId -Force -ErrorAction SilentlyContinue }
        Stop-Process -Id $timeoutOwned.process.Id -Force
        throw 'Suspended-GUI quit was still running after 45s'
      }
      $timeoutOwned.process.Refresh()
      $timeoutProc = $timeoutOwned.process
      $timeoutOutcome = Read-Outcome $timeoutTx
      $gui.Refresh()
      $result.timeoutSeconds = [math]::Round($elapsed, 2)
      if (-not $gui.HasExited -and $timeoutProc.ExitCode -ne 0 -and $timeoutOutcome.reason -eq 'gui-quit-unavailable') {
        $result.timeoutSelfExit = $true
        $helpers = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine -match 'RequestQuit' })
        Assert ($helpers.Count -eq 0) 'Quit helper was still running after the bounded failure'
      } elseif ($gui.HasExited -and $gui.ExitCode -eq 0 -and $timeoutProc.ExitCode -eq 0 -and $timeoutOutcome.state -eq 'backup_verified') {
        $result.trayQuitVerified = $true
        $result.trayExitCode = 0
        $limits.Add('The published GUI completed a normal quit while the test had suspended it, so the timeout itself was not observed.')
      } else {
        $result.timeoutSelfExit = $false
        $limits.Add('Suspending the published GUI did not produce a bounded quit refusal. failure=' + $timeoutOutcome.failure)
      }
      if (-not $gui.HasExited) { Assert ([FxHandoffProbe]::Resume($gui.Id) -eq 0) 'Could not resume the published GUI' }
      $result.guiSuspended = $false
    } else {
      $limits.Add('The hosted runner refused to suspend the published GUI, so the quit timeout was not observed.')
    }
    if (-not $gui.HasExited) {
      $quitTx = New-Tx
      $quitOwned = Start-OwnedPrepare $quitTx $app 12 'none'
      $helperSid = ''
      $until = [DateTime]::UtcNow.AddSeconds(40)
      do {
        $kids = @(Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine -match 'RequestQuit' })
        foreach ($kid in $kids) {
          $sid = [FxHandoffProbe]::Integrity([int]$kid.ProcessId)
          if ($sid) { $helperSid = $sid }
        }
        if ($quitOwned.process.HasExited) { break }
        Start-Sleep -Milliseconds 50
      } while ([DateTime]::UtcNow -lt $until)
      Wait-Prepare $quitOwned.process 20
      $quitProc = $quitOwned.process
      $quitOutcome = Read-Outcome $quitTx
      $gui.Refresh()
      $result.mediumHelperSid = $helperSid
      $result.quitReason = $quitOutcome.reason
      $result.quitState = $quitOutcome.state
      if ($quitProc.ExitCode -eq 0 -and $gui.HasExited -and $gui.ExitCode -eq 0 -and $quitOutcome.state -eq 'backup_verified') {
        $result.trayQuitVerified = $true
        $result.trayExitCode = 0
        $result.mediumHelperVerified = ($helperSid -eq 'S-1-16-8192')
        if (-not $result.mediumHelperVerified) { $limits.Add('The published GUI quit through the installer script, but a medium-integrity helper token was not observed. This runner was already elevated; nobody clicked a UAC consent dialog.') }
        $gui = $null
      } else {
        $result.trayQuitVerified = $false
        $limits.Add('The published 2.5.3 window was visible, but the installer did not complete its normal tray quit on this hosted desktop. failure=' + $quitOutcome.failure)
        if (-not $gui.HasExited) { Stop-Process -Id $gui.Id -Force; $gui = $null }
      }
    }
  }
  if ($result.trayQuitVerified) {
    $sealTx = New-Tx
    $seal = Start-OwnedPrepare $sealTx $app 20 'backup'
    Assert ($seal.process.WaitForExit(180000)) 'Post-quit lease seal hung'
    $seal.process.Refresh()
    $sealOutcome = Read-Outcome $sealTx
    Assert ($sealOutcome.state -eq 'rolled_back') ('Lease was not terminal before the fixture: ' + $sealOutcome.failure)
    $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    if (-not (Test-Path -LiteralPath $iscc)) {
      choco install innosetup -y --no-progress
      Assert ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $iscc)) 'Inno Setup compiler missing'
    }
    $build = Join-Path $evidence 'fixture-build'
    $null = New-Item -ItemType Directory -Force -Path $build
    & $iscc '/DMyAppVersion=2.5.3' '/DTxTestBuild=1' ('/DMyServiceName=' + $service) ('/DTxPipeName=' + $pipe) ('/DPublishDir=' + $publish) ('/O' + $build) (Join-Path $repo 'installer\FindX.iss') *> (Join-Path $evidence 'iscc.log')
    Assert ($LASTEXITCODE -eq 0) 'Fixture installer compile failed'
    $fixture = Join-Path $build 'FindX-2.5.3-fixture-setup.exe'
    Assert (Test-Path -LiteralPath $fixture) 'Fixture setup was not produced'
    $result.fixtureCompiled = $true
    $gui = Start-Process -FilePath (Join-Path $app 'FindX.exe') -PassThru
    $null = $gui.Handle
    $until = [DateTime]::UtcNow.AddSeconds(25)
    do { $gui.Refresh(); if ($gui.HasExited -or $gui.MainWindowHandle -ne [IntPtr]::Zero) { break }; Start-Sleep -Milliseconds 200 } while ([DateTime]::UtcNow -lt $until)
    $fixtureTx = New-Tx
    $fixtureLog = Join-Path $evidence 'fixture-setup.log'
    $fixtureArgs = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOICONS /COMPONENTS=main /TASKS= /DIR="' + $app + '" /TXROOT="' + $fixtureTx + '" /TXDATA="' + $data + '" /TXNATIVE="' + $native + '" /TXLEGACY="' + $legacy + '" /TXGROUP="' + $group + '" /TXDESKTOP="' + $desktop + '" /TXFAULT=none /LOG="' + $fixtureLog + '"'
    $setupProc = Start-Process -FilePath $fixture -ArgumentList $fixtureArgs -PassThru
    if (-not $setupProc.WaitForExit(180000)) {
      Stop-Process -Id $setupProc.Id -Force
      $limits.Add('The TxTestBuild fixture was still running after 180s. It was stopped. The published GUI was not force-killed by the installer.')
      $result.fixtureHandoffVerified = $false
    } else {
      $setupProc.Refresh()
      $gui.Refresh()
      $fixtureOutcome = Read-Outcome $fixtureTx
      $result.fixtureExit = $setupProc.ExitCode
      $result.fixtureJournalState = $fixtureOutcome.state
      $result.fixtureReason = $fixtureOutcome.reason
      $result.fixtureGuiExit = $(if ($gui.HasExited) { $gui.ExitCode } else { $null })
      if ($setupProc.ExitCode -eq 0 -and $fixtureOutcome.state -eq 'committed' -and $gui.HasExited -and $gui.ExitCode -eq 0) {
        $result.fixtureHandoffVerified = $true
        $gui = $null
      } else {
        $result.fixtureHandoffVerified = $false
        $limits.Add('The TxTestBuild fixture did not finish the published-GUI handoff. Its Prepare budget is 3 seconds. failure=' + $fixtureOutcome.failure)
        if (-not $gui.HasExited) { Stop-Process -Id $gui.Id -Force; $gui = $null }
      }
    }
  } else {
    $limits.Add('Fixture installer was not run because the published tray quit was not verified on this runner.')
  }
  Assert (-not (Get-Service -Name 'FindX2Search' -ErrorAction SilentlyContinue)) 'Production service appeared'
  Assert (-not (Get-Service -Name $service -ErrorAction SilentlyContinue)) 'Isolated test service was registered'
  $result.ok = $true
  Save-Result
  Write-Output 'isolated-actions-handoff-ok'
} catch {
  $result.ok = $false
  $result.error = $_.Exception.Message
  Save-Result
  throw
} finally {
  if ($locker -and -not $locker.HasExited) { Stop-Process -Id $locker.Id -Force -ErrorAction SilentlyContinue }
  if ($stay -and -not $stay.process.HasExited) { Stop-Process -Id $stay.process.Id -Force -ErrorAction SilentlyContinue }
  if ($gui -and -not $gui.HasExited) {
    try { [FxHandoffProbe]::Resume($gui.Id) | Out-Null } catch {}
    Stop-Process -Id $gui.Id -Force -ErrorAction SilentlyContinue
  }
}
