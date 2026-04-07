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
    private const string DefaultOwner = "user";
    private const string DefaultRepo = "findx";

    private readonly HttpClient _http;
    private readonly string _owner;
    private readonly string _repo;

    public UpdateChecker(string? owner = null, string? repo = null)
    {
        _owner = owner ?? DefaultOwner;
        _repo = repo ?? DefaultRepo;
        _http = new HttpClient();
        _http.DefaultRequestHeaders.UserAgent.ParseAdd("FindX-UpdateChecker/1.0");
        _http.Timeout = TimeSpan.FromSeconds(15);
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

            var latestStr = release.TagName.TrimStart('v', 'V');
            var currentStr = GetCurrentVersion();

            bool hasUpdate = false;
            if (Version.TryParse(NormalizeSemver(latestStr), out var latest) &&
                Version.TryParse(NormalizeSemver(currentStr), out var current))
            {
                hasUpdate = latest > current;
            }

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
