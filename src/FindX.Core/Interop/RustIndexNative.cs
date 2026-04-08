using System.Runtime.InteropServices;

namespace FindX.Core.Interop;

/// <summary>Rust <c>findx_engine</c> 紧凑索引的 C ABI P/Invoke。</summary>
internal static class RustIndexNative
{
    private const string DllName = "findx_engine";

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern IntPtr findx_engine_create();

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_destroy(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_clear(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_try_get_index(IntPtr p, ushort vol, ulong fileRef, out int outIdx);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_live_count(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_add_entry_utf16(
        IntPtr p,
        ushort vol,
        ulong fileRef,
        ulong parentRef,
        IntPtr nameUtf16,
        int nameLen,
        uint attr,
        long size,
        long mtime);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_upsert_entry_utf16(
        IntPtr p,
        ushort vol,
        ulong fileRef,
        ulong parentRef,
        IntPtr nameUtf16,
        int nameLen,
        uint attr,
        long size,
        long mtime);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_remove_by_ref(IntPtr p, ushort vol, ulong fileRef);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_rebuild_indexes(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_begin_bulk(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_end_bulk(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_is_sort_ready(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_is_in_bulk_load(IntPtr p);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_search_name_prefix(
        IntPtr p,
        IntPtr prefixUtf8,
        int prefixLen,
        IntPtr outIndices,
        int outCap);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_search_pinyin_prefix(
        IntPtr p,
        IntPtr prefixUtf8,
        int prefixLen,
        IntPtr outIndices,
        int outCap);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_search_full_py_prefix(
        IntPtr p,
        IntPtr prefixUtf8,
        int prefixLen,
        IntPtr outIndices,
        int outCap);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_search_name_contains(
        IntPtr p,
        IntPtr needleUtf8,
        int needleLen,
        IntPtr outIndices,
        int outCap);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_search_full_py_contains(
        IntPtr p,
        IntPtr needleUtf8,
        int needleLen,
        IntPtr outIndices,
        int outCap);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_search_initials_contains(
        IntPtr p,
        IntPtr needleUtf8,
        int needleLen,
        IntPtr outIndices,
        int outCap);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_get_live_record(
        IntPtr p,
        int idx,
        out ulong fileRef,
        out ulong parentRef,
        out ushort vol,
        out uint attr,
        out long size,
        out long mtime);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_get_name_utf16(
        IntPtr p,
        int idx,
        IntPtr buf,
        int bufLenChars);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    public delegate int VisitLiveFn(IntPtr user, int idx);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_visit_live(
        IntPtr p,
        IntPtr user,
        VisitLiveFn? cb);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    public delegate int PersistRowFn(
        IntPtr user,
        ulong fileRef,
        ulong parentRef,
        IntPtr nameUtf16,
        int nameLen,
        uint attr,
        long size,
        long mtime,
        ushort vol);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern void findx_engine_for_each_persist(
        IntPtr p,
        IntPtr user,
        PersistRowFn? cb);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_build_path_utf16(
        IntPtr p,
        int idx,
        IntPtr buf,
        int bufLenChars);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_save_file(
        IntPtr p,
        IntPtr pathUtf16,
        int pathLen);

    [DllImport(DllName, CallingConvention = CallingConvention.StdCall)]
    public static extern int findx_engine_load_file(
        IntPtr p,
        IntPtr pathUtf16,
        int pathLen);
}
