using System.Net.Http;
using System.Net.Http.Json;
using System.Reflection;
using System.Text.Json.Serialization;

namespace FindX.Core.Update;

public sealed class UpdateInfo
{
    public string CurrentVersion { get; init; } = "";
    public string LatestVersion { get; init; } = "";
    public string? ReleaseUrl { get; init; }
    public string? DownloadUrl { get; init; }
    public string? ReleaseNotes { get; init; }
    public DateTime? PublishedAt { get; init; }
    public bool HasUpdate { get; init; }
}

public sealed class UpdateChecker : IDisposable
{
    private const string DefaultOwner = "chaojimct";
    private const string DefaultRepo = "findx";

    private readonly HttpClient _http;
    private readonly string _owner;
    private readonly string _repo;

    public UpdateChecker(string? owner = null, string? repo = null)
    {
        (owner, repo) = ResolveOwnerRepo(owner, repo);
        _owner = owner;
        _repo = repo;
        _http = new HttpClient();
        _http.DefaultRequestHeaders.UserAgent.ParseAdd("FindX-UpdateChecker/1.0");
        _http.Timeout = TimeSpan.FromSeconds(15);
    }

    /// <summary>环境变量 FINDX_UPDATE_REPO=owner/repo，便于 fork / 私有源；未设置则用默认仓库。</summary>
    private static (string Owner, string Repo) ResolveOwnerRepo(string? owner, string? repo)
    {
        if (owner != null && repo != null)
            return (owner, repo);

        var env = Environment.GetEnvironmentVariable("FINDX_UPDATE_REPO")?.Trim();
        if (!string.IsNullOrEmpty(env))
        {
            var idx = env.IndexOf('/');
            if (idx > 0 && idx < env.Length - 1)
            {
                var o = env[..idx].Trim();
                var r = env[(idx + 1)..].Trim();
                if (o.Length > 0 && r.Length > 0)
                    return (o, r);
            }
        }

        return (owner ?? DefaultOwner, repo ?? DefaultRepo);
    }

    public static string GetCurrentVersion()
    {
        var asm = Assembly.GetEntryAssembly() ?? Assembly.GetExecutingAssembly();
        var infoVer = asm.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion;
        if (infoVer != null)
        {
            var plusIdx = infoVer.IndexOf('+');
            if (plusIdx > 0) infoVer = infoVer[..plusIdx];
            return infoVer;
        }
        return asm.GetName().Version?.ToString(3) ?? "0.0.0";
    }

    public async Task<UpdateInfo?> CheckAsync(CancellationToken ct = default)
    {
        try
        {
            var url = $"https://api.github.com/repos/{_owner}/{_repo}/releases/latest";
            var release = await _http.GetFromJsonAsync<GitHubRelease>(url, ct);
            if (release?.TagName == null) return null;

            var latestStr = StripLeadingV(release.TagName);
            var currentStr = GetCurrentVersion();

            bool hasUpdate = CompareVersions(latestStr, currentStr) > 0;

            string? downloadUrl = null;
            if (release.Assets != null)
            {
                var setupAsset = Array.Find(release.Assets,
                    a => a.Name != null && a.Name.Contains("setup", StringComparison.OrdinalIgnoreCase)
                                        && a.Name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase));
                downloadUrl = setupAsset?.BrowserDownloadUrl;
            }

            return new UpdateInfo
            {
                CurrentVersion = currentStr,
                LatestVersion = latestStr,
                HasUpdate = hasUpdate,
                ReleaseUrl = release.HtmlUrl,
                DownloadUrl = downloadUrl,
                ReleaseNotes = release.Body,
                PublishedAt = release.PublishedAt,
            };
        }
        catch
        {
            return null;
        }
    }

    /// <summary>去掉 tag 前缀 v/V（逐字符 TrimStart 会误伤其他情况，这里只去一层常见前缀）。</summary>
    private static string StripLeadingV(string tag)
    {
        tag = tag.Trim();
        if (tag.Length >= 2 &&
            (tag[0] == 'v' || tag[0] == 'V') &&
            char.IsDigit(tag[1]))
            return tag[1..];
        return tag;
    }

    /// <summary>剥离 SemVer 的 +build / -prerelease 后再交给 <see cref="Version"/>（仅支持 x.x.x.x 数值段）。</summary>
    private static bool TryParseCoreVersion(string ver, out Version? version)
    {
        ver = ver.Trim();
        var plus = ver.IndexOf('+');
        if (plus >= 0) ver = ver[..plus];
        var dash = ver.IndexOf('-');
        if (dash >= 0) ver = ver[..dash];
        ver = NormalizeSemver(ver);
        return Version.TryParse(ver, out version);
    }

    /// <returns>&lt;0 当前较新；0 相同或无法比较；&gt;0 远程较新。</returns>
    private static int CompareVersions(string remoteLatest, string localCurrent)
    {
        if (!TryParseCoreVersion(remoteLatest, out var latest) || latest is null ||
            !TryParseCoreVersion(localCurrent, out var current) || current is null)
            return 0;

        return latest.CompareTo(current);
    }

    private static string NormalizeSemver(string ver)
    {
        var parts = ver.Split('.');
        return parts.Length switch
        {
            1 => $"{parts[0]}.0.0",
            2 => $"{parts[0]}.{parts[1]}.0",
            _ => ver,
        };
    }

    public void Dispose() => _http.Dispose();

    private sealed class GitHubRelease
    {
        [JsonPropertyName("tag_name")]
        public string? TagName { get; set; }

        [JsonPropertyName("html_url")]
        public string? HtmlUrl { get; set; }

        [JsonPropertyName("body")]
        public string? Body { get; set; }

        [JsonPropertyName("published_at")]
        public DateTime? PublishedAt { get; set; }

        [JsonPropertyName("assets")]
        public GitHubAsset[]? Assets { get; set; }
    }

    private sealed class GitHubAsset
    {
        [JsonPropertyName("name")]
        public string? Name { get; set; }

        [JsonPropertyName("browser_download_url")]
        public string? BrowserDownloadUrl { get; set; }
    }
}
