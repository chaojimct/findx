using System.Diagnostics;
using System.Net.Http;

namespace FindX.Core.Update;

/// <summary>下载发布页中的 setup 安装包并启动 Inno Setup 进行覆盖安装。</summary>
public static class UpdateInstaller
{
    /// <summary>启动安装包，显示向导（不使用静默参数，避免静默失败且用户无感知）。</summary>
    public static void LaunchInstaller(string setupPath)
    {
        Process.Start(new ProcessStartInfo
        {
            FileName = setupPath,
            UseShellExecute = true,
        });
    }

    public static async Task<string> DownloadInstallerAsync(
        string downloadUrl,
        string versionLabel,
        IProgress<(long BytesRead, long? TotalBytes)>? progress,
        CancellationToken ct = default)
    {
        using var http = new HttpClient();
        http.DefaultRequestHeaders.UserAgent.ParseAdd("FindX-Updater/1.0");
        http.Timeout = TimeSpan.FromMinutes(15);

        var safe = new string(versionLabel.Where(c => !Path.GetInvalidFileNameChars().Contains(c)).ToArray());
        if (string.IsNullOrWhiteSpace(safe)) safe = "latest";
        var path = Path.Combine(Path.GetTempPath(), $"FindX-{safe}-setup.exe");

        using var response = await http.GetAsync(downloadUrl, HttpCompletionOption.ResponseHeadersRead, ct);
        response.EnsureSuccessStatusCode();

        long? total = response.Content.Headers.ContentLength;

        await using var httpStream = await response.Content.ReadAsStreamAsync(ct);
        await using var file = new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.None, 81920,
            FileOptions.Asynchronous);

        var buffer = new byte[81920];
        long readTotal = 0;
        int n;
        while ((n = await httpStream.ReadAsync(buffer.AsMemory(0, buffer.Length), ct)) > 0)
        {
            await file.WriteAsync(buffer.AsMemory(0, n), ct);
            readTotal += n;
            progress?.Report((readTotal, total));
        }

        return path;
    }
}
