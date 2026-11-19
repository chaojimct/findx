# Only the existing attested private-VHD primitive. No service or host index.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows'){throw 'Private VHD test is CI only'}
Import-Module (Join-Path $PSScriptRoot 'test-owned-ntfs-volume.psm1') -Force
$base=Join-Path $env:RUNNER_TEMP ('FindXUpgradeTest_identity_'+[guid]::NewGuid().ToString('N'));$null=New-Item -ItemType Directory $base
$owned=$null
try{
 $owned=New-FxOwnedTestVolume (Join-Path $base 'owned-health.vhd') ('FXTX'+[guid]::NewGuid().ToString('N').Substring(0,8).ToUpperInvariant())
 [IO.File]::WriteAllText((Join-Path $owned.Root '.findx-identity-ci-owned'),$env:GITHUB_SHA)
 $env:FINDX_IDENTITY_OWNED_ROOT=$owned.Root
 $p=Start-Process cargo -ArgumentList 'test --locked -p findx2-windows namespace_native_owned -- --ignored --nocapture --test-threads=1' -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $base 'native-identity.log') -RedirectStandardError (Join-Path $base 'native-identity.err')
 $handle=$p.Handle;$p.WaitForExit();$exit=$p.ExitCode
 $text=Get-Content (Join-Path $base 'native-identity.log') -Raw;Write-Output $text
 if($exit -ne 0 -or $text -notmatch 'NAMESPACE_NATIVE_OWNED_PASS' -or $text -notmatch '1 passed; 0 failed'){Get-Content (Join-Path $base 'native-identity.err');throw 'Actual isolated identity test failed or never entered'}
}finally{
 Remove-Item Env:FINDX_IDENTITY_OWNED_ROOT -ErrorAction SilentlyContinue
 if($owned){$image=Get-DiskImage -ImagePath $owned.Image;if($image.Attached){Dismount-DiskImage -ImagePath $owned.Image -ErrorAction Stop}}
}
