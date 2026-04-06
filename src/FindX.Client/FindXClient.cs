using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace FindX.Client;

/// <summary>
/// FindX Named Pipe 客户端。提供异步搜索、状态查询、重建索引接口。
/// 自动管理管道连接，断线自动重连。
/// </summary>
public sealed class FindXClient : IDisposable
{
    public const string PipeName = "FindX";

    /// <summary>最近一次调用失败时的可读原因（连接失败、服务端 error 字段、JSON 解析失败等）。</summary>
    public string? LastError { get; private set; }
    /// <summary>
    /// 连接命名管道超时。服务端先监听管道再建索引，连接本身应较快。
    /// 无服务进程时过长会误伤 UX，默认 15s；可通过环境变量 <c>FINDX_CONNECT_TIMEOUT_MS</c> 覆盖（建库极慢机可设为 60000）。
    /// </summary>
    private static readonly int ConnectTimeoutMs = GetConnectTimeoutMs();

    private static int GetConnectTimeoutMs()
    {
        var v = Environment.GetEnvironmentVariable("FINDX_CONNECT_TIMEOUT_MS");
        if (int.TryParse(v, out var ms) && ms > 0)
            return ms;
        return 15000;
    }

    private NamedPipeClientStream? _pipe;
    private StreamReader? _reader;
    private StreamWriter? _writer;
    private int _nextId = 1;
    private readonly SemaphoreSlim _lock = new(1, 1);

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    public async Task<FindXSearchResult?> SearchAsync(string query, int maxResults = 50,
        string? pathFilter = null, CancellationToken ct = default)
    {
        LastError = null;
        var request = new
        {
            id = Interlocked.Increment(ref _nextId),
            method = "search",
            @params = new { query, maxResults, pathFilter },
        };

        var response = await SendAsync(JsonSerializer.Serialize(request, JsonOpts), ct).ConfigureAwait(false);
        if (response == null) return null;
        if (TryGetRpcError(response, out var rpcErr))
        {
            LastError = rpcErr;
            return null;
        }

        try
        {
            using var doc = JsonDocument.Parse(response);
            var root = doc.RootElement;
            if (!root.TryGetProperty("result", out var resultEl))
            {
                LastError = $"响应无 result 字段，原始: {Truncate(response)}";
                return null;
            }

            var result = new FindXSearchResult
            {
                TotalCount = resultEl.TryGetProperty("totalCount", out var tc) ? tc.GetInt32() : 0,
                ElapsedMs = resultEl.TryGetProperty("elapsedMs", out var em) ? em.GetDouble() : 0,
            };

            if (resultEl.TryGetProperty("items", out var itemsEl))
            {
                foreach (var item in itemsEl.EnumerateArray())
                {
                    result.Items.Add(new FindXResultItem
                    {
                        Path = item.TryGetProperty("path", out var p) ? p.GetString() ?? "" : "",
                        Name = item.TryGetProperty("name", out var n) ? n.GetString() ?? "" : "",
                        IsDir = item.TryGetProperty("isDir", out var d) && d.GetBoolean(),
                        Size = item.TryGetProperty("size", out var s) ? s.GetInt64() : 0,
                        Score = item.TryGetProperty("score", out var sc) ? sc.GetInt32() : 0,
                    });
                }
            }

            return result;
        }
        catch (Exception ex)
        {
            LastError = $"解析搜索响应失败: {ex.Message}";
            return null;
        }
    }

    public async Task<FindXStatus?> GetStatusAsync(CancellationToken ct = default)
    {
        LastError = null;
        var request = new
        {
            id = Interlocked.Increment(ref _nextId),
            method = "status",
        };

        var response = await SendAsync(JsonSerializer.Serialize(request, JsonOpts), ct).ConfigureAwait(false);
        if (response == null) return null;
        if (TryGetRpcError(response, out var rpcErr))
        {
            LastError = rpcErr;
            return null;
        }

        try
        {
            using var doc = JsonDocument.Parse(response);
            var root = doc.RootElement;
            if (!root.TryGetProperty("result", out var resultEl))
            {
                LastError = $"响应无 result 字段，原始: {Truncate(response)}";
                return null;
            }

            long fileCount = 0;
            if (resultEl.TryGetProperty("fileCount", out var fc) && fc.ValueKind == JsonValueKind.Number)
                fileCount = fc.GetInt64();

            var idxReady = true;
            if (resultEl.TryGetProperty("indexReady", out var irEl))
            {
                if (irEl.ValueKind == JsonValueKind.False) idxReady = false;
                else if (irEl.ValueKind == JsonValueKind.True) idxReady = true;
            }

            return new FindXStatus
            {
                FileCount = fileCount > int.MaxValue ? int.MaxValue : (int)fileCount,
                MemoryMb = resultEl.TryGetProperty("memoryMb", out var mm) && mm.TryGetDouble(out var mb) ? mb : 0,
                IndexReady = idxReady,
            };
        }
        catch (Exception ex)
        {
            LastError = $"解析状态响应失败: {ex.Message}";
            return null;
        }
    }

    public async Task<bool> ReindexAsync(CancellationToken ct = default)
    {
        var request = new
        {
            id = Interlocked.Increment(ref _nextId),
            method = "reindex",
        };

        var response = await SendAsync(JsonSerializer.Serialize(request, JsonOpts), ct);
        return response != null;
    }

    private async Task<string?> SendAsync(string json, CancellationToken ct)
    {
        await _lock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            await EnsureConnectedAsync(ct).ConfigureAwait(false);
            if (_writer == null || _reader == null)
            {
                LastError ??= "内部错误: Reader/Writer 未初始化";
                return null;
            }

            await _writer.WriteLineAsync(json.AsMemory(), ct).ConfigureAwait(false);
            await _writer.FlushAsync(ct).ConfigureAwait(false);
            var line = await _reader.ReadLineAsync(ct).ConfigureAwait(false);
            if (line == null)
                LastError ??= "服务端在返回数据前关闭了连接（请确认运行的是与本 CLI 配套的 FindX.exe，且仅一个实例持有管道）。";
            return line;
        }
        catch (Exception ex)
        {
            LastError ??= $"{ex.GetType().Name}: {ex.Message}";
            Disconnect();
            return null;
        }
        finally { _lock.Release(); }
    }

    private async Task EnsureConnectedAsync(CancellationToken ct)
    {
        if (_pipe is { IsConnected: true }) return;
        Disconnect();

        _pipe = new NamedPipeClientStream(".", PipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        await _pipe.ConnectAsync(ConnectTimeoutMs, ct).ConfigureAwait(false);
        _reader = new StreamReader(_pipe, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false), detectEncodingFromByteOrderMarks: false);
        _writer = new StreamWriter(_pipe, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false)) { AutoFlush = false };
    }

    private static bool TryGetRpcError(string json, out string error)
    {
        error = "";
        try
        {
            using var doc = JsonDocument.Parse(json);
            if (doc.RootElement.TryGetProperty("error", out var el))
            {
                error = el.GetString() ?? el.ToString();
                return true;
            }
        }
        catch { /* ignore */ }

        return false;
    }

    private static string Truncate(string s, int max = 200)
        => s.Length <= max ? s : s[..max] + "…";

    private void Disconnect()
    {
        try { _reader?.Dispose(); } catch { /* 服务端已断开时 Reader 可能已不可用 */ }
        try { _writer?.Dispose(); } catch { /* Flush 时管道可能已关闭 */ }
        try { _pipe?.Dispose(); } catch { }
        _reader = null;
        _writer = null;
        _pipe = null;
    }

    public void Dispose()
    {
        Disconnect();
        _lock.Dispose();
    }
}
