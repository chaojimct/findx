using System.Runtime.InteropServices;

namespace FindX.Core.Interop;

[UnmanagedFunctionPointer(CallingConvention.StdCall)]
public delegate void FindXEnumCallback(
    ulong fileRef,
    ulong parentRef,
    IntPtr fileName,
    int fileNameLen,
    uint attributes,
    ulong fileSize,
    long lastWriteTime);

[UnmanagedFunctionPointer(CallingConvention.StdCall)]
public delegate void FindXJournalCallback(
    uint reason,
    ulong fileRef,
    ulong parentRef,
    IntPtr fileName,
    int fileNameLen,
    uint attributes);

public static class NativeInterop
{
    private const string DllName = "FindXNative";

    /// <param name="driveLetter">盘符 UTF-16，例如 <c>(ushort)'C'</c>。</param>
    [DllImport(DllName, CallingConvention = CallingConvention.StdCall, SetLastError = true)]
    public static extern int FindX_EnumVolume(
        ushort driveLetter,
        FindXEnumCallback callback,
        out ulong outNextUsn);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall, SetLastError = true)]
    public static extern int FindX_ReadJournal(
        ushort driveLetter,
        ulong startUsn,
        FindXJournalCallback callback,
        out ulong outNextUsn);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall, SetLastError = true)]
    public static extern int FindX_QueryJournal(
        ushort driveLetter,
        out ulong outJournalId,
        out ulong outNextUsn,
        out ulong outLowestUsn);

    /// <summary>返回 0 正常；-1 打开卷失败（openErr）；-2 USN 查询失败（journalErr）。</summary>
    [DllImport(DllName, CallingConvention = CallingConvention.StdCall, SetLastError = false)]
    public static extern int FindX_DiagnoseVolume(
        ushort driveLetter,
        out uint openErr,
        out uint journalErr);

    public static unsafe string PtrToString(IntPtr ptr, int len)
    {
        if (ptr == IntPtr.Zero || len <= 0) return string.Empty;
        return new string((char*)ptr, 0, len);
    }

    public const uint USN_REASON_FILE_CREATE = 0x00000100;
    public const uint USN_REASON_FILE_DELETE = 0x00000200;
    public const uint USN_REASON_RENAME_NEW_NAME = 0x00002000;
    public const uint USN_REASON_RENAME_OLD_NAME = 0x00001000;
    public const uint USN_REASON_CLOSE = 0x80000000;
}
