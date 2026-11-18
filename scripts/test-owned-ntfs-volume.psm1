# Only new, disposable CI-owned VHD files; never selects or clears a host disk.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
function New-FxOwnedTestVolume([string]$Image,[string]$Label){
 if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows'){throw 'Owned-volume fixture is Windows CI only'}
 $image=[IO.Path]::GetFullPath($Image)
 if(-not $image.StartsWith(([IO.Path]::GetFullPath($env:RUNNER_TEMP)+'\FindXUpgradeTest_'),[StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($image) -ne 'owned-health.vhd' -or $image -match '["\r\n]' -or (Test-Path -LiteralPath $image) -or $Label -notmatch '^FXTX[0-9A-F]{8}$'){throw 'Not a new owned exact fixture image'}
 $letter=@('Z','Y','X','W','V','U','T' | Where-Object {-not (Test-Path ($_+':\')) -and -not (Get-Volume -DriveLetter $_ -ErrorAction SilentlyContinue)})[0]
 if(-not $letter){throw 'No unused private test drive letter; no host drive reassignment'}
 $commands=Join-Path ([IO.Path]::GetDirectoryName($image)) 'create-owned-volume.txt'
 [IO.File]::WriteAllText($commands,('create vdisk file="'+$image+'" maximum=64 type=expandable'+"`r`nattach vdisk`r`n"),[Text.Encoding]::ASCII)
 $p=Start-Process -FilePath "$env:SystemRoot\System32\diskpart.exe" -ArgumentList ('/s "'+$commands+'"') -PassThru -NoNewWindow -RedirectStandardOutput ($commands+'.stdout.log') -RedirectStandardError ($commands+'.stderr.log')
 $processHandle=$p.Handle
 if(-not $p.WaitForExit(60000) -or $p.ExitCode -ne 0){throw 'Owned VHD creation timed out/failed; no force-stop'}
 $diskImage=Get-DiskImage -ImagePath $image -ErrorAction Stop
 $disk=$diskImage | Get-Disk -ErrorAction Stop
 if(-not $diskImage.Attached -or $disk.IsBoot -or $disk.IsSystem -or $disk.Size -ne 67108864 -or [int]$disk.BusType -ne 15 -or $disk.PartitionStyle -ne 'RAW'){throw 'Refusing anything except this new raw 64MiB file-backed disk'}
 # Number is obtained ONLY from the exact just-created image, never a host selector.
 $null=Initialize-Disk -Number $disk.Number -PartitionStyle MBR -PassThru
 $partition=New-Partition -DiskNumber $disk.Number -UseMaximumSize -DriveLetter $letter
 $null=$partition | Format-Volume -FileSystem NTFS -NewFileSystemLabel $Label -Confirm:$false
 $vol=Get-Volume -DriveLetter $letter
 if($vol.FileSystem -ne 'NTFS' -or $vol.FileSystemLabel -ne $Label){throw 'Owned volume identity failed'}
 # Only the attested tiny volume receives a journal; never C:/D: or an arbitrary input.
 $p=Start-Process -FilePath "$env:SystemRoot\System32\fsutil.exe" -ArgumentList ('usn createjournal m=16777216 a=1048576 '+$letter+':') -PassThru -NoNewWindow -RedirectStandardOutput ($commands+'.journal.log') -RedirectStandardError ($commands+'.journal.stderr.log')
 $processHandle=$p.Handle
 if(-not $p.WaitForExit(30000) -or $p.ExitCode -ne 0){throw 'Private NTFS journal creation failed; no host fallback'}
 [ordered]@{image=$image;diskNumber=$disk.Number;busType=[int]$disk.BusType;bytes=$disk.Size;letter=$letter;label=$Label;hostDiskTouched=$false} | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path ([IO.Path]::GetDirectoryName($image)) 'owned-volume.json')
 return [pscustomobject]@{Image=$image;Root=($letter+':\');Volume=($letter+':')}
}
Export-ModuleMember -Function New-FxOwnedTestVolume
