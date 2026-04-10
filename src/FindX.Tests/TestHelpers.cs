using System.Linq;
using FindX.Core.Index;
using FindX.Core.Search;

namespace FindX.Tests;

internal static class TestHelpers
{
    public static FileEntry MakeEntry(string name, bool isDir = false, long size = 0,
        long mtime = 0, uint attr = 0x20) // FILE_ATTRIBUTE_ARCHIVE
    {
        if (isDir) attr |= 0x10;
        return new FileEntry { Name = name, Attributes = attr, Size = size, LastWriteTimeTicks = mtime };
    }

    public static EvalContext MakeCtx(FileEntry entry, string fullPath)
    {
        var ctx = new EvalContext();
        int pathDepth = fullPath.Count(ch => ch is '\\' or '/');
        ctx.Reset(entry, fullPath, pathDepth);
        return ctx;
    }

    public static EvalContext MakeCtx(string name, string fullPath,
        bool isDir = false, long size = 0, long mtime = 0, uint attr = 0x20)
    {
        return MakeCtx(MakeEntry(name, isDir, size, mtime, attr), fullPath);
    }
}
