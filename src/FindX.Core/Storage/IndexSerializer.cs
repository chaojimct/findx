using FindX.Core.Index;

namespace FindX.Core.Storage;

/// <summary>
/// 索引二进制序列化/反序列化。
/// 优先使用 FXBIN06 快速格式（Rust 内存直写，含排序索引与拼音池 + trigram 快照，冷启动不重算拼音）；
/// 兼容 FXBIN05（加载后 Rust 内重建拼音与倒排）；回退 FINDX01 旧格式（逐条读取+rebuild）。
/// </summary>
public static class IndexSerializer
{
    private static readonly byte[] MagicLegacy = "FINDX01\0"u8.ToArray();
    private static readonly byte[] MagicBinary05 = "FXBIN05\0"u8.ToArray();
    private static readonly byte[] MagicBinary06 = "FXBIN06\0"u8.ToArray();

    /// <summary>快速二进制保存。</summary>
    public static void Save(string path, FileIndex index, Dictionary<char, ulong> volumeUsns)
    {
        var dir = Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);
        index.SaveBinary(path, volumeUsns);
    }

    /// <summary>
    /// 尝试加载 FXBIN03 二进制格式。成功返回 live 条目数，失败返回 -1。
    /// 无需 BeginBulk/EndBulk，含排序索引直接可用。
    /// </summary>
    public static int TryLoadBinary(string path, FileIndex index, Dictionary<char, ulong> volumeUsns)
    {
        if (!File.Exists(path)) return -1;
        try
        {
            byte[] magic;
            using (var peek = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read))
            {
                magic = new byte[8];
                if (peek.Read(magic, 0, 8) < 8) return -1;
            }
            if (!magic.AsSpan().SequenceEqual(MagicBinary05) && !magic.AsSpan().SequenceEqual(MagicBinary06))
                return -1;
            return index.LoadBinary(path, volumeUsns);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"[IndexSerializer] TryLoadBinary error: {ex}");
            return -1;
        }
    }

    /// <summary>
    /// 加载旧 FINDX01 格式（调用方须先 BeginBulk，完成后 EndBulk 触发 rebuild）。
    /// </summary>
    public static int LoadStreaming(string path, FileIndex index, Dictionary<char, ulong> volumeUsns)
    {
        if (!File.Exists(path)) return -1;
        try
        {
            using var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, bufferSize: 1 << 20);
            using var br = new BinaryReader(fs);

            var magic = br.ReadBytes(8);
            if (!magic.SequenceEqual(MagicLegacy)) return -1;

            int entryCount = br.ReadInt32();
            int usnCount = br.ReadInt32();

            const int batchSize = 8192;
            var batch = new List<FileEntry>(batchSize);

            for (int i = 0; i < entryCount; i++)
            {
                batch.Add(new FileEntry
                {
                    FileRef = br.ReadUInt64(),
                    ParentRef = br.ReadUInt64(),
                    Name = br.ReadString(),
                    Attributes = br.ReadUInt32(),
                    Size = br.ReadInt64(),
                    LastWriteTimeTicks = br.ReadInt64(),
                    VolumeLetter = br.ReadChar(),
                });

                if (batch.Count >= batchSize)
                {
                    index.AddBulk(batch);
                    batch.Clear();
                }
            }

            if (batch.Count > 0)
                index.AddBulk(batch);

            for (int i = 0; i < usnCount; i++)
            {
                var vol = br.ReadChar();
                var usn = br.ReadUInt64();
                volumeUsns[vol] = usn;
            }

            return entryCount;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"[IndexSerializer] LoadStreaming error: {ex}");
            return -1;
        }
    }
}
