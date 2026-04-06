namespace FindX.Core.Index;

public sealed class FileEntry
{
    /// <summary>USN 删除后置为墓碑；仍保留槽位以稳定下标，不参与搜索与持久化。</summary>
    public bool IsDeleted;

    public ulong FileRef;
    public ulong ParentRef;
    public string Name = "";
    public uint Attributes;
    public long Size;
    public long LastWriteTimeTicks;
    public char VolumeLetter;

    /// <summary>拼音首字母链（仅用于拼音前缀 Trie）；全拼由 <see cref="Pinyin.PinyinMatcher"/> 在查询时按名字现算，避免索引阶段为每个文件分配 string[]。</summary>
    public string PinyinInitials = "";

    public bool IsDirectory => (Attributes & 0x10) != 0;

    public void ComputePinyin()
    {
        PinyinInitials = Pinyin.PinyinTable.GetInitials(Name);
    }
}
