using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Threading;

namespace FindX.Core.Interop;

/// <summary>
/// 为打开 \\.\X: 与 USN IOCTL 尝试启用常见特权（备份/管理卷），便于非「完整管理员」会话下仍能走 FindXNative。
/// </summary>
public static class VolumeAccessHelper
{
    private const uint TokenAdjustPrivileges = 0x0020;
    private const uint TokenQuery = 0x0008;
    private const uint SePrivilegeEnabled = 0x00000002;

    [StructLayout(LayoutKind.Sequential)]
    private struct Luid
    {
        public uint Low;
        public int High;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct TokenPrivileges
    {
        public int PrivilegeCount;
        public Luid Luid;
        public uint Attributes;
    }

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool OpenProcessToken(nint processHandle, uint desiredAccess, out nint tokenHandle);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(nint h);

    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool LookupPrivilegeValue(string? lpSystemName, string lpName, out Luid lpLuid);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool AdjustTokenPrivileges(nint tokenHandle, bool disableAllPrivileges,
        ref TokenPrivileges newState, int bufferLength, nint previousState, nint returnLength);

    private static int _prepared;

    public static void PrepareOnce(Action<string>? log)
    {
        if (Interlocked.Exchange(ref _prepared, 1) != 0) return;

        using var cur = WindowsIdentity.GetCurrent();
        var isAdmin = new WindowsPrincipal(cur).IsInRole(WindowsBuiltInRole.Administrator);

        var backup = TryEnablePrivilege("SeBackupPrivilege");
        var manageVol = TryEnablePrivilege("SeManageVolumePrivilege");

        log?.Invoke(
            $"[FindXNative] 进程管理员={isAdmin}; SeBackupPrivilege={(backup ? "已启用" : "未启用/无此权限")}; " +
            $"SeManageVolumePrivilege={(manageVol ? "已启用" : "未启用/无此权限")}");
    }

    private static bool TryEnablePrivilege(string privilegeName)
    {
        nint tok = 0;
        try
        {
            if (!OpenProcessToken(Process.GetCurrentProcess().Handle, TokenAdjustPrivileges | TokenQuery, out tok))
                return false;
            if (!LookupPrivilegeValue(null, privilegeName, out var luid))
                return false;

            var tp = new TokenPrivileges
            {
                PrivilegeCount = 1,
                Luid = luid,
                Attributes = SePrivilegeEnabled,
            };
            if (!AdjustTokenPrivileges(tok, false, ref tp, 0, 0, 0))
                return false;
            return Marshal.GetLastWin32Error() == 0;
        }
        finally
        {
            if (tok != 0) CloseHandle(tok);
        }
    }
}
