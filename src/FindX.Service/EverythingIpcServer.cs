using System.Runtime.InteropServices;
using System.Text;
using FindX.Core.Index;
using FindX.Core.Search;

namespace FindX.Service;

/// <summary>
/// Everything SDK v2 兼容层：创建类名为 "EVERYTHING" 的隐藏窗口，
/// 处理 WM_COPYDATA (QUERY/QUERY2)，使 Flow Launcher、Wox、PowerToys Run 等
/// 第三方工具无需任何修改即可使用 FindX 替代 Everything。
/// </summary>
public sealed class EverythingIpcServer : IDisposable
{
    private readonly FileIndex _index;
    private readonly SearchEngine _search;
    private Thread? _thread;
    private IntPtr _hwnd;
    private volatile bool _disposed;

    private sealed class PendingReplyData
    {
        public IntPtr ReplyHwnd;
        public IntPtr MyHwnd;
        public uint ReplyMsg;
        public byte[] Buffer = null!;
        public int TotalResults;
        public int ItemCount;
    }

    public event Action<string>? Log;
    public Func<bool>? GetIndexReady { get; set; }

    // WM_COPYDATA dwData 值 (everything_ipc.h)
    const int COPYDATA_COMMAND_LINE_UTF8 = 0;
    const int COPYDATA_QUERYA = 1;
    const int COPYDATA_QUERYW = 2;
    const int COPYDATA_QUERY2A = 17;
    const int COPYDATA_QUERY2W = 18;
    const int COPYDATA_GET_RUN_COUNTA = 19;
    const int COPYDATA_GET_RUN_COUNTW = 20;
    const int COPYDATA_SET_RUN_COUNTA = 21;
    const int COPYDATA_SET_RUN_COUNTW = 22;
    const int COPYDATA_INC_RUN_COUNTA = 23;
    const int COPYDATA_INC_RUN_COUNTW = 24;

    // WM_USER wParam 值 (everything_ipc.h)
    const uint IPC_GET_MAJOR_VERSION = 0;
    const uint IPC_GET_MINOR_VERSION = 1;
    const uint IPC_GET_REVISION = 2;
    const uint IPC_GET_BUILD_NUMBER = 3;
    const uint IPC_EXIT = 4;
    const uint IPC_GET_TARGET_MACHINE = 5;
    const uint IPC_IS_NTFS_DRIVE_INDEXED = 400;
    const uint IPC_IS_DB_LOADED = 401;
    const uint IPC_IS_DB_BUSY = 402;
    const uint IPC_IS_ADMIN = 403;
    const uint IPC_IS_APPDATA = 404;
    const uint IPC_REBUILD_DB = 405;
    const uint IPC_UPDATE_ALL_FOLDER_INDEXES = 406;
    const uint IPC_SAVE_DB = 407;
    const uint IPC_SAVE_RUN_HISTORY = 408;
    const uint IPC_DELETE_RUN_HISTORY = 409;
    const uint IPC_IS_FAST_SORT = 410;
    const uint IPC_IS_FILE_INFO_INDEXED = 411;

    // Search flags
    const uint IPC_REGEX = 0x0001;
    const uint IPC_MATCHCASE = 0x0002;
    const uint IPC_MATCHWHOLEWORD = 0x0004;
    const uint IPC_MATCHPATH = 0x0008;

    // QUERY2 request flags
    const uint REQUEST_NAME = 0x00000001;
    const uint REQUEST_PATH = 0x00000002;
    const uint REQUEST_FULL_PATH_AND_NAME = 0x00000004;
    const uint REQUEST_EXTENSION = 0x00000008;
    const uint REQUEST_SIZE = 0x00000010;
    const uint REQUEST_DATE_CREATED = 0x00000020;
    const uint REQUEST_DATE_MODIFIED = 0x00000040;
    const uint REQUEST_DATE_ACCESSED = 0x00000080;
    const uint REQUEST_ATTRIBUTES = 0x00000100;
    const uint REQUEST_FILE_LIST_FILE_NAME = 0x00000200;
    const uint REQUEST_RUN_COUNT = 0x00000400;
    const uint REQUEST_DATE_RUN = 0x00000800;
    const uint REQUEST_DATE_RECENTLY_CHANGED = 0x00001000;
    const uint REQUEST_HIGHLIGHTED_NAME = 0x00002000;
    const uint REQUEST_HIGHLIGHTED_PATH = 0x00004000;
    const uint REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME = 0x00008000;

    // IPC item flags
    const uint ITEM_FOLDER = 0x01;

    const string WNDCLASS_IPC = "EVERYTHING_TASKBAR_NOTIFICATION";
    const string WNDCLASS_ALT = "EVERYTHING";

    const uint WS_POPUP = 0x80000000;

    public EverythingIpcServer(FileIndex index, SearchEngine search)
    {
        _index = index;
        _search = search;
        WndProcDelegate = WndProcImpl;
    }

    public void Start()
    {
        _thread = new Thread(MessageLoop)
        {
            IsBackground = true,
            Name = "FindX.EverythingIpc",
        };
        _thread.SetApartmentState(ApartmentState.STA);
        _thread.Start();
    }

    private IntPtr _hwndAlt;

    private void MessageLoop()
    {
        var hInst = GetModuleHandle(null);

        // 主窗口：EVERYTHING_TASKBAR_NOTIFICATION — SDK 标准类名
        var wc = new WNDCLASSEX
        {
            cbSize = Marshal.SizeOf<WNDCLASSEX>(),
            lpfnWndProc = WndProcDelegate,
            lpszClassName = WNDCLASS_IPC,
            hInstance = hInst,
        };
        if (RegisterClassEx(ref wc) == 0)
        {
            Log?.Invoke($"Everything IPC: RegisterClassEx({WNDCLASS_IPC}) 失败 (err={Marshal.GetLastWin32Error()})");
            return;
        }

        _hwnd = CreateWindowEx(0, WNDCLASS_IPC, "EVERYTHING",
            WS_POPUP, 0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, hInst, IntPtr.Zero);

        // 备用窗口：EVERYTHING — 部分工具直接查找此类名
        var wc2 = new WNDCLASSEX
        {
            cbSize = Marshal.SizeOf<WNDCLASSEX>(),
            lpfnWndProc = WndProcDelegate,
            lpszClassName = WNDCLASS_ALT,
            hInstance = hInst,
        };
        var atom2 = RegisterClassEx(ref wc2);
        if (atom2 != 0)
        {
            _hwndAlt = CreateWindowEx(0, WNDCLASS_ALT, "EVERYTHING",
                WS_POPUP, 0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, hInst, IntPtr.Zero);
            if (_hwndAlt == IntPtr.Zero)
                Log?.Invoke($"Everything IPC: CreateWindowEx({WNDCLASS_ALT}) 失败 (err={Marshal.GetLastWin32Error()})");
        }
        else
        {
            Log?.Invoke($"Everything IPC: RegisterClassEx({WNDCLASS_ALT}) 失败 (err={Marshal.GetLastWin32Error()})");
        }

        if (_hwnd == IntPtr.Zero && _hwndAlt == IntPtr.Zero)
        {
            Log?.Invoke($"Everything IPC: 所有窗口创建失败");
            return;
        }

        Log?.Invoke("Everything IPC 兼容层已启动");

        while (!_disposed)
        {
            if (GetMessage(out var msg, IntPtr.Zero, 0, 0) <= 0)
                break;
            TranslateMessage(ref msg);
            DispatchMessage(ref msg);
        }

        if (_hwnd != IntPtr.Zero) DestroyWindow(_hwnd);
        if (_hwndAlt != IntPtr.Zero) DestroyWindow(_hwndAlt);
        UnregisterClass(WNDCLASS_IPC, hInst);
        UnregisterClass(WNDCLASS_ALT, hInst);
    }

    private WndProc WndProcDelegate = null!;

    private IntPtr WndProcImpl(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam)
    {
        if (msg == WM_COPYDATA)
        {
            try
            {
                return HandleCopyData(hwnd, wParam, lParam);
            }
            catch (Exception ex)
            {
                Log?.Invoke($"Everything IPC 异常: {ex.Message}");
            }
        }
        if (msg == WM_USER)
        {
            var cmd = (uint)(long)wParam;
            return cmd switch
            {
                IPC_GET_MAJOR_VERSION => (IntPtr)1,
                IPC_GET_MINOR_VERSION => (IntPtr)4,
                IPC_GET_REVISION => (IntPtr)1,
                IPC_GET_BUILD_NUMBER => (IntPtr)1026,
                IPC_EXIT => IntPtr.Zero,
                IPC_GET_TARGET_MACHINE => (IntPtr)2,                                // x64
                IPC_IS_NTFS_DRIVE_INDEXED => (IntPtr)1,
                IPC_IS_DB_LOADED => (IntPtr)((GetIndexReady?.Invoke() ?? true) ? 1 : 0),
                IPC_IS_DB_BUSY => IntPtr.Zero,
                IPC_IS_ADMIN => IntPtr.Zero,
                IPC_IS_APPDATA => IntPtr.Zero,
                IPC_REBUILD_DB => IntPtr.Zero,
                IPC_UPDATE_ALL_FOLDER_INDEXES => IntPtr.Zero,
                IPC_SAVE_DB => IntPtr.Zero,
                IPC_SAVE_RUN_HISTORY => IntPtr.Zero,
                IPC_DELETE_RUN_HISTORY => IntPtr.Zero,
                IPC_IS_FAST_SORT => (IntPtr)1,
                IPC_IS_FILE_INFO_INDEXED => (IntPtr)1,
                _ => IntPtr.Zero,
            };
        }

        return DefWindowProc(hwnd, msg, wParam, lParam);
    }

    private unsafe IntPtr HandleCopyData(IntPtr hwnd, IntPtr wParam, IntPtr lParam)
    {
        var cds = Marshal.PtrToStructure<COPYDATASTRUCT>(lParam);
        var replyHwnd = wParam;

        switch ((int)cds.dwData)
        {
            case COPYDATA_COMMAND_LINE_UTF8:
                return (IntPtr)1;

            case COPYDATA_QUERYA:
                return HandleQueryA(cds, replyHwnd, hwnd);

            case COPYDATA_QUERYW:
                return HandleQueryW(cds, replyHwnd, hwnd);

            case COPYDATA_QUERY2A:
                return HandleQuery2A(cds, replyHwnd, hwnd);

            case COPYDATA_QUERY2W:
                return HandleQuery2W(cds, replyHwnd, hwnd);

            case COPYDATA_GET_RUN_COUNTA:
            case COPYDATA_GET_RUN_COUNTW:
                return IntPtr.Zero;

            case COPYDATA_SET_RUN_COUNTA:
            case COPYDATA_SET_RUN_COUNTW:
                return (IntPtr)1;

            case COPYDATA_INC_RUN_COUNTA:
            case COPYDATA_INC_RUN_COUNTW:
                return IntPtr.Zero;

            default:
                return IntPtr.Zero;
        }
    }

    private unsafe IntPtr HandleQueryA(COPYDATASTRUCT cds, IntPtr replyHwnd, IntPtr myHwnd)
    {
        if (cds.cbData < 20) return IntPtr.Zero;
        var data = new Span<byte>((void*)cds.lpData, (int)cds.cbData);

        uint maxResults = BitConverter.ToUInt32(data[..4]);
        uint offset = BitConverter.ToUInt32(data[4..8]);
        uint searchFlags = BitConverter.ToUInt32(data[8..12]);
        uint replyMsg = BitConverter.ToUInt32(data[16..20]);

        string searchStr = ExtractAString(data[20..]);

        if (maxResults == 0) maxResults = 100;
        var results = DoSearch(searchStr, (int)(maxResults + offset), searchFlags);

        SendResultList(replyHwnd, myHwnd, replyMsg, results, offset, maxResults, 0);
        return (IntPtr)1;
    }

    private unsafe IntPtr HandleQueryW(COPYDATASTRUCT cds, IntPtr replyHwnd, IntPtr myHwnd)
    {
        if (cds.cbData < 20) return IntPtr.Zero;
        var data = new Span<byte>((void*)cds.lpData, (int)cds.cbData);

        uint maxResults = BitConverter.ToUInt32(data[..4]);
        uint offset = BitConverter.ToUInt32(data[4..8]);
        uint searchFlags = BitConverter.ToUInt32(data[8..12]);
        uint replyMsg = BitConverter.ToUInt32(data[16..20]);

        var searchBytes = data[20..];
        string searchStr = ExtractWString(searchBytes);

        if (maxResults == 0) maxResults = 100;
        var results = DoSearch(searchStr, (int)(maxResults + offset), searchFlags);

        SendResultList(replyHwnd, myHwnd, replyMsg, results, offset, maxResults, 0);
        return (IntPtr)1;
    }

    private unsafe IntPtr HandleQuery2A(COPYDATASTRUCT cds, IntPtr replyHwnd, IntPtr myHwnd)
    {
        if (cds.cbData < 28) return IntPtr.Zero;
        var data = new Span<byte>((void*)cds.lpData, (int)cds.cbData);

        uint replyMsg = BitConverter.ToUInt32(data.Slice(4, 4));
        uint searchFlags = BitConverter.ToUInt32(data.Slice(8, 4));
        uint offset = BitConverter.ToUInt32(data.Slice(12, 4));
        uint maxResults = BitConverter.ToUInt32(data.Slice(16, 4));
        uint requestFlags = BitConverter.ToUInt32(data.Slice(20, 4));
        uint sortType = BitConverter.ToUInt32(data.Slice(24, 4));
        string searchStr = ExtractAString(data[28..]);

        if (maxResults == 0) maxResults = 100;
        var results = DoSearch(searchStr, (int)(maxResults + offset), searchFlags);

        SendResultList(replyHwnd, myHwnd, replyMsg, results, offset, maxResults, requestFlags);
        return (IntPtr)1;
    }

    private unsafe IntPtr HandleQuery2W(COPYDATASTRUCT cds, IntPtr replyHwnd, IntPtr myHwnd)
    {
        // EVERYTHING_IPC_QUERY2 (#pragma pack 1):
        // [0..4] reply_hwnd, [4..8] reply_copydata_message,
        // [8..12] search_flags, [12..16] offset, [16..20] max_results,
        // [20..24] request_flags, [24..28] sort_type, [28..] search_string
        if (cds.cbData < 28) return IntPtr.Zero;
        var data = new Span<byte>((void*)cds.lpData, (int)cds.cbData);

        uint replyMsg = BitConverter.ToUInt32(data.Slice(4, 4));
        uint searchFlags = BitConverter.ToUInt32(data.Slice(8, 4));
        uint offset = BitConverter.ToUInt32(data.Slice(12, 4));
        uint maxResults = BitConverter.ToUInt32(data.Slice(16, 4));
        uint requestFlags = BitConverter.ToUInt32(data.Slice(20, 4));
        uint sortType = BitConverter.ToUInt32(data.Slice(24, 4));
        string searchStr = ExtractWString(data[28..]);

        if (maxResults == 0) maxResults = 100;
        var results = DoSearch(searchStr, (int)(maxResults + offset), searchFlags);

        SendResultList(replyHwnd, myHwnd, replyMsg, results, offset, maxResults, requestFlags);
        return (IntPtr)1;
    }

    private List<SearchResult> DoSearch(string query, int max, uint flags)
    {
        if (GetIndexReady?.Invoke() == false)
            return new List<SearchResult>();

        if (string.IsNullOrWhiteSpace(query))
            return new List<SearchResult>();

        return _search.Search(query, Math.Min(max, 8192));
    }

    private static void WriteIpcString(System.IO.BinaryWriter bw, string s)
    {
        bw.Write((uint)s.Length);
        bw.Write(Encoding.Unicode.GetBytes(s + "\0"));
    }

    private unsafe void BuildAndSendList1(IntPtr replyHwnd, IntPtr myHwnd, uint replyMsg,
        List<SearchResult> results, int offset, int start, int count)
    {
        // EVERYTHING_IPC_LISTW (#pragma pack 1):
        // totfolders(4) + totfiles(4) + totitems(4) +
        // numfolders(4) + numfiles(4) + numitems(4) + offset(4) = 28B
        // EVERYTHING_IPC_ITEMW[numitems]: flags(4) + filename_offset(4) + path_offset(4) = 12B each

        int totFolders = 0, totFiles = 0, numFolders = 0, numFiles = 0;
        foreach (var r in results) { if (r.IsDirectory) totFolders++; else totFiles++; }

        var strings = new List<byte>();
        var items = new List<(uint flags, int fnOff, int pathOff)>();

        for (int i = start; i < start + count; i++)
        {
            var r = results[i];
            string name = r.Name;
            string dir = "";
            int sep = r.FullPath.LastIndexOf('\\');
            if (sep >= 0) dir = r.FullPath[..sep];

            int fnOff = strings.Count / 2;
            strings.AddRange(Encoding.Unicode.GetBytes(name + "\0"));
            int pathOff = strings.Count / 2;
            strings.AddRange(Encoding.Unicode.GetBytes(dir + "\0"));

            uint flags = r.IsDirectory ? ITEM_FOLDER : 0u;
            if (r.IsDirectory) numFolders++; else numFiles++;
            items.Add((flags, fnOff, pathOff));
        }

        int headerSize = 28;
        int strBase = headerSize + count * 12;
        int total = strBase + strings.Count;
        var buf = new byte[total];

        BitConverter.TryWriteBytes(buf.AsSpan(0), (uint)totFolders);        // totfolders
        BitConverter.TryWriteBytes(buf.AsSpan(4), (uint)totFiles);          // totfiles
        BitConverter.TryWriteBytes(buf.AsSpan(8), (uint)results.Count);     // totitems
        BitConverter.TryWriteBytes(buf.AsSpan(12), (uint)numFolders);       // numfolders
        BitConverter.TryWriteBytes(buf.AsSpan(16), (uint)numFiles);         // numfiles
        BitConverter.TryWriteBytes(buf.AsSpan(20), (uint)count);            // numitems
        BitConverter.TryWriteBytes(buf.AsSpan(24), (uint)offset);           // offset

        for (int i = 0; i < items.Count; i++)
        {
            int pos = headerSize + i * 12;
            var (fl, fn, pa) = items[i];
            BitConverter.TryWriteBytes(buf.AsSpan(pos), fl);
            BitConverter.TryWriteBytes(buf.AsSpan(pos + 4), (uint)(strBase + fn * 2));
            BitConverter.TryWriteBytes(buf.AsSpan(pos + 8), (uint)(strBase + pa * 2));
        }
        strings.CopyTo(0, buf, strBase, strings.Count);

        var replyData = new PendingReplyData
        {
            ReplyHwnd = replyHwnd, MyHwnd = myHwnd, ReplyMsg = replyMsg,
            Buffer = buf, TotalResults = results.Count, ItemCount = count,
        };
        ThreadPool.QueueUserWorkItem(_ => { Thread.Sleep(5); SendPendingReply(replyData); });
    }

    private unsafe void SendResultList(IntPtr replyHwnd, IntPtr myHwnd, uint replyMsg,
        List<SearchResult> results, uint offset, uint maxResults, uint requestFlags)
    {
        int start = (int)Math.Min(offset, results.Count);
        int count = (int)Math.Min(maxResults, results.Count - start);
        if (count < 0) count = 0;

        bool isQuery2 = requestFlags != 0;

        if (!isQuery2)
        {
            BuildAndSendList1(replyHwnd, myHwnd, replyMsg, results, (int)offset, start, count);
            return;
        }

        // EVERYTHING_IPC_LIST2 (#pragma pack 1):
        // Header: totitems(4) + numitems(4) + offset(4) + request_flags(4) + sort_type(4) = 20B
        // Items:  ITEM2[numitems] — each 8B (flags + data_offset)
        // Data:   variable-length data for each item at data_offset (relative to LIST2 start)

        using var ms = new System.IO.MemoryStream();
        using var bw = new System.IO.BinaryWriter(ms);

        int headerSize = 20;
        int itemsSize = count * 8;
        int dataStart = headerSize + itemsSize;

        // Placeholder header + items (fill later)
        bw.Write(new byte[dataStart]);

        // Build data for each item, track data_offset
        var dataOffsets = new int[count];
        for (int i = 0; i < count; i++)
        {
            var r = results[start + i];
            dataOffsets[i] = (int)ms.Position;

            string name = r.Name;
            string dir = "";
            int sep = r.FullPath.LastIndexOf('\\');
            if (sep >= 0) dir = r.FullPath[..sep];

            string ext = "";
            int dot = name.LastIndexOf('.');
            if (dot >= 0 && !r.IsDirectory) ext = name[(dot + 1)..];

            uint rf = requestFlags;
            if ((rf & REQUEST_NAME) != 0) WriteIpcString(bw, name);
            if ((rf & REQUEST_PATH) != 0) WriteIpcString(bw, dir);
            if ((rf & REQUEST_FULL_PATH_AND_NAME) != 0) WriteIpcString(bw, r.FullPath);
            if ((rf & REQUEST_EXTENSION) != 0) WriteIpcString(bw, ext);
            if ((rf & REQUEST_SIZE) != 0) bw.Write(r.Size);
            if ((rf & REQUEST_DATE_CREATED) != 0) bw.Write(0L);
            if ((rf & REQUEST_DATE_MODIFIED) != 0) bw.Write(0L);
            if ((rf & REQUEST_DATE_ACCESSED) != 0) bw.Write(0L);
            if ((rf & REQUEST_ATTRIBUTES) != 0) bw.Write(r.IsDirectory ? 0x10u : 0x20u);
            if ((rf & REQUEST_FILE_LIST_FILE_NAME) != 0) WriteIpcString(bw, "");
            if ((rf & REQUEST_RUN_COUNT) != 0) bw.Write(0u);
            if ((rf & REQUEST_DATE_RUN) != 0) bw.Write(0L);
            if ((rf & REQUEST_DATE_RECENTLY_CHANGED) != 0) bw.Write(0L);
            if ((rf & REQUEST_HIGHLIGHTED_NAME) != 0) WriteIpcString(bw, name);
            if ((rf & REQUEST_HIGHLIGHTED_PATH) != 0) WriteIpcString(bw, dir);
            if ((rf & REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME) != 0) WriteIpcString(bw, r.FullPath);
        }

        bw.Flush();
        var buf = ms.ToArray();

        // Fill header
        BitConverter.TryWriteBytes(buf.AsSpan(0), (uint)results.Count);   // totitems
        BitConverter.TryWriteBytes(buf.AsSpan(4), (uint)count);            // numitems
        BitConverter.TryWriteBytes(buf.AsSpan(8), offset);                 // offset
        BitConverter.TryWriteBytes(buf.AsSpan(12), requestFlags);          // request_flags
        BitConverter.TryWriteBytes(buf.AsSpan(16), 1u);                    // sort_type = NAME_ASC

        // Fill items (flags + data_offset)
        for (int i = 0; i < count; i++)
        {
            int pos = headerSize + i * 8;
            uint flags = results[start + i].IsDirectory ? ITEM_FOLDER : 0u;
            BitConverter.TryWriteBytes(buf.AsSpan(pos), flags);
            BitConverter.TryWriteBytes(buf.AsSpan(pos + 4), (uint)dataOffsets[i]);
        }

        var replyData = new PendingReplyData
        {
            ReplyHwnd = replyHwnd,
            MyHwnd = myHwnd,
            ReplyMsg = replyMsg,
            Buffer = buf,
            TotalResults = results.Count,
            ItemCount = count,
        };
        ThreadPool.QueueUserWorkItem(_ =>
        {
            Thread.Sleep(5);
            SendPendingReply(replyData);
        });
    }

    private unsafe void SendPendingReply(PendingReplyData pr)
    {
        fixed (byte* pBuf = pr.Buffer)
        {
            COPYDATASTRUCT cds;
            cds.dwData = (IntPtr)pr.ReplyMsg;
            cds.cbData = (uint)pr.Buffer.Length;
            cds.lpData = (IntPtr)pBuf;

            SendMessage(pr.ReplyHwnd, WM_COPYDATA, pr.MyHwnd, (IntPtr)(&cds));
        }
    }

    private static string ExtractWString(Span<byte> data)
    {
        int charCount = data.Length / 2;
        var chars = MemoryMarshal.Cast<byte, char>(data);
        int nullIdx = chars.IndexOf('\0');
        return nullIdx >= 0 ? new string(chars[..nullIdx]) : new string(chars);
    }

    private static string ExtractAString(Span<byte> data)
    {
        int nullIdx = data.IndexOf((byte)0);
        int len = nullIdx >= 0 ? nullIdx : data.Length;
        return Encoding.Default.GetString(data[..len]);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        if (_hwnd != IntPtr.Zero)
            PostMessage(_hwnd, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        _thread?.Join(3000);
    }

    // -------------------------------------------------------
    // Win32 interop
    // -------------------------------------------------------

    const uint WM_COPYDATA = 0x004A;
    const uint WM_USER = 0x0400;
    const uint WM_QUIT = 0x0012;

    delegate IntPtr WndProc(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct WNDCLASSEX
    {
        public int cbSize;
        public uint style;
        [MarshalAs(UnmanagedType.FunctionPtr)]
        public WndProc lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        public string? lpszMenuName;
        public string lpszClassName;
        public IntPtr hIconSm;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct MSG
    {
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int ptX, ptY;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct COPYDATASTRUCT
    {
        public IntPtr dwData;
        public uint cbData;
        public IntPtr lpData;
    }

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern ushort RegisterClassEx(ref WNDCLASSEX wc);

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool UnregisterClass(string lpClassName, IntPtr hInstance);

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr CreateWindowEx(uint exStyle, string className, string windowName,
        uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);

    [DllImport("user32.dll")]
    static extern bool DestroyWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    static extern int GetMessage(out MSG msg, IntPtr hwnd, uint filterMin, uint filterMax);

    [DllImport("user32.dll")]
    static extern bool TranslateMessage(ref MSG msg);

    [DllImport("user32.dll")]
    static extern IntPtr DispatchMessage(ref MSG msg);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr DefWindowProc(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr GetModuleHandle(string? moduleName);
}
