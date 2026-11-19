# File-only primitives for the native upgrade executor. No process, SCM, registry,
# install, engine, or shell execution here. Caller MUST establish writer quiescence.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-FxTxPath([string]$Path) {
    if (-not [IO.Path]::IsPathRooted($Path) -or $Path.StartsWith('\\')) { throw 'Local absolute path required' }
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if ($full -eq [IO.Path]::GetPathRoot($full).TrimEnd('\')) { throw 'Volume root is not an upgrade component' }
    $check = $full
    while ($check) {
        if ([IO.File]::Exists($check) -or [IO.Directory]::Exists($check)) {
            if (((Get-Item -LiteralPath $check -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Reparse point refused' }
        }
        $parent = [IO.Path]::GetDirectoryName($check)
        if ($parent -eq $check) { break }; $check = $parent
    }
    return $full
}
function Test-FxTxWithin([string]$Path, [string]$Parent) {
    return $Path.Equals($Parent, [StringComparison]::OrdinalIgnoreCase) -or $Path.StartsWith($Parent + '\', [StringComparison]::OrdinalIgnoreCase)
}
function Assert-FxTxRelative([string]$Relative) {
    if ([string]::IsNullOrEmpty($Relative) -or [IO.Path]::IsPathRooted($Relative) -or $Relative.Contains(':') -or @(($Relative -split '[\\/]') | Where-Object { $_ -eq '..' -or $_ -eq '.' -or $_ -eq '' }).Count) { throw 'Invalid component relative path' }
}
function Get-FxTxFileHash([string]$Path) {
    # Deny concurrent writers/deleters while reading; do not tolerate a locked source.
    $f = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try { $sha = [Security.Cryptography.SHA256]::Create(); try { return [BitConverter]::ToString($sha.ComputeHash($f)).Replace('-','').ToLowerInvariant() } finally { $sha.Dispose() } } finally { $f.Dispose() }
}
if(-not ('FxTxNativeStreams' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class FxTxNativeStreams {
 [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
 struct StreamData { public long Size; [MarshalAs(UnmanagedType.ByValTStr, SizeConst=296)] public string Name; }
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr FindFirstStreamW(string path,int level,out StreamData data,int flags);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool FindNextStreamW(IntPtr handle,out StreamData data);
 [DllImport("kernel32.dll")] static extern bool FindClose(IntPtr handle);
 [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool SetFileSecurityW(string path,uint information,byte[] descriptor);
 public static void RestoreLegacyDacl(string path,string sddl) {
  var sd=new System.Security.AccessControl.RawSecurityDescriptor(sddl);
  // Set-Acl/SetNamedSecurityInfo converts legacy ACLs to the modern automatic
  // inheritance model. Preserve the original model instead of ignoring AI in
  // readback. DACL-only SetFileSecurity deliberately does not propagate to children.
  if((sd.ControlFlags & System.Security.AccessControl.ControlFlags.DiscretionaryAclAutoInherited)!=0)return;
  if((sd.ControlFlags & System.Security.AccessControl.ControlFlags.DiscretionaryAclAutoInheritRequired)!=0)throw new InvalidOperationException("Unstable requested inheritance transition; retained backup");
  byte[] raw=new byte[sd.BinaryLength];sd.GetBinaryForm(raw,0);
  if(!SetFileSecurityW(path,4,raw))throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(),"Cannot restore exact legacy DACL model");
 }
 public static void RequireUnnamedOnly(string path) {
  StreamData data; IntPtr h=FindFirstStreamW(path,0,out data,0);
  if(h==new IntPtr(-1)){int e=Marshal.GetLastWin32Error();if(e==38)return;throw new System.ComponentModel.Win32Exception(e,"Cannot verify NTFS streams; refusing incomplete backup");}
  try { int count=0; do { if(++count>1024 || data.Name!="::$DATA") throw new InvalidOperationException("Named NTFS data stream; refusing non-preserving backup"); } while(FindNextStreamW(h,out data));
   int e=Marshal.GetLastWin32Error();if(e!=38)throw new System.ComponentModel.Win32Exception(e);
  } finally {FindClose(h);}
 }
}
'@
}
$script:RequireAuditSecurity=$false
function Enable-FxTxFullSecurity { $script:RequireAuditSecurity=$true }
function Get-FxTxMetadata([string]$Path) {
    $item=Get-Item -LiteralPath $Path -Force
    # These cannot be reproduced by an ordinary byte-copy transaction. Refuse,
    # never silently strip encryption, sparse/compression semantics or named data.
    if(($item.Attributes -band ([IO.FileAttributes]::Encrypted -bor [IO.FileAttributes]::Compressed -bor [IO.FileAttributes]::SparseFile -bor [IO.FileAttributes]::Offline)) -ne 0){throw ('Unsupported encrypted/compressed/sparse/offline object: '+$Path+'; attributes='+$item.Attributes+' (0x'+([int]$item.Attributes).ToString('X')+'); unchanged')}
    [FxTxNativeStreams]::RequireUnnamedOnly($Path)
    $security=if($script:RequireAuditSecurity){Get-Acl -LiteralPath $Path -Audit}else{Get-Acl -LiteralPath $Path}
    return [ordered]@{sddl=$security.Sddl;attributes=[int]$item.Attributes;lastWriteUtcTicks=$item.LastWriteTimeUtc.Ticks;creationUtcTicks=$item.CreationTimeUtc.Ticks}
}
function Set-FxTxMetadata([string]$Path,$Metadata,[bool]$Directory) {
    if($Directory){$acl=[Security.AccessControl.DirectorySecurity]::new()}else{$acl=[Security.AccessControl.FileSecurity]::new()}
    $acl.SetSecurityDescriptorSddlForm($Metadata.sddl);Set-Acl -LiteralPath $Path -AclObject $acl
    [IO.File]::SetAttributes($Path,[IO.FileAttributes]$Metadata.attributes)
    if($Directory){[IO.Directory]::SetCreationTimeUtc($Path,[datetime]::new([long]$Metadata.creationUtcTicks,[DateTimeKind]::Utc));[IO.Directory]::SetLastWriteTimeUtc($Path,[datetime]::new([long]$Metadata.lastWriteUtcTicks,[DateTimeKind]::Utc))}
    else{[IO.File]::SetCreationTimeUtc($Path,[datetime]::new([long]$Metadata.creationUtcTicks,[DateTimeKind]::Utc));[IO.File]::SetLastWriteTimeUtc($Path,[datetime]::new([long]$Metadata.lastWriteUtcTicks,[DateTimeKind]::Utc))}
}
# Only these individual files are owned settings. Native production intentionally
# shares FindX-native-dev with development indexes; never quarantine that directory.
# prefs.rs/legacy_compat.rs read legacy sources but only write native prefs/settings.
function Get-FxTxOwnedConfigLeaf([string]$Name) {
    switch ($Name) {
        nativePreferences { return 'prefs.json' }
        nativeSettings { return 'settings.json' }
        legacySettings { return 'findx2-gui-settings.json' }
        default { return '' }
    }
}
function Get-FxTxComponentKind($Component) {
    if($Component -is [Collections.IDictionary]) { if($Component.Contains('kind')) { return [string]$Component['kind'] } }
    elseif($Component.PSObject.Properties['kind']) { return [string]$Component.kind }
    return 'directory' # Existing schema1 directory backups remain restorable.
}
function Get-FxTxUpgradeBackupRoots($Roots) {
    return [ordered]@{
        app=[string]$Roots.app; data=[string]$Roots.data
        nativePreferences=(Join-Path $Roots.nativeConfig 'prefs.json')
        nativeSettings=(Join-Path $Roots.nativeConfig 'settings.json')
        legacySettings=(Join-Path $Roots.legacyConfig 'findx2-gui-settings.json')
    }
}
function Get-FxTxInventory([System.Collections.IDictionary]$Roots) {
    $components = @(); $files = @(); $directories = @(); [decimal]$bytes = 0
    foreach ($name in @($Roots.Keys | Sort-Object)) {
        if ($name -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,39}$') { throw 'Invalid component name' }
        $root = Get-FxTxPath $Roots[$name]
        foreach ($c in $components) {
            if ((Test-FxTxWithin $root $c.path) -or (Test-FxTxWithin $c.path $root)) { throw 'Overlapping component roots' }
        }
        $ownedLeaf=Get-FxTxOwnedConfigLeaf $name
        if($ownedLeaf) {
            if([IO.Path]::GetFileName($root) -cne $ownedLeaf -or [IO.Directory]::Exists($root)) { throw 'Owned configuration must be its exact file or absent' }
            $exists=[IO.File]::Exists($root)
            $metadata=if($exists){Get-FxTxMetadata $root}else{$null}
            $components += [ordered]@{name=$name;path=$root;exists=$exists;metadata=$metadata;kind='file'}
            if($exists) {
                $item=Get-Item -LiteralPath $root -Force
                $files += [ordered]@{component=$name;relative='content';length=$item.Length;sha256=(Get-FxTxFileHash $root);attributes=$metadata.attributes;lastWriteUtcTicks=$metadata.lastWriteUtcTicks;sddl=$metadata.sddl;creationUtcTicks=$metadata.creationUtcTicks}
                $bytes += $item.Length
            }
            continue
        }
        if ([IO.File]::Exists($root)) { throw 'Component root must be a directory or absent' }
        $exists = [IO.Directory]::Exists($root)
        $components += [ordered]@{ name = $name; path = $root; exists = $exists; metadata = $(if($exists){Get-FxTxMetadata $root}else{$null}) }
        if (-not $exists) { continue }
        # Walk explicitly. Get-ChildItem -Recurse may follow provider-specific links.
        $queue = [Collections.Generic.Queue[string]]::new(); $queue.Enqueue($root)
        while ($queue.Count) {
            $dir = $queue.Dequeue()
            foreach ($item in @(Get-ChildItem -LiteralPath $dir -Force -ErrorAction Stop | Sort-Object FullName)) {
                if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Reparse point refused' }
                $relative = $item.FullName.Substring($root.Length + 1); Assert-FxTxRelative $relative
                if($files.Count+$directories.Count -ge 100000){throw 'Inventory object bound exceeded'}
                if ($item.PSIsContainer) { $directories += [ordered]@{ component = $name; relative = $relative; metadata=(Get-FxTxMetadata $item.FullName) }; $queue.Enqueue($item.FullName); continue }
                if ($files.Count -ge 100000) { throw 'Inventory bound exceeded; no partial acceptance' }
                $metadata=Get-FxTxMetadata $item.FullName
                $len = $item.Length; $hash = Get-FxTxFileHash $item.FullName
                $files += [ordered]@{ component = $name; relative = $relative; length = $len; sha256 = $hash; attributes = [int]$item.Attributes; lastWriteUtcTicks = $item.LastWriteTimeUtc.Ticks; sddl = $metadata.sddl; creationUtcTicks=$metadata.creationUtcTicks }
                $bytes += $len
            }
        }
    }
    return [ordered]@{ schema = 1; components = @($components); directories = @($directories); files = @($files); bytes = $bytes }
}
function Write-FxTxNewJson([string]$Path, $Value) {
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($Value | ConvertTo-Json -Depth 12))
    $f = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $f.Write($bytes, 0, $bytes.Length); $f.Flush($true) } finally { $f.Dispose() }
}
function Test-FxTxBackup([string]$BackupRoot, $Manifest) {
    $backup = Get-FxTxPath $BackupRoot
    if ($Manifest.schema -ne 1) { throw 'Unsupported backup schema' }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $names = @{}
    foreach ($component in $Manifest.components) {
        if ($component.name -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,39}$' -or $names.ContainsKey($component.name)) { throw 'Invalid or duplicate backup component' }
        $kind=Get-FxTxComponentKind $component
        if($kind -notin @('file','directory')) { throw 'Unknown backup component kind' }
        if($kind -eq 'file') {
            $leaf=Get-FxTxOwnedConfigLeaf $component.name
            if(-not $leaf -or [IO.Path]::GetFileName($component.path) -cne $leaf) { throw 'Invalid owned configuration backup path' }
            $owned=@($Manifest.files | Where-Object {$_.component -eq $component.name})
            if($owned.Count -ne [int][bool]$component.exists -or @($owned | Where-Object {$_.relative -cne 'content'}).Count -or @($Manifest.directories | Where-Object {$_.component -eq $component.name}).Count) { throw 'Invalid owned configuration backup shape' }
        }
        $names[$component.name] = $true
    }
    foreach ($component in $Manifest.components) {
        if ($component.exists -and -not [IO.Directory]::Exists((Get-FxTxPath (Join-Path $backup $component.name)))) { throw 'Missing component backup directory' }
    }
    foreach ($dir in $Manifest.directories) {
        Assert-FxTxRelative $dir.relative
        if (-not $names.ContainsKey($dir.component)) { throw 'Unknown directory component' }
        if (-not [IO.Directory]::Exists((Get-FxTxPath (Join-Path $backup ($dir.component + '\' + $dir.relative))))) { throw 'Missing backup directory' }
    }
    foreach ($file in $Manifest.files) {
        Assert-FxTxRelative $file.relative
        if (-not $names.ContainsKey($file.component)) { throw 'Unknown backup component' }
        $relative = $file.component + '\' + $file.relative
        if (-not $seen.Add($relative)) { throw 'Duplicate backup file' }
        $path = Get-FxTxPath (Join-Path $backup $relative)
        if (-not (Test-FxTxWithin $path $backup)) { throw 'Backup path escaped' }
        if (-not [IO.File]::Exists($path) -or (Get-Item -LiteralPath $path).Length -ne $file.length -or (Get-FxTxFileHash $path) -ne $file.sha256) { throw ('Backup integrity failed: ' + $relative) }
    }
    return $true
}
function New-FxTxVerifiedBackup([System.Collections.IDictionary]$Roots, [string]$BackupRoot, [decimal]$CandidateBytes = 0, [decimal]$ReserveBytes = 33554432, [switch]$AdministratorsOnly) {
    if ($CandidateBytes -lt 0 -or $ReserveBytes -lt 0) { throw 'Negative capacity budget' }
    $backup = Get-FxTxPath $BackupRoot
    if (Test-Path -LiteralPath $backup) { throw 'Backup must be create-new; retain prior evidence' }
    $inventory = Get-FxTxInventory $Roots
    foreach ($c in $inventory.components) {
        if ((Test-FxTxWithin $backup $c.path) -or (Test-FxTxWithin $c.path $backup)) { throw 'Backup overlaps a component' }
    }
    if([Text.Encoding]::UTF8.GetByteCount(($inventory | ConvertTo-Json -Depth 15 -Compress)) -gt 67108864){throw 'Backup metadata bound exceeded'}
    $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($backup))
    $required = $inventory.bytes + $CandidateBytes + $ReserveBytes
    if ($required -gt $drive.AvailableFreeSpace) { throw 'Insufficient backup/staging capacity; unchanged' }
    $null = [IO.Directory]::CreateDirectory($backup)
    # The backup contains private configuration/index bytes: do not inherit broad access.
    $acl = [Security.AccessControl.DirectorySecurity]::new(); $acl.SetAccessRuleProtection($true, $false)
    $owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $principals=@([Security.Principal.SecurityIdentifier]::new('S-1-5-18'),[Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))
    if($AdministratorsOnly){$acl.SetOwner([Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))}else{$principals+=@($owner)}
    foreach ($sid in $principals) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    }
    Set-Acl -LiteralPath $backup -AclObject $acl
    Write-FxTxNewJson (Join-Path $backup 'inventory.pending.json') $inventory
    foreach ($c in $inventory.components) { if ($c.exists) { $null = [IO.Directory]::CreateDirectory((Join-Path $backup $c.name)) } }
    foreach ($dir in $inventory.directories) { $null = [IO.Directory]::CreateDirectory((Join-Path $backup ($dir.component + '\' + $dir.relative))) }
    foreach ($file in $inventory.files) {
        $source = $(if(Get-FxTxOwnedConfigLeaf $file.component){[string]$Roots[$file.component]}else{Join-Path $Roots[$file.component] $file.relative})
        $dest = Join-Path $backup ($file.component + '\' + $file.relative)
        $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($dest))
        $input = [IO.File]::Open($source, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            $output = [IO.File]::Open($dest, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $input.CopyTo($output); $output.Flush($true) } finally { $output.Dispose() }
        } finally { $input.Dispose() }
        if ((Get-FxTxFileHash $dest) -ne $file.sha256) { throw 'Copy changed or failed integrity; incomplete backup retained' }
    }
    # Detect a source change between enumeration and copy; never bless a mixed snapshot.
    $after = Get-FxTxInventory $Roots
    if (($inventory | ConvertTo-Json -Depth 12 -Compress) -ne ($after | ConvertTo-Json -Depth 12 -Compress)) { throw 'Source changed while copying; incomplete backup retained' }
    $null = Test-FxTxBackup $backup $inventory
    # Commit record is create-new, flushed and re-read. A missing/partial record is NEVER success.
    Write-FxTxNewJson (Join-Path $backup 'backup.complete.json') $inventory
    $record = Get-Content -LiteralPath (Join-Path $backup 'backup.complete.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    $null = Test-FxTxBackup $backup $record
    return $inventory
}
function Read-FxTxVerifiedBackup([string]$BackupRoot) {
    $path = Join-Path (Get-FxTxPath $BackupRoot) 'backup.complete.json'
    if (-not [IO.File]::Exists($path) -or (Get-Item -LiteralPath $path).Length -gt 67108864) { throw 'No complete bounded backup record' }
    $record = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
    $null = Test-FxTxBackup $BackupRoot $record
    return $record
}
function Write-FxTxUpdateJson([string]$Path,$Value) {
 $tmp=$Path+'.'+[guid]::NewGuid().ToString('N')+'.tmp';Write-FxTxNewJson $tmp $Value
 if([IO.File]::Exists($Path)){[IO.File]::Replace($tmp,$Path,($Path+'.previous.'+[guid]::NewGuid().ToString('N')))}else{[IO.File]::Move($tmp,$Path)}
}
function Assert-FxTxRestoredComponent($Record,$Component,[string]$Path) {
 $roots=[ordered]@{};$roots[$Component.name]=$Path;$actual=Get-FxTxInventory $roots
 $actual.components[0].path=$Component.path
 $files=@($Record.files | Where-Object {$_.component -eq $Component.name});[decimal]$total=0;foreach($f in $files){$total+=$f.length}
 $expected=[ordered]@{schema=1;components=@($Component);directories=@($Record.directories | Where-Object {$_.component -eq $Component.name});files=$files;bytes=$total}
 if(($actual | ConvertTo-Json -Depth 15 -Compress) -cne ($expected | ConvertTo-Json -Depth 15 -Compress)){
  if($script:restoreDiagnosticRoot){$tag=[guid]::NewGuid().ToString('N');Write-FxTxNewJson (Join-Path $script:restoreDiagnosticRoot ('restore-diff-'+$tag+'-actual.json')) $actual;Write-FxTxNewJson (Join-Path $script:restoreDiagnosticRoot ('restore-diff-'+$tag+'-expected.json')) $expected}
  throw ('Restored complete bytes/ACL/metadata set differs: '+$Component.name)
 }
}
function Assert-FxTxRestoredContent($Record,$Component,[string]$Path) {
 $roots=[ordered]@{};$roots[$Component.name]=$Path;$actual=Get-FxTxInventory $roots
 $a=@($actual.files | ForEach-Object {[ordered]@{relative=$_.relative;length=$_.length;sha256=$_.sha256}})
 $e=@($Record.files | Where-Object {$_.component -eq $Component.name} | ForEach-Object {[ordered]@{relative=$_.relative;length=$_.length;sha256=$_.sha256}})
 if(($a | ConvertTo-Json -Depth 8 -Compress) -cne ($e | ConvertTo-Json -Depth 8 -Compress)){throw 'Recovery content set differs'}
 $a=@($actual.directories | ForEach-Object {$_.relative});$e=@($Record.directories | Where-Object {$_.component -eq $Component.name} | ForEach-Object {$_.relative})
 if(($a | ConvertTo-Json -Compress) -cne ($e | ConvertTo-Json -Compress)){throw 'Recovery directory set differs'}
}
function Set-FxTxComponentMetadata($Record,$Component,[string]$Path) {
 # Apply inherited permissions only AFTER atomic publication under the ORIGINAL
 # parent. Setting them while staged under a different ACL would re-inherit the
 # recovery store's ACL rather than the original parent's ACL.
 Set-FxTxMetadata $Path $Component.metadata $true
 foreach($d in @($Record.directories | Where-Object {$_.component -eq $Component.name} | Sort-Object {$_.relative.Length})){Set-FxTxMetadata (Join-Path $Path $d.relative) $d.metadata $true}
 foreach($f in @($Record.files | Where-Object {$_.component -eq $Component.name})){Set-FxTxMetadata (Join-Path $Path $f.relative) $f $false}
 foreach($d in @($Record.directories | Where-Object {$_.component -eq $Component.name} | Sort-Object {$_.relative.Length} -Descending)){Set-FxTxMetadata (Join-Path $Path $d.relative) $d.metadata $true}
 Set-FxTxMetadata $Path $Component.metadata $true
 foreach($f in @($Record.files | Where-Object {$_.component -eq $Component.name})){[FxTxNativeStreams]::RestoreLegacyDacl((Join-Path $Path $f.relative),$f.sddl)}
 foreach($d in @($Record.directories | Where-Object {$_.component -eq $Component.name})){[FxTxNativeStreams]::RestoreLegacyDacl((Join-Path $Path $d.relative),$d.metadata.sddl)}
 [FxTxNativeStreams]::RestoreLegacyDacl($Path,$Component.metadata.sddl)
}
function Restore-FxTxOwnedConfig($Record,$Component,[string]$BackupRoot,[string]$Evidence) {
    $target=Get-FxTxPath $Component.path
    if([IO.Directory]::Exists($target)) { throw ('Owned config target became a directory: '+$target) }
    if([IO.File]::Exists($target)) { $null=Get-FxTxMetadata $target }
    $quarantine=Join-Path $Evidence ($Component.name+'-replaced-file')
    if(-not $Component.exists) {
        # Only remove our exact newly created owned file, never its parent/siblings.
        if([IO.File]::Exists($target)) {
            if(Test-Path -LiteralPath $quarantine) { throw 'Concurrent owned config creation; retained unchanged' }
            [IO.File]::Move($target,$quarantine)
        }
        Assert-FxTxRestoredComponent $Record $Component $target
        return
    }
    if(-not [IO.Directory]::Exists([IO.Path]::GetDirectoryName($target))) { throw 'Settings parent unexpectedly missing; retained verified backup' }
    $file=@($Record.files | Where-Object {$_.component -eq $Component.name})[0]
    # A previous atomic replacement may already have published before interruption.
    if(-not [IO.File]::Exists($target) -or (Get-FxTxFileHash $target) -cne $file.sha256) {
        if(Test-Path -LiteralPath $quarantine) { throw 'Concurrent settings change after recovery; retained both versions' }
        $stage=Join-Path $Evidence ('.staging-'+$Component.name)
        if([IO.File]::Exists($stage)) {
            if((Get-FxTxFileHash $stage) -cne $file.sha256) { throw 'Partial settings staging differs; retained' }
        } else {
            $source=Join-Path $BackupRoot ($Component.name+'\content')
            $input=[IO.File]::Open($source,'Open','Read','Read')
            try {$output=[IO.File]::Open($stage,'CreateNew','Write','None');try {$input.CopyTo($output);$output.Flush($true)} finally {$output.Dispose()}} finally {$input.Dispose()}
        }
        if([IO.File]::Exists($target)) { [IO.File]::Replace($stage,$target,$quarantine) }
        else { [IO.File]::Move($stage,$target) }
    }
    Set-FxTxMetadata $target $Component.metadata $false
    [FxTxNativeStreams]::RestoreLegacyDacl($target,$Component.metadata.sddl)
    Assert-FxTxRestoredComponent $Record $Component $target
}
function Restore-FxTxVerifiedBackup([string]$BackupRoot,[string]$EvidenceRoot) {
 # A create-new, flushed per-component plan distinguishes our partial recovery from
 # unknown pre-existing data. Re-entry never re-quarantines a partly restored tree.
 $record=Read-FxTxVerifiedBackup $BackupRoot;$evidence=Get-FxTxPath $EvidenceRoot;$script:restoreDiagnosticRoot=$evidence
 foreach($c in $record.components){if((Test-FxTxWithin $evidence $c.path) -or (Test-FxTxWithin $c.path $evidence)){throw 'Restore evidence overlaps original component'}}
 $digest=Get-FxTxFileHash (Join-Path $BackupRoot 'backup.complete.json');$plan=Join-Path $evidence '.restore-plan.json'
 if(Test-Path -LiteralPath $evidence){
  if(-not [IO.File]::Exists($plan)){throw 'Unowned restore evidence; unchanged'}
  $owned=Get-Content -LiteralPath $plan -Raw -Encoding UTF8 | ConvertFrom-Json
  if($owned.schema -ne 1 -or $owned.backupHash -cne $digest -or $owned.backup -cne (Get-FxTxPath $BackupRoot)){throw 'Restore plan does not match verified backup'}
 }else{$null=[IO.Directory]::CreateDirectory($evidence);Write-FxTxNewJson $plan ([ordered]@{schema=1;backup=(Get-FxTxPath $BackupRoot);backupHash=$digest})}
 foreach($c in $record.components){
  if((Get-FxTxComponentKind $c) -eq 'file') { Restore-FxTxOwnedConfig $record $c $BackupRoot $evidence;continue }
  $target=Get-FxTxPath $c.path;$quarantine=Join-Path $evidence $c.name;$stage=Join-Path $evidence ('.staging\'+$c.name);$marker=Join-Path $evidence ('.component-'+$c.name+'.json')
  if([IO.File]::Exists($marker)){$progress=Get-Content -LiteralPath $marker -Raw -Encoding UTF8 | ConvertFrom-Json;if($progress.backupHash -cne $digest){throw 'Component plan mismatch'}}
  else{$progress=[pscustomobject]@{backupHash=$digest;phase='quarantine_intent'};Write-FxTxNewJson $marker $progress}
  if($progress.phase -eq 'quarantine_intent'){
   if([IO.Directory]::Exists($quarantine)){if(Test-Path -LiteralPath $target){throw 'Ambiguous concurrent destination; retained all data'}}
   elseif([IO.Directory]::Exists($target)){[IO.Directory]::Move($target,$quarantine)}
   elseif([IO.File]::Exists($target)){throw 'Component root changed type; unchanged'}
   $progress.phase='building';Write-FxTxUpdateJson $marker $progress
  }
  if(-not $c.exists){if(Test-Path -LiteralPath $target){throw 'Absent original root was fabricated'};$progress.phase='complete';Write-FxTxUpdateJson $marker $progress;continue}
  if($progress.phase -eq 'building'){
   if(Test-Path -LiteralPath $target){throw 'Concurrent destination while building; unchanged'}
   $null=[IO.Directory]::CreateDirectory($stage)
   foreach($d in @($record.directories | Where-Object {$_.component -eq $c.name})){$null=[IO.Directory]::CreateDirectory((Join-Path $stage $d.relative))}
   foreach($f in @($record.files | Where-Object {$_.component -eq $c.name})){
    $source=Join-Path $BackupRoot ($c.name+'\'+$f.relative);$dest=Join-Path $stage $f.relative
    if([IO.File]::Exists($dest)){if((Get-FxTxFileHash $dest) -cne $f.sha256){throw 'Partial staging bytes differ; retained for manual recovery'}}
    else{$null=[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($dest));$input=[IO.File]::Open($source,'Open','Read','Read');try{$output=[IO.File]::Open($dest,'CreateNew','Write','None');try{$input.CopyTo($output);$output.Flush($true)}finally{$output.Dispose()}}finally{$input.Dispose()}}
   }
   Assert-FxTxRestoredContent $record $c $stage
   $progress.phase='publish_intent';Write-FxTxUpdateJson $marker $progress
  }
  if($progress.phase -eq 'publish_intent'){
   if(Test-Path -LiteralPath $target){if(Test-Path -LiteralPath $stage){throw 'Concurrent restored destination; unchanged'};Assert-FxTxRestoredContent $record $c $target}
   else{[IO.Directory]::Move($stage,$target)}
   Set-FxTxComponentMetadata $record $c $target;Assert-FxTxRestoredComponent $record $c $target
   $progress.phase='complete';Write-FxTxUpdateJson $marker $progress
  }
  if($progress.phase -ne 'complete'){throw 'Unknown component recovery phase'}
  Assert-FxTxRestoredComponent $record $c $target
 }
 $complete=Join-Path $evidence 'restore.complete.json'
 if(-not [IO.File]::Exists($complete)){Write-FxTxNewJson $complete ([ordered]@{schema=1;restored=$true;backupHash=$digest})}
 return $true
}
Export-ModuleMember -Function Get-FxTxUpgradeBackupRoots,Enable-FxTxFullSecurity,Write-FxTxUpdateJson,Get-FxTxMetadata,Set-FxTxMetadata,Write-FxTxNewJson,Restore-FxTxVerifiedBackup, Read-FxTxVerifiedBackup, Get-FxTxInventory,New-FxTxVerifiedBackup,Test-FxTxBackup,Get-FxTxFileHash,Get-FxTxPath,Assert-FxTxRelative
