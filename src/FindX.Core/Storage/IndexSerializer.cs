using FindX.Core.Index;

namespace FindX.Core.Storage;

/// <summary>
/// 索引二进制序列化/反序列化。
/// 格式：Header + Entry[] + VolumeUsn[]
/// 支持增量 NextUsn 记录点，启动时可跳过全量扫描。
/// </summary>
public static class IndexSerializer
{
    private static readonly byte[] Magic = "FINDX01\0"u8.ToArray();

    public sealed class IndexSnapshot
    {
        public List<FileEntry> Entries = new();
        public Dictionary<char, ulong> VolumeUsns = new();
    }

    public static void Save(string path, FileIndex index, Dictionary<char, ulong> volumeUsns)
    {
        var dir = Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

        using var fs = new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.None);
        using var bw = new BinaryWriter(fs);

        bw.Write(Magic);
        index.WritePersistedEntries(bw, volumeUsns);
    }

    public static IndexSnapshot? Load(string path)
    {
        if (!File.Exists(path)) return null;

        try
        {
            using var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
            using var br = new BinaryReader(fs);

            var magic = br.ReadBytes(8);
            if (!magic.SequenceEqual(Magic)) return null;

            int entryCount = br.ReadInt32();
            int usnCount = br.ReadInt32();

            var snapshot = new IndexSnapshot();
            snapshot.Entries.Capacity = entryCount;

            for (int i = 0; i < entryCount; i++)
            {
                var entry = new FileEntry
                {
                    FileRef = br.ReadUInt64(),
                    ParentRef = br.ReadUInt64(),
                    Name = br.ReadString(),
                    Attributes = br.ReadUInt32(),
                    Size = br.ReadInt64(),
                    LastWriteTimeTicks = br.ReadInt64(),
                    VolumeLetter = br.ReadChar(),
                };
                snapshot.Entries.Add(entry);
            }

            for (int i = 0; i < usnCount; i++)
            {
                var vol = br.ReadChar();
                var usn = br.ReadUInt64();
                snapshot.VolumeUsns[vol] = usn;
            }

            return snapshot;
        }
        catch
        {
            return null;
        }
    }
}
