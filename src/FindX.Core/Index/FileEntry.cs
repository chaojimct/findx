namespace FindX.Core.Index;

public sealed class FileEntry
{
    /// <summary>Keeps the slot stable after USN deletion; deleted entries do not participate in search or persistence.</summary>
    public bool IsDeleted;

    public ulong FileRef;
    public ulong ParentRef;
    public string Name = "";
    public uint Attributes;
    public long Size;
    public long LastWriteTimeTicks;
    public long CreationTimeTicks;
    public long AccessTimeTicks;
    public char VolumeLetter;

    public bool IsDirectory => (Attributes & 0x10) != 0;
}
