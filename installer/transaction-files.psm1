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
function Get-FxTxInventory([System.Collections.IDictionary]$Roots) {
    $components = @(); $files = @(); $directories = @(); [decimal]$bytes = 0
    foreach ($name in @($Roots.Keys | Sort-Object)) {
        if ($name -notmatch '^[A-Za-z][A-Za-z0-9_-]{0,39}$') { throw 'Invalid component name' }
        $root = Get-FxTxPath $Roots[$name]
        foreach ($c in $components) {
            if ((Test-FxTxWithin $root $c.path) -or (Test-FxTxWithin $c.path $root)) { throw 'Overlapping component roots' }
        }
        if ([IO.File]::Exists($root)) { throw 'Component root must be a directory or absent' }
        $exists = [IO.Directory]::Exists($root)
        $components += [ordered]@{ name = $name; path = $root; exists = $exists }
        if (-not $exists) { continue }
        # Walk explicitly. Get-ChildItem -Recurse may follow provider-specific links.
        $queue = [Collections.Generic.Queue[string]]::new(); $queue.Enqueue($root)
        while ($queue.Count) {
            $dir = $queue.Dequeue()
            foreach ($item in @(Get-ChildItem -LiteralPath $dir -Force -ErrorAction Stop | Sort-Object FullName)) {
                if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Reparse point refused' }
                $relative = $item.FullName.Substring($root.Length + 1); Assert-FxTxRelative $relative
                if ($item.PSIsContainer) { $directories += [ordered]@{ component = $name; relative = $relative }; $queue.Enqueue($item.FullName); continue }
                if ($files.Count -ge 100000) { throw 'Inventory bound exceeded; no partial acceptance' }
                $len = $item.Length; $hash = Get-FxTxFileHash $item.FullName
                $acl = (Get-Acl -LiteralPath $item.FullName).Sddl
                $files += [ordered]@{ component = $name; relative = $relative; length = $len; sha256 = $hash; attributes = [int]$item.Attributes; lastWriteUtcTicks = $item.LastWriteTimeUtc.Ticks; sddl = $acl }
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
function New-FxTxVerifiedBackup([System.Collections.IDictionary]$Roots, [string]$BackupRoot, [decimal]$CandidateBytes = 0, [decimal]$ReserveBytes = 33554432) {
    if ($CandidateBytes -lt 0 -or $ReserveBytes -lt 0) { throw 'Negative capacity budget' }
    $backup = Get-FxTxPath $BackupRoot
    if (Test-Path -LiteralPath $backup) { throw 'Backup must be create-new; retain prior evidence' }
    $inventory = Get-FxTxInventory $Roots
    foreach ($c in $inventory.components) {
        if ((Test-FxTxWithin $backup $c.path) -or (Test-FxTxWithin $c.path $backup)) { throw 'Backup overlaps a component' }
    }
    $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($backup))
    $required = $inventory.bytes + $CandidateBytes + $ReserveBytes
    if ($required -gt $drive.AvailableFreeSpace) { throw 'Insufficient backup/staging capacity; unchanged' }
    $null = [IO.Directory]::CreateDirectory($backup)
    # The backup contains private configuration/index bytes: do not inherit broad access.
    $acl = [Security.AccessControl.DirectorySecurity]::new(); $acl.SetAccessRuleProtection($true, $false)
    $owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
    foreach ($sid in @($owner, [Security.Principal.SecurityIdentifier]::new('S-1-5-18'), [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    }
    Set-Acl -LiteralPath $backup -AclObject $acl
    Write-FxTxNewJson (Join-Path $backup 'inventory.pending.json') $inventory
    foreach ($c in $inventory.components) { if ($c.exists) { $null = [IO.Directory]::CreateDirectory((Join-Path $backup $c.name)) } }
    foreach ($dir in $inventory.directories) { $null = [IO.Directory]::CreateDirectory((Join-Path $backup ($dir.component + '\' + $dir.relative))) }
    foreach ($file in $inventory.files) {
        $source = Join-Path $Roots[$file.component] $file.relative
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
function Restore-FxTxVerifiedBackup([string]$BackupRoot, [string]$EvidenceRoot) {
    # Caller must have stopped and verified all candidate writers BEFORE entry.
    $record = Read-FxTxVerifiedBackup $BackupRoot
    $evidence = Get-FxTxPath $EvidenceRoot
    if (Test-Path -LiteralPath $evidence) { throw 'Restore evidence must be create-new' }
    foreach ($c in $record.components) {
        $target = Get-FxTxPath $c.path
        if ((Test-FxTxWithin $evidence $target) -or (Test-FxTxWithin $target $evidence)) { throw 'Restore evidence overlaps live data' }
    }
    $null = [IO.Directory]::CreateDirectory($evidence)
    foreach ($c in $record.components) {
        $target = Get-FxTxPath $c.path
        # Preserve ALL failed/new bytes, including unknown files. No recursive deletion.
        if ([IO.Directory]::Exists($target)) { [IO.Directory]::Move($target, (Join-Path $evidence $c.name)) }
        if (-not $c.exists) { continue }
        $null = [IO.Directory]::CreateDirectory($target)
        foreach ($d in $record.directories | Where-Object { $_.component -eq $c.name }) { $null=[IO.Directory]::CreateDirectory((Join-Path $target $d.relative)) }
        foreach ($f in $record.files | Where-Object { $_.component -eq $c.name }) {
            $source = Join-Path $BackupRoot ($c.name + '\' + $f.relative); $dest = Join-Path $target $f.relative
            $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($dest))
            $input = [IO.File]::Open($source,'Open','Read','Read')
            try { $output=[IO.File]::Open($dest,'CreateNew','Write','None'); try { $input.CopyTo($output); $output.Flush($true) } finally { $output.Dispose() } } finally { $input.Dispose() }
            if ((Get-FxTxFileHash $dest) -ne $f.sha256) { throw 'Restored bytes differ; retained backup/evidence, no runtime restart' }
            [IO.File]::SetLastWriteTimeUtc($dest, [datetime]::new([long]$f.lastWriteUtcTicks, [DateTimeKind]::Utc))
            $acl=[Security.AccessControl.FileSecurity]::new(); $acl.SetSecurityDescriptorSddlForm($f.sddl); Set-Acl -LiteralPath $dest -AclObject $acl
            [IO.File]::SetAttributes($dest, [IO.FileAttributes]$f.attributes)
        }
    }
    foreach ($c in $record.components) {
        if (-not $c.exists) { if (Test-Path -LiteralPath $c.path) { throw 'Absent root was fabricated by rollback' }; continue }
        foreach ($f in $record.files | Where-Object { $_.component -eq $c.name }) { if ((Get-FxTxFileHash (Join-Path $c.path $f.relative)) -ne $f.sha256) { throw 'Restore readback failed' } }
    }
    Write-FxTxNewJson (Join-Path $evidence 'restore.complete.json') ([ordered]@{schema=1;restored=$true;backup=$BackupRoot})
    return $true
}
Export-ModuleMember -Function Restore-FxTxVerifiedBackup, Read-FxTxVerifiedBackup, Get-FxTxInventory,New-FxTxVerifiedBackup,Test-FxTxBackup,Get-FxTxFileHash,Get-FxTxPath,Assert-FxTxRelative
