using FindX.Core.Index;

namespace FindX.Core.FileSystem;

internal static class FileMetadataReader
{
    public static bool NeedsHydration(FileEntry entry)
    {
        if (entry.LastWriteTimeTicks <= 0)
            return true;
        return !entry.IsDirectory && entry.Size == 0;
    }

    public static bool TryHydrate(string fullPath, FileEntry source, out FileEntry hydrated)
    {
        hydrated = source;

        try
        {
            if (source.IsDirectory)
            {
                var info = new DirectoryInfo(fullPath);
                if (!info.Exists)
                    return false;

                hydrated.Attributes = (uint)(info.Attributes & (FileAttributes)0xFFFF);
                hydrated.Size = 0;
                hydrated.LastWriteTimeTicks = info.LastWriteTimeUtc.Ticks;
                hydrated.CreationTimeTicks = info.CreationTimeUtc.Ticks;
                hydrated.AccessTimeTicks = info.LastAccessTimeUtc.Ticks;
                return true;
            }

            var file = new FileInfo(fullPath);
            if (!file.Exists)
                return false;

            hydrated.Attributes = (uint)(file.Attributes & (FileAttributes)0xFFFF);
            hydrated.Size = file.Length;
            hydrated.LastWriteTimeTicks = file.LastWriteTimeUtc.Ticks;
            hydrated.CreationTimeTicks = file.CreationTimeUtc.Ticks;
            hydrated.AccessTimeTicks = file.LastAccessTimeUtc.Ticks;
            return true;
        }
        catch
        {
            return false;
        }
    }
}
