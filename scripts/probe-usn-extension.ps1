# Read-only check of one stuck USN prefix. It asks the live pipe for the
# checkpoint, then reads at most 256 KiB of journal from that cursor and the
# hardlink names of one already-known base file. It does not write the journal,
# the index, or a cursor. UTF-8 BOM is required on Windows PowerShell 5.1.
param(
    [string]$Pipe = "findx2",
    [string]$Volume = "C",
    [string]$Target = "9000000085571",
    [string]$Base = "5000000085567"
)
$ErrorActionPreference = "Stop"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class FxUsn {
    public const uint GENERIC_READ = 0x80000000;
    public const uint SHARE = 7;
    public const uint OPEN_EXISTING = 3;
    public const uint BACKUP = 0x02000000;
    public const uint REPARSE = 0x00200000;
    public const uint READ_ATTR = 0x80;
    public const uint FSCTL_QUERY = 0x000900F4;
    public const uint FSCTL_READ = 0x000900BB;
    public const uint REASON_MASK = 0x0000B307;
    [StructLayout(LayoutKind.Sequential)]
    public struct Desc { public uint Size; public int Type; public long FileId; public long Pad; }
    [StructLayout(LayoutKind.Sequential)]
    public struct IoStatus { public UIntPtr Status; public UIntPtr Information; }
    [DllImport("kernel32", SetLastError=true, CharSet=CharSet.Unicode)]
    public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sec, uint disp, uint flags, IntPtr tmpl);
    [DllImport("kernel32", SetLastError=true)]
    public static extern IntPtr OpenFileById(IntPtr hint, ref Desc id, uint access, uint share, IntPtr sec, uint flags);
    [DllImport("kernel32", SetLastError=true)]
    public static extern bool DeviceIoControl(IntPtr h, uint code, byte[] input, uint inLen, byte[] output, uint outLen, out uint returned, IntPtr ov);
    [DllImport("kernel32")] public static extern bool CloseHandle(IntPtr h);
    [DllImport("ntdll")] public static extern int NtQueryInformationFile(IntPtr h, ref IoStatus ios, byte[] buf, uint len, uint cls);
    public static bool Invalid(IntPtr h) { return h == IntPtr.Zero || h == new IntPtr(-1); }
}
"@
function U64([string]$Hex) { [Convert]::ToUInt64($Hex.TrimStart("0x"), 16) }
function Open-Volume([string]$Device) {
    $h = [FxUsn]::CreateFileW($Device, [FxUsn]::GENERIC_READ, [FxUsn]::SHARE, [IntPtr]::Zero, [FxUsn]::OPEN_EXISTING, [FxUsn]::BACKUP, [IntPtr]::Zero)
    if ([FxUsn]::Invalid($h)) { return $null }
    return $h
}
$targetId = U64 $Target
$baseId = U64 $Base
$letter = $Volume.TrimEnd('\').TrimEnd(':').ToUpperInvariant()
$status = $null
$statusError = $null
try {
    $client = New-Object System.IO.Pipes.NamedPipeClientStream(".", $Pipe, [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::None)
    $client.Connect(5000)
    $payload = [Text.Encoding]::UTF8.GetBytes("{""type"":""status""}`n")
    $client.Write($payload, 0, $payload.Length)
    $client.Flush()
    $reader = New-Object System.IO.StreamReader($client, [Text.Encoding]::UTF8, $false, 1024, $true)
    $line = $reader.ReadLine()
    $client.Dispose()
    $status = $line | ConvertFrom-Json
    $liveLast = [uint64]::Parse(([regex]::Match($line, '"last_usn"\s*:\s*(\d+)')).Groups[1].Value)
    $liveJournalText = ([regex]::Match($line, '"journal_id"\s*:\s*(\d+)')).Groups[1].Value
    if ($liveJournalText.Length -eq 0) { throw "status has no journal_id" }
    $liveJournalId = [uint64]::Parse($liveJournalText)
} catch {
    $statusError = $_.Exception.Message
}
$result = [ordered]@{
    readOnly = $true
    wrote = $false
    advancedCursor = $false
    target = ("0x{0:x}" -f $targetId)
    base = ("0x{0:x}" -f $baseId)
    statusError = $statusError
    checkpoint = $null
    journal = $null
    hits = @()
    hardlinks = @()
    truncated = $false
}
if ($null -eq $status -or $status.type -ne "status_result") {
    $result.stopped = "live status unavailable"
    $result | ConvertTo-Json -Depth 6
    exit 0
}
$result.checkpoint = [ordered]@{
    source = "ipc-status"
    lastUsn = $liveLast
    journalId = ("0x{0:x}" -f $liveJournalId)
    volume = $status.volume_letter
    healthy = $status.healthy
    metadataReady = $status.metadata_ready
    watchError = $status.watch_error
}
if ($liveJournalId -eq 0 -or $liveLast -eq 0) {
    $result.stopped = "live checkpoint is empty"
    $result | ConvertTo-Json -Depth 6
    exit 0
}
$device = "\\.\$letter`:"
$volumeHandle = Open-Volume $device
if ($null -eq $volumeHandle) {
    $result.stopped = "volume handle unavailable"
    $result | ConvertTo-Json -Depth 6
    exit 0
}
$query = New-Object byte[] 64
$got = 0
$empty = New-Object byte[] 0
$ok = [FxUsn]::DeviceIoControl($volumeHandle, [FxUsn]::FSCTL_QUERY, $empty, 0, $query, $query.Length, [ref]$got, [IntPtr]::Zero)
if (-not $ok -or $got -lt 24) {
    [void][FxUsn]::CloseHandle($volumeHandle)
    $result.stopped = "journal query failed"
    $result | ConvertTo-Json -Depth 6
    exit 0
}
$liveJournal = [BitConverter]::ToUInt64($query, 0)
$firstUsn = [BitConverter]::ToInt64($query, 8)
$nextUsn = [BitConverter]::ToInt64($query, 16)
$result.journal = [ordered]@{ id = ("0x{0:x}" -f $liveJournal); firstUsn = $firstUsn; nextUsn = $nextUsn }
if ($liveJournal -ne $liveJournalId) {
    [void][FxUsn]::CloseHandle($volumeHandle)
    $result.stopped = "live journal id differs from the service checkpoint"
    $result | ConvertTo-Json -Depth 6
    exit 0
}
$start = [BitConverter]::ToInt64([BitConverter]::GetBytes($liveLast), 0)
if ($start -lt $firstUsn) {
    [void][FxUsn]::CloseHandle($volumeHandle)
    $result.stopped = "checkpoint is below the journal tail; not scanning forward"
    $result | ConvertTo-Json -Depth 6
    exit 0
}
$cursor = $start
$scanned = 0
$bytesRead = 0
$hits = New-Object System.Collections.Generic.List[object]
while ($bytesRead -lt 262144 -and $scanned -lt 2048 -and $hits.Count -lt 32) {
    $request = New-Object byte[] 40
    [Array]::Copy([BitConverter]::GetBytes([int64]$cursor), 0, $request, 0, 8)
    [Array]::Copy([BitConverter]::GetBytes([uint32][FxUsn]::REASON_MASK), 0, $request, 8, 4)
    [Array]::Copy([BitConverter]::GetBytes([uint64]$liveJournal), 0, $request, 32, 8)
    $output = New-Object byte[] 65536
    $got = 0
    $ok = [FxUsn]::DeviceIoControl($volumeHandle, [FxUsn]::FSCTL_READ, $request, 40, $output, $output.Length, [ref]$got, [IntPtr]::Zero)
    if (-not $ok -or $got -lt 8) { $result.truncated = $true; break }
    $bytesRead += $got
    $cursor = [BitConverter]::ToInt64($output, 0)
    $offset = 8
    $advanced = $false
    while ($offset + 8 -le $got -and $scanned -lt 2048) {
        $len = [BitConverter]::ToUInt32($output, $offset)
        if ($len -lt 60 -or $offset + $len -gt $got) { break }
        $major = [BitConverter]::ToUInt16($output, $offset + 4)
        $file = $null; $parent = $null; $reason = 0; $name = ""
        if ($major -eq 2 -and $len -ge 60) {
            $file = [BitConverter]::ToUInt64($output, $offset + 8)
            $parent = [BitConverter]::ToUInt64($output, $offset + 16)
            $reason = [BitConverter]::ToUInt32($output, $offset + 40)
            $nameLen = [BitConverter]::ToUInt16($output, $offset + 56)
            $nameOff = [BitConverter]::ToUInt16($output, $offset + 58)
            if ($nameOff -ge 60 -and ($nameOff + (2 * $nameLen)) -le $len) {
                $name = [Text.Encoding]::Unicode.GetString($output, $offset + $nameOff, 2 * $nameLen)
            }
        } elseif ($major -eq 3 -and $len -ge 76) {
            $file = [BitConverter]::ToUInt64($output, $offset + 8)
            $parent = [BitConverter]::ToUInt64($output, $offset + 24)
            $reason = [BitConverter]::ToUInt32($output, $offset + 56)
            $nameLen = [BitConverter]::ToUInt16($output, $offset + 72)
            $nameOff = [BitConverter]::ToUInt16($output, $offset + 74)
            if ($nameOff -ge 76 -and ($nameOff + (2 * $nameLen)) -le $len) {
                $name = [Text.Encoding]::Unicode.GetString($output, $offset + $nameOff, 2 * $nameLen)
            }
        }
        if ($null -ne $file) {
            $role = $null
            if ($file -eq $targetId) { $role = "file" }
            elseif ($parent -eq $targetId) { $role = "parent" }
            if ($null -ne $role -and $hits.Count -lt 32) {
                $hits.Add([pscustomobject]@{
                    role = $role
                    usn = [BitConverter]::ToInt64($output, $offset + $(if ($major -eq 2) { 24 } else { 40 }))
                    reason = ("0x{0:x}" -f $reason)
                    name = $name
                    file = ("0x{0:x}" -f $file)
                    parent = ("0x{0:x}" -f $parent)
                })
            }
            $advanced = $true
        }
        $scanned += 1
        $offset += $len
    }
    if (-not $advanced -or $cursor -eq $start) { break }
    $start = $cursor
}
$result.scanned = $scanned
$result.bytesRead = $bytesRead
$result.hits = $hits
$desc = New-Object FxUsn+Desc
$desc.Size = 24
$desc.Type = 0
$desc.FileId = [int64]$baseId
$opened = [FxUsn]::OpenFileById($volumeHandle, [ref]$desc, [FxUsn]::READ_ATTR, [FxUsn]::SHARE, [IntPtr]::Zero, ([FxUsn]::BACKUP -bor [FxUsn]::REPARSE))
if (-not [FxUsn]::Invalid($opened)) {
    $buf = New-Object byte[] 8192
    $ios = New-Object FxUsn+IoStatus
    $nt = [FxUsn]::NtQueryInformationFile($opened, [ref]$ios, $buf, $buf.Length, 46)
    [void][FxUsn]::CloseHandle($opened)
    if ($nt -ge 0 -and $ios.Information.ToUInt64() -ge 8) {
        $count = [BitConverter]::ToUInt32($buf, 4)
        if ($count -gt 64) { $count = 64; $result.truncated = $true }
        $at = 8
        $links = New-Object System.Collections.Generic.List[object]
        for ($i = 0; $i -lt $count; $i++) {
            if ($at + 20 -gt $buf.Length) { break }
            $next = [BitConverter]::ToUInt32($buf, $at)
            $parent = [BitConverter]::ToUInt64($buf, $at + 8)
            $units = [BitConverter]::ToUInt32($buf, $at + 16)
            $bytes = $units * 2
            if ($units -gt 255 -or $at + 20 + $bytes -gt $buf.Length) { break }
            $linkName = [Text.Encoding]::Unicode.GetString($buf, $at + 20, $bytes)
            $links.Add([pscustomobject]@{ parent = ("0x{0:x}" -f $parent); name = $linkName })
            if ($next -eq 0) { break }
            $at += $next
        }
        $result.hardlinks = $links
    } else {
        $result.hardlinkStatus = ("0x{0:x8}" -f ($nt -band 0xffffffff))
    }
} else {
    $result.hardlinkStatus = "base open failed"
}
[void][FxUsn]::CloseHandle($volumeHandle)
$result.fileNameMatchesHardlink = $false
foreach ($hit in @($result.hits)) {
    if ($hit.role -ne "file") { continue }
    foreach ($link in @($result.hardlinks)) {
        if ($hit.name.Equals([string]$link.name, [StringComparison]::OrdinalIgnoreCase)) {
            $result.fileNameMatchesHardlink = $true
        }
    }
}
$result.parentHits = @($result.hits | Where-Object { $_.role -eq "parent" }).Count
$result.fileHits = @($result.hits | Where-Object { $_.role -eq "file" }).Count
$result | ConvertTo-Json -Depth 6
