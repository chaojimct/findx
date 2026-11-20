# Read-only MFT witness probe. Opens existing files and volume handles.
# It does not create, write, delete, or reset an index. Run from an elevated
# PowerShell 5.1 prompt. UTF-8 BOM is required on ACP936.
param(
    [string]$Volume = "C",
    [uint64]$Frn = 0
)
$ErrorActionPreference = "Stop"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class FxWitness {
    public const uint SHARE = 7;
    public const uint OPEN_EXISTING = 3;
    public const uint BACKUP = 0x02000000;
    public const uint REPARSE = 0x00200000;
    public const uint READ_ATTR = 0x80;
    public const uint FSCTL_RECORD = 0x00090068;
    [StructLayout(LayoutKind.Sequential)]
    public struct Desc {
        public uint Size;
        public int Type;
        public long FileId;
        public long Pad;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct Info {
        public uint Attr;
        public System.Runtime.InteropServices.ComTypes.FILETIME Create;
        public System.Runtime.InteropServices.ComTypes.FILETIME Access;
        public System.Runtime.InteropServices.ComTypes.FILETIME Write;
        public uint Serial;
        public uint SizeHigh;
        public uint SizeLow;
        public uint Links;
        public uint IndexHigh;
        public uint IndexLow;
    }
    [DllImport("kernel32", SetLastError=true, CharSet=CharSet.Unicode)]
    public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sec, uint disp, uint flags, IntPtr tmpl);
    [DllImport("kernel32", SetLastError=true)]
    public static extern IntPtr OpenFileById(IntPtr hint, ref Desc id, uint access, uint share, IntPtr sec, uint flags);
    [DllImport("kernel32", SetLastError=true)]
    public static extern bool GetFileInformationByHandle(IntPtr h, out Info info);
    [DllImport("kernel32", SetLastError=true)]
    public static extern bool DeviceIoControl(IntPtr h, uint code, byte[] input, uint inLen, byte[] output, uint outLen, out uint returned, IntPtr ov);
    [DllImport("kernel32")]
    public static extern bool CloseHandle(IntPtr h);
    [DllImport("ntdll")]
    public static extern int RtlGetLastNtStatus();
    public static bool Invalid(IntPtr h) { return h == IntPtr.Zero || h == new IntPtr(-1); }
}
"@
function Open-ReadOnly([string]$Path, [uint32]$Access) {
    $h = [FxWitness]::CreateFileW($Path, $Access, [FxWitness]::SHARE, [IntPtr]::Zero, [FxWitness]::OPEN_EXISTING, [FxWitness]::BACKUP, [IntPtr]::Zero)
    if ([FxWitness]::Invalid($h)) { return $null }
    return $h
}
function Try-OpenById([IntPtr]$Hint, [uint64]$Id) {
    $desc = New-Object FxWitness+Desc
    $desc.Size = 24
    $desc.Type = 0
    $desc.FileId = [int64]$Id
    $opened = [FxWitness]::OpenFileById($Hint, [ref]$desc, [FxWitness]::READ_ATTR, [FxWitness]::SHARE, [IntPtr]::Zero, ([FxWitness]::BACKUP -bor [FxWitness]::REPARSE))
    $win32 = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    $nt = [FxWitness]::RtlGetLastNtStatus()
    $ok = -not [FxWitness]::Invalid($opened)
    if ($ok) { [void][FxWitness]::CloseHandle($opened); $win32 = 0 }
    [pscustomobject]@{ ok = $ok; win32 = $win32; ntstatus = ("0x{0:x8}" -f ($nt -band 0xffffffff)) }
}
$letter = $Volume.TrimEnd('\').TrimEnd(':').ToUpperInvariant()
$knownPath = Join-Path $env:SystemRoot "System32\ntdll.dll"
$known = Open-ReadOnly $knownPath ([FxWitness]::READ_ATTR)
if ($null -eq $known) { throw "cannot read $knownPath" }
$info = New-Object FxWitness+Info
if (-not [FxWitness]::GetFileInformationByHandle($known, [ref]$info)) { throw "file id unavailable" }
[void][FxWitness]::CloseHandle($known)
$knownFrn = ([uint64]$info.IndexHigh -shl 32) -bor [uint64]$info.IndexLow
$device = "\\.\$letter`:"
$root = "$letter`:\"
$hints = @(
    @{ name = "dasd-generic-read"; path = $device; access = 0x80000000 },
    @{ name = "dasd-access-0"; path = $device; access = 0 },
    @{ name = "dasd-read-attributes"; path = $device; access = [FxWitness]::READ_ATTR },
    @{ name = "root-read-attributes"; path = $root; access = [FxWitness]::READ_ATTR }
)
$rows = @()
foreach ($hint in $hints) {
    $handle = Open-ReadOnly $hint.path ([uint32]$hint.access)
    if ($null -eq $handle) {
        $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        $rows += [pscustomobject]@{ hint = $hint.name; openedHint = $false; hintWin32 = $err; known = $null; target = $null }
        continue
    }
    $knownTry = Try-OpenById $handle $knownFrn
    $targetTry = $null
    if ($Frn -ne 0) { $targetTry = Try-OpenById $handle $Frn }
    [void][FxWitness]::CloseHandle($handle)
    $rows += [pscustomobject]@{ hint = $hint.name; openedHint = $true; hintWin32 = 0; known = $knownTry; target = $targetTry }
}
$record = $null
$data = Open-ReadOnly $device 0x80000000
if ($null -ne $data -and $Frn -ne 0) {
    $ordinal = $Frn -band 0xffffffffffff
    $input = [BitConverter]::GetBytes([int64]$ordinal)
    $output = New-Object byte[] 4108
    $got = 0
    $ok = [FxWitness]::DeviceIoControl($data, [FxWitness]::FSCTL_RECORD, $input, 8, $output, $output.Length, [ref]$got, [IntPtr]::Zero)
    if ($ok -and $got -ge 60) {
        $sig = [Text.Encoding]::ASCII.GetString($output, 12, 4)
        $seq = [BitConverter]::ToUInt16($output, 12 + 16)
        $flags = [BitConverter]::ToUInt16($output, 12 + 22)
        $base = [BitConverter]::ToUInt64($output, 12 + 32) -band 0xffffffffffff
        $slot = [BitConverter]::ToUInt32($output, 12 + 44)
        $record = [pscustomobject]@{ signature = $sig; sequence = $seq; flags = $flags; base = ("0x{0:x}" -f $base); recordNumber = $slot; bytes = $got }
    } else {
        $record = [pscustomobject]@{ fsctlWin32 = [Runtime.InteropServices.Marshal]::GetLastWin32Error() }
    }
}
if ($null -ne $data) { [void][FxWitness]::CloseHandle($data) }
$result = [pscustomobject]@{
    readOnly = $true
    wrote = $false
    volume = $letter
    knownPath = $knownPath
    knownFrn = ("0x{0:x}" -f $knownFrn)
    requestedFrn = if ($Frn -eq 0) { $null } else { ("0x{0:x}" -f $Frn) }
    matrix = $rows
    record = $record
}
$result | ConvertTo-Json -Depth 6
