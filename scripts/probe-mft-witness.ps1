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
    public const uint GENERIC_READ = 0x80000000;
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
    [DllImport("kernel32")]
    public static extern void SetLastError(uint error);
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
    [FxWitness]::SetLastError(0)
    $opened = [FxWitness]::OpenFileById($Hint, [ref]$desc, [FxWitness]::READ_ATTR, [FxWitness]::SHARE, [IntPtr]::Zero, ([FxWitness]::BACKUP -bor [FxWitness]::REPARSE))
    $ok = -not [FxWitness]::Invalid($opened)
    if ($ok) {
        [void][FxWitness]::CloseHandle($opened)
        return [pscustomobject]@{ ok = $true; win32 = 0; ntstatus = "0x00000000" }
    }
    $win32 = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    $nt = [FxWitness]::RtlGetLastNtStatus()
    [pscustomobject]@{ ok = $false; win32 = $win32; ntstatus = ("0x{0:x8}" -f ($nt -band 0xffffffff)) }
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
    @{ name = "dasd-generic-read"; path = $device; access = [FxWitness]::GENERIC_READ },
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
$data = Open-ReadOnly $device ([FxWitness]::GENERIC_READ)
$baseFollow = $null
if ($null -ne $data -and $Frn -ne 0) {
    $ordinalBytes = [BitConverter]::GetBytes([uint64]$Frn)
    $ordinalBytes[6] = 0
    $ordinalBytes[7] = 0
    $ordinal = [BitConverter]::ToUInt64($ordinalBytes, 0)
    $input = [BitConverter]::GetBytes([int64]$ordinal)
    $output = New-Object byte[] 4108
    $got = 0
    $ok = [FxWitness]::DeviceIoControl($data, [FxWitness]::FSCTL_RECORD, $input, 8, $output, $output.Length, [ref]$got, [IntPtr]::Zero)
    if ($ok -and $got -ge 60) {
        $sig = [Text.Encoding]::ASCII.GetString($output, 12, 4)
        $seq = [BitConverter]::ToUInt16($output, 12 + 16)
        $flags = [BitConverter]::ToUInt16($output, 12 + 22)
        $rawBase = New-Object byte[] 8
        [Array]::Copy($output, 12 + 32, $rawBase, 0, 8)
        $high16 = [BitConverter]::ToUInt16($rawBase, 6)
        $lowBytes = New-Object byte[] 8
        [Array]::Copy($rawBase, 0, $lowBytes, 0, 6)
        $low48 = [BitConverter]::ToUInt64($lowBytes, 0)
        $slot = [BitConverter]::ToUInt32($output, 12 + 44)
        $record = [pscustomobject]@{
            signature = $sig
            sequence = $seq
            flags = $flags
            rawBase = ([BitConverter]::ToString($rawBase))
            referenceLow48 = ("0x{0:x}" -f $low48)
            referenceHigh16 = $high16
            recordNumber = $slot
            bytes = $got
        }
        if ($high16 -ne 0 -and $low48 -ne 0 -and $low48 -ne [uint64]$slot) {
            $baseInput = [BitConverter]::GetBytes([int64]$low48)
            $baseOutput = New-Object byte[] 4108
            $baseGot = 0
            $baseOk = [FxWitness]::DeviceIoControl($data, [FxWitness]::FSCTL_RECORD, $baseInput, 8, $baseOutput, $baseOutput.Length, [ref]$baseGot, [IntPtr]::Zero)
            $fullBytes = [BitConverter]::GetBytes([uint64]$low48)
            $hiBytes = [BitConverter]::GetBytes([uint16]$high16)
            $fullBytes[6] = $hiBytes[0]
            $fullBytes[7] = $hiBytes[1]
            $baseOpen = Try-OpenById $data ([BitConverter]::ToUInt64($fullBytes, 0))
            if ($baseOk -and $baseGot -ge 60) {
                $baseRaw = New-Object byte[] 8
                [Array]::Copy($baseOutput, 12 + 32, $baseRaw, 0, 8)
                $baseFollow = [pscustomobject]@{
                    readOnly = $true
                    signature = [Text.Encoding]::ASCII.GetString($baseOutput, 12, 4)
                    sequence = [BitConverter]::ToUInt16($baseOutput, 12 + 16)
                    flags = [BitConverter]::ToUInt16($baseOutput, 12 + 22)
                    rawBase = ([BitConverter]::ToString($baseRaw))
                    recordNumber = [BitConverter]::ToUInt32($baseOutput, 12 + 44)
                    open = $baseOpen
                }
            } else {
                $baseFollow = [pscustomobject]@{ readOnly = $true; fsctlWin32 = [Runtime.InteropServices.Marshal]::GetLastWin32Error(); open = $baseOpen }
            }
        }
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
    baseFollow = $baseFollow
}
$result | ConvertTo-Json -Depth 6
