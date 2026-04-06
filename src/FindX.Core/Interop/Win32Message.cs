using System.Runtime.InteropServices;
using System.Text;

namespace FindX.Core.Interop;

public static class Win32Message
{
    private const uint FORMAT_MESSAGE_FROM_SYSTEM = 0x00001000;
    private const uint FORMAT_MESSAGE_IGNORE_INSERTS = 0x00000200;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = false)]
    private static extern uint FormatMessage(
        uint dwFlags,
        nint lpSource,
        uint dwMessageId,
        uint dwLanguageId,
        StringBuilder lpBuffer,
        int nSize,
        nint arguments);

    public static string Format(uint code)
    {
        if (code == 0) return "";
        var sb = new StringBuilder(512);
        uint n = FormatMessage(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            0, code, 0, sb, sb.Capacity, 0);
        return n > 0 ? sb.ToString().Trim() : $"({code})";
    }
}
