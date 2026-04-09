using System.IO;
using System.IO.Pipes;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Threading;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using FindX.Core.Index;
using FindX.Core.Search;
using FindX.Core.Pinyin;

namespace FindX.Service;

/// <summary>
/// Named Pipe JSON-RPC 服务端。
/// 管道名: FindX，支持多客户端并发连接。
/// 协议: 每行一个 JSON 请求，每行一个 JSON 响应。
/// </summary>
public sealed class IpcServer : IDisposable
{
    public const string PipeName = "FindX";

    private readonly FileIndex _index;
    private readonly SearchEngine _searchEngine;
    private CancellationTokenSource? _cts;
    private Task? _listenTask;

    public event Action<string>? Log;
    public Func<Task>? OnReindexRequested;
    /// <summary>为 null 时视为已就绪（兼容旧客户端）。返回 true 表示可认为索引建立完成。</summary>
    public Func<bool>? GetIndexReady { get; set; }

    public IpcServer(FileIndex index, SearchEngine searchEngine)
    {
        _index = index;
        _searchEngine = searchEngine;
    }

    public void Start()
    {
        _cts = new CancellationTokenSource();
        _listenTask = Task.Run(() => ListenLoop(_cts.Token));
        Log?.Invoke("IPC 服务已启动");
    }

    /// <summary>
    /// 默认可选安全描述符在「服务端管理员、客户端普通用户」或会话不一致时会导致客户端 <see cref="UnauthorizedAccessException"/>。
    /// 显式允许已认证用户读写本地命名管道。
    /// </summary>
    private static PipeSecurity CreatePipeSecurity()
    {
        var ps = new PipeSecurity();

        ps.AddAccessRule(new PipeAccessRule(
            new SecurityIdentifier(WellKnownSidType.AuthenticatedUserSid, null),
            PipeAccessRights.ReadWrite,
            AccessControlType.Allow));

        try
        {
            using var cur = WindowsIdentity.GetCurrent();
            if (cur.User is { } sid)
                ps.AddAccessRule(new PipeAccessRule(sid, PipeAccessRights.FullControl, AccessControlType.Allow));
        }
        catch { /* 无用户上下文时仍可依赖 AuthenticatedUser */ }

        ps.AddAccessRule(new PipeAccessRule(
            new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null),
            PipeAccessRights.FullControl,
            AccessControlType.Allow));

        return ps;
    }

    private async Task ListenLoop(CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            try
            {
                var pipeSecurity = CreatePipeSecurity();
                // .NET 无 (pipeName,..., pipeSecurity) 的直接构造，需用 Create 以写入 DACL
                var pipe = NamedPipeServerStreamAcl.Create(
                    PipeName,
                    PipeDirection.InOut,
                    NamedPipeServerStream.MaxAllowedServerInstances,
                    PipeTransmissionMode.Byte,
                    PipeOptions.Asynchronous,
                    0,
                    0,
                    pipeSecurity);

                await pipe.WaitForConnectionAsync(ct);
                var p = pipe;
                new Thread(() => HandleClientLoop(p, ct))
                {
                    IsBackground = true,
                    Name = "FindX.IpcClient",
                }.Start();
            }
            catch (OperationCanceledException) { break; }
            catch (Exception ex)
            {
                Log?.Invoke($"IPC listen error: {ex.Message}");
                await Task.Delay(500, ct);
            }
        }
    }

    /// <summary>同步读写 + 专用线程，避免全量索引时线程池饱和导致 async 延续永不运行、CLI 无响应。</summary>
    private void HandleClientLoop(NamedPipeServerStream pipe, CancellationToken ct)
    {
        try
        {
            using (pipe)
            using (var reader = new StreamReader(pipe, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, bufferSize: 1024, leaveOpen: false))
            using (var writer = new StreamWriter(pipe, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false)) { AutoFlush = true })
            {
                // 连接刚建立时 IsConnected 偶发为 false，若以此作为循环条件会导致永远不读请求、客户端一直等响应
                while (!ct.IsCancellationRequested)
                {
                    string? line;
                    try { line = reader.ReadLine(); }
                    catch (IOException) { break; }
                    if (line == null) break;

                    var response = ProcessRequest(line);
                    try { writer.WriteLine(response); }
                    catch (IOException) { break; }
                }
            }
        }
        catch (OperationCanceledException) { }
        catch (IOException) { }
        catch (Exception ex) { Log?.Invoke($"IPC client error: {ex.Message}"); }
    }

    private string ProcessRequest(string json)
    {
        try
        {
            var req = JsonSerializer.Deserialize<IpcRequest>(json, JsonOpts);
            if (req == null) return ErrorResponse(0, "Invalid request");

            return req.Method?.ToLowerInvariant() switch
            {
                "search" => HandleSearch(req),
                "status" => HandleStatus(req),
                "reindex" => HandleReindex(req),
                _ => ErrorResponse(req.Id, $"Unknown method: {req.Method}"),
            };
        }
        catch (Exception ex)
        {
            return ErrorResponse(0, ex.Message);
        }
    }

    private string HandleSearch(IpcRequest req)
    {
        // 建库/重扫期间排序表故意不维护；此时搜索会走 FileIndex 的「未就绪则全量 rebuild」逻辑，
        // 千万级条目下一次重建可达数分钟至数十分钟，IPC 线程表现为永久卡住。
        if (GetIndexReady?.Invoke() == false)
        {
            return ErrorResponse(req.Id,
                "索引尚未就绪（全量扫描或最终重建中）。请等 fx status 显示「就绪」后再搜索；建库期间请勿搜索。");
        }

        var query = req.Params?.Query ?? "";
        var maxResults = req.Params?.MaxResults ?? 50;
        var pathFilter = req.Params?.PathFilter;

        var sw = System.Diagnostics.Stopwatch.StartNew();
        var results = _searchEngine.Search(query, maxResults, pathFilter);
        sw.Stop();

        var items = results.Select(r => new IpcResultItem
        {
            Path = r.FullPath,
            Name = r.Name,
            IsDir = r.IsDirectory,
            Size = r.Size,
            Score = r.Score,
            LastWriteUtcTicks = r.LastWriteUtcTicks,
        }).ToList();

        var response = new IpcResponse
        {
            Id = req.Id,
            Result = new IpcSearchResult
            {
                Items = items,
                TotalCount = items.Count,
                ElapsedMs = sw.Elapsed.TotalMilliseconds,
            }
        };

        return JsonSerializer.Serialize(response, JsonOpts);
    }

    private string HandleStatus(IpcRequest req)
    {
        var ready = GetIndexReady?.Invoke() ?? true;
        var result = new
        {
            fileCount = _index.CountSnapshot,
            // WorkingSet 避免在索引暴增分配时与 GC.GetTotalMemory 争用导致 status 长时间卡住
            memoryMb = System.Diagnostics.Process.GetCurrentProcess().WorkingSet64 / 1024.0 / 1024.0,
            indexReady = ready,
        };
        return JsonSerializer.Serialize(new { id = req.Id, result }, JsonOpts);
    }

    private string HandleReindex(IpcRequest req)
    {
        OnReindexRequested?.Invoke();
        return JsonSerializer.Serialize(new { id = req.Id, result = new { ok = true } }, JsonOpts);
    }

    private static string ErrorResponse(int id, string msg)
        => JsonSerializer.Serialize(new { id, error = msg }, JsonOpts);

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    public void Dispose()
    {
        _cts?.Cancel();
        _listenTask?.Wait(3000);
        _cts?.Dispose();
    }
}

public class IpcRequest
{
    public int Id { get; set; }
    public string? Method { get; set; }
    public IpcParams? Params { get; set; }
}

public class IpcParams
{
    public string? Query { get; set; }
    public int? MaxResults { get; set; }
    public string? PathFilter { get; set; }
}

public class IpcResponse
{
    public int Id { get; set; }
    public IpcSearchResult? Result { get; set; }
}

public class IpcSearchResult
{
    public List<IpcResultItem> Items { get; set; } = new();
    public int TotalCount { get; set; }
    public double ElapsedMs { get; set; }
}

public class IpcResultItem
{
    public string Path { get; set; } = "";
    public string Name { get; set; } = "";
    public bool IsDir { get; set; }
    public long Size { get; set; }
    public int Score { get; set; }
    /// <summary>最后写入 UTC .NET ticks，0 表示索引中无此元数据</summary>
    public long LastWriteUtcTicks { get; set; }
}
