using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using Media = System.Windows.Media;
using System.Windows.Threading;
using FindX.Core.Search;
using SaveFileDialog = Microsoft.Win32.SaveFileDialog;

namespace FindX.Service;

// TODO(IbEverythingExt 可借): 结果列表快速选择（如 0-9/A-Z 选中、Alt+数字打开）与关闭/定位热键，降低纯键盘操作成本。
public partial class SearchWindow : Window
{
    private readonly ServiceHost _host;
    private readonly Action _openSettings;
    private readonly DispatcherTimer _debounce;
    private string _currentTypeFilter = "";
    private string _currentTimeFilter = "";
    private bool _initialized;
    private CancellationTokenSource? _searchCts;
    private int _searchVersion;
    private string? _sortColumn;
    private bool _sortAsc = true;
    private List<ResultItem> _currentItems = new();

    private static readonly string HistoryPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "FindX", "search_history.json");
    private List<string> _searchHistory = new();
    private const int MaxHistory = 30;

    public SearchWindow(ServiceHost host, Action openSettings)
    {
        _host = host;
        _openSettings = openSettings;
        InitializeComponent();
        _initialized = true;

        _debounce = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(200) };
        _debounce.Tick += (_, _) =>
        {
            _debounce.Stop();
            DoSearch();
        };

        LoadHistory();
        UpdateIndexStatus();
    }

    private void SearchBox_TextChanged(object sender, System.Windows.Controls.TextChangedEventArgs e)
    {
        SearchPlaceholder.Visibility = string.IsNullOrEmpty(SearchBox.Text)
            ? Visibility.Visible : Visibility.Collapsed;

        _debounce.Stop();
        _debounce.Start();
    }

    private void SearchBox_KeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key == System.Windows.Input.Key.Enter)
        {
            _debounce.Stop();
            DoSearch();
        }
        else if (e.Key == System.Windows.Input.Key.Escape)
        {
            if (string.IsNullOrEmpty(SearchBox.Text))
                Hide();
            else
                SearchBox.Clear();
        }
    }

    private void SearchBtn_Click(object sender, RoutedEventArgs e) => DoSearch();

    private void SettingsBtn_Click(object sender, RoutedEventArgs e)
    {
        _openSettings();
    }

    private void TypeFilter_Changed(object sender, RoutedEventArgs e)
    {
        if (!_initialized) return;
        _currentTypeFilter = sender switch
        {
            var r when r == FilterFolder => "folder:",
            var r when r == FilterFile => "file:",
            var r when r == FilterDoc => "ext:doc;docx;pdf;xls;xlsx;ppt;pptx;txt;rtf;odt;csv;md",
            var r when r == FilterImage => "ext:jpg;jpeg;png;gif;bmp;svg;webp;ico;tiff;psd;raw",
            var r when r == FilterVideo => "ext:mp4;avi;mkv;mov;wmv;flv;webm;m4v;ts",
            var r when r == FilterAudio => "ext:mp3;wav;flac;aac;ogg;wma;m4a;opus",
            _ => "",
        };
        _debounce.Stop();
        DoSearch();
    }

    private void TimeFilter_Changed(object sender, RoutedEventArgs e)
    {
        if (!_initialized) return;
        if (sender == Time1D)
            _currentTimeFilter = $"dm:>{DateTime.Today.AddDays(-1):yyyy-MM-dd}";
        else if (sender == Time7D)
            _currentTimeFilter = $"dm:>{DateTime.Today.AddDays(-7):yyyy-MM-dd}";
        else if (sender == Time30D)
            _currentTimeFilter = $"dm:>{DateTime.Today.AddDays(-30):yyyy-MM-dd}";
        else if (sender == Time365D)
            _currentTimeFilter = $"dm:>{DateTime.Today.AddDays(-365):yyyy-MM-dd}";
        else
            _currentTimeFilter = "";

        _debounce.Stop();
        DoSearch();
    }

    private void SidebarFilter_KeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key == System.Windows.Input.Key.Enter)
        {
            _debounce.Stop();
            DoSearch();
        }
    }

    private void SidebarFilter_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (!_initialized)
            return;
        _debounce.Stop();
        _debounce.Start();
    }

    private string BuildSidebarFilters()
    {
        var parts = new List<string>();
        var pathText = PathFilterBox.Text.Trim();
        if (!string.IsNullOrEmpty(pathText))
            parts.Add($"path:\"{pathText}\"");

        var sizeMin = SizeMinBox.Text.Trim();
        var sizeMax = SizeMaxBox.Text.Trim();
        if (!string.IsNullOrEmpty(sizeMin) && !string.IsNullOrEmpty(sizeMax))
            parts.Add($"size:{sizeMin}mb..{sizeMax}mb");
        else if (!string.IsNullOrEmpty(sizeMin))
            parts.Add($"size:>={sizeMin}mb");
        else if (!string.IsNullOrEmpty(sizeMax))
            parts.Add($"size:<={sizeMax}mb");

        return string.Join(" ", parts);
    }

    private async void DoSearch()
    {
        _searchCts?.Cancel();

        var raw = SearchBox.Text.Trim();
        var sidebarFilters = BuildSidebarFilters();
        if (string.IsNullOrEmpty(raw) && string.IsNullOrEmpty(_currentTypeFilter)
            && string.IsNullOrEmpty(_currentTimeFilter) && string.IsNullOrEmpty(sidebarFilters))
        {
            ResultsList.ItemsSource = null;
            _currentItems.Clear();
            StatusText.Text = "就绪";
            UpdateIndexStatus();
            return;
        }

        if (_host.IsIndexInBulkLoad)
        {
            ResultsList.ItemsSource = null;
            _currentItems.Clear();
            StatusText.Text = "索引构建中，请稍候…";
            UpdateIndexStatus();
            return;
        }

        var parts = new List<string>();
        if (!string.IsNullOrEmpty(raw)) parts.Add(raw);
        if (!string.IsNullOrEmpty(_currentTypeFilter)) parts.Add(_currentTypeFilter);
        if (!string.IsNullOrEmpty(_currentTimeFilter)) parts.Add(_currentTimeFilter);
        if (!string.IsNullOrEmpty(sidebarFilters)) parts.Add(sidebarFilters);
        var query = string.Join(" ", parts);

        var ver = Interlocked.Increment(ref _searchVersion);
        var cts = _searchCts = new CancellationTokenSource();

        StatusText.Text = "搜索中…";
        SearchProgress.Visibility = Visibility.Visible;

        List<SearchResult> results;
        Stopwatch sw;
        try
        {
            sw = Stopwatch.StartNew();
            results = await Task.Run(() => _host.Search(query, 200), cts.Token);
            sw.Stop();
        }
        catch (OperationCanceledException)
        {
            SearchProgress.Visibility = Visibility.Collapsed;
            return;
        }
        catch
        {
            SearchProgress.Visibility = Visibility.Collapsed;
            if (ver != Volatile.Read(ref _searchVersion)) return;
            StatusText.Text = "搜索出错";
            return;
        }

        SearchProgress.Visibility = Visibility.Collapsed;
        if (ver != Volatile.Read(ref _searchVersion)) return;

        if (!string.IsNullOrEmpty(raw))
            AddHistory(raw);

        var items = new List<ResultItem>(results.Count);
        foreach (var r in results)
        {
            var nameParts = SearchHighlightBuilder.BuildNameParts(r.Name, raw);
            items.Add(new ResultItem
            {
                Name = r.Name,
                NameParts = nameParts,
                FullPath = r.FullPath,
                Path = TruncatePath(r.FullPath),
                IsDirectory = r.IsDirectory,
                Size = r.Size,
                SizeText = r.IsDirectory ? "" : FormatSize(r.Size),
                ModifiedText = r.LastWriteUtcTicks > 0
                    ? new DateTime(r.LastWriteUtcTicks, DateTimeKind.Utc).ToLocalTime().ToString("yyyy/M/d H:mm:ss")
                    : "",
                ModifiedTicks = r.LastWriteUtcTicks > 0
                    ? new DateTime(r.LastWriteUtcTicks, DateTimeKind.Utc).ToLocalTime().Ticks
                    : 0,
                Icon = GetIcon(r.Name, r.IsDirectory),
                IconColor = GetIconColor(r.Name, r.IsDirectory),
            });
        }

        _currentItems = items;
        ResultsList.ItemsSource = items;
        StatusText.Text = $"找到 {results.Count} 个结果（耗时 {sw.Elapsed.TotalMilliseconds:F1}ms）";
        UpdateIndexStatus();
    }

    private void ResultsList_DoubleClick(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        Ctx_Open(sender, e);
    }

    private void Ctx_Open(object sender, RoutedEventArgs e)
    {
        if (ResultsList.SelectedItem is not ResultItem item) return;
        try
        {
            Process.Start(new ProcessStartInfo(item.FullPath) { UseShellExecute = true });
        }
        catch { }
    }

    private void Ctx_OpenFolder(object sender, RoutedEventArgs e)
    {
        if (ResultsList.SelectedItem is not ResultItem item) return;
        try
        {
            Process.Start(new ProcessStartInfo("explorer.exe", $"/select,\"{item.FullPath}\"")
                { UseShellExecute = true });
        }
        catch { }
    }

    private void Ctx_CopyPath(object sender, RoutedEventArgs e)
    {
        if (ResultsList.SelectedItem is ResultItem item)
            System.Windows.Clipboard.SetText(item.FullPath);
    }

    private void Ctx_CopyName(object sender, RoutedEventArgs e)
    {
        if (ResultsList.SelectedItem is ResultItem item)
            System.Windows.Clipboard.SetText(item.Name);
    }

    private void Ctx_CopyAllPaths(object sender, RoutedEventArgs e)
    {
        if (_currentItems.Count == 0) return;
        var selected = ResultsList.SelectedItems.Cast<ResultItem>().ToList();
        var list = selected.Count > 1 ? selected : _currentItems;
        System.Windows.Clipboard.SetText(string.Join(Environment.NewLine, list.Select(i => i.FullPath)));
    }

    private void Ctx_ExportCsv(object sender, RoutedEventArgs e)
    {
        if (_currentItems.Count == 0) return;
        var dlg = new SaveFileDialog
        {
            Filter = "CSV 文件|*.csv",
            DefaultExt = ".csv",
            FileName = $"FindX_Export_{DateTime.Now:yyyyMMdd_HHmmss}.csv",
        };
        if (dlg.ShowDialog() != true) return;

        try
        {
            using var fs = new FileStream(dlg.FileName, FileMode.Create, FileAccess.Write, FileShare.Read);
            using var sw = new StreamWriter(fs, new UTF8Encoding(true));
            sw.WriteLine("名称,完整路径,大小,修改时间");
            foreach (var item in _currentItems)
            {
                var name = CsvEscape(item.Name);
                var path = CsvEscape(item.FullPath);
                sw.WriteLine($"{name},{path},{item.Size},{item.ModifiedText}");
            }
            StatusText.Text = $"已导出 {_currentItems.Count} 条至 {dlg.FileName}";
        }
        catch (Exception ex)
        {
            System.Windows.MessageBox.Show($"导出失败: {ex.Message}", "FindX", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private static string CsvEscape(string s)
    {
        if (s.Contains(',') || s.Contains('"') || s.Contains('\n'))
            return $"\"{s.Replace("\"", "\"\"")}\"";
        return s;
    }

    private void ColumnHeader_Click(object sender, RoutedEventArgs e)
    {
        if (e.OriginalSource is not GridViewColumnHeader header) return;
        var headerText = header.Content?.ToString();
        if (string.IsNullOrEmpty(headerText)) return;

        if (_sortColumn == headerText)
            _sortAsc = !_sortAsc;
        else
        {
            _sortColumn = headerText;
            _sortAsc = true;
        }

        var sorted = headerText switch
        {
            "名称" => _sortAsc
                ? _currentItems.OrderBy(x => x.Name, StringComparer.OrdinalIgnoreCase).ToList()
                : _currentItems.OrderByDescending(x => x.Name, StringComparer.OrdinalIgnoreCase).ToList(),
            "路径" => _sortAsc
                ? _currentItems.OrderBy(x => x.FullPath, StringComparer.OrdinalIgnoreCase).ToList()
                : _currentItems.OrderByDescending(x => x.FullPath, StringComparer.OrdinalIgnoreCase).ToList(),
            "大小" => _sortAsc
                ? _currentItems.OrderBy(x => x.Size).ToList()
                : _currentItems.OrderByDescending(x => x.Size).ToList(),
            "修改时间" => _sortAsc
                ? _currentItems.OrderBy(x => x.ModifiedTicks).ToList()
                : _currentItems.OrderByDescending(x => x.ModifiedTicks).ToList(),
            _ => _currentItems,
        };

        _currentItems = sorted;
        ResultsList.ItemsSource = sorted;
    }

    // --- Search history ---

    private void HistoryBtn_Click(object sender, RoutedEventArgs e)
    {
        if (_searchHistory.Count == 0) return;
        HistoryList.ItemsSource = _searchHistory;
        HistoryPopup.IsOpen = true;
    }

    private void HistoryList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (HistoryList.SelectedItem is string text)
        {
            SearchBox.Text = text;
            HistoryPopup.IsOpen = false;
            _debounce.Stop();
            DoSearch();
        }
    }

    private void AddHistory(string query)
    {
        _searchHistory.Remove(query);
        _searchHistory.Insert(0, query);
        if (_searchHistory.Count > MaxHistory)
            _searchHistory.RemoveRange(MaxHistory, _searchHistory.Count - MaxHistory);
        SaveHistory();
    }

    private void LoadHistory()
    {
        try
        {
            if (File.Exists(HistoryPath))
            {
                var json = File.ReadAllText(HistoryPath);
                _searchHistory = JsonSerializer.Deserialize<List<string>>(json) ?? new();
            }
        }
        catch { _searchHistory = new(); }
    }

    private void SaveHistory()
    {
        try
        {
            var dir = System.IO.Path.GetDirectoryName(HistoryPath);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);
            File.WriteAllText(HistoryPath, JsonSerializer.Serialize(_searchHistory));
        }
        catch { }
    }

    // --- Helpers ---

    private void UpdateIndexStatus()
    {
        IndexStatus.Text = _host.IndexBuildInProgress
            ? $"索引: {_host.IndexCount:N0}（建立中…）"
            : $"索引: {_host.IndexCount:N0}";
    }

    protected override void OnClosing(System.ComponentModel.CancelEventArgs e)
    {
        e.Cancel = true;
        Hide();
    }

    protected override void OnActivated(EventArgs e)
    {
        base.OnActivated(e);
        SearchBox.Focus();
    }

    private static string TruncatePath(string fullPath)
    {
        var dir = System.IO.Path.GetDirectoryName(fullPath);
        return dir != null && dir.Length > 70 ? dir[..35] + "..." + dir[^30..] : dir ?? fullPath;
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 0) return "";
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:F1} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):F1} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):F2} GB";
    }

    private static string GetIcon(string name, bool isDir)
    {
        if (isDir) return "\uE8B7";
        var ext = System.IO.Path.GetExtension(name).ToLowerInvariant();
        return ext switch
        {
            ".pdf" => "\uEA90",
            ".doc" or ".docx" or ".rtf" or ".odt" => "\uE8A5",
            ".xls" or ".xlsx" or ".csv" => "\uE80A",
            ".ppt" or ".pptx" => "\uE8A5",
            ".jpg" or ".jpeg" or ".png" or ".gif" or ".bmp"
                or ".svg" or ".webp" or ".ico" => "\uEB9F",
            ".mp4" or ".avi" or ".mkv" or ".mov" or ".wmv"
                or ".flv" or ".webm" => "\uE714",
            ".mp3" or ".wav" or ".flac" or ".aac"
                or ".ogg" or ".wma" => "\uE8D6",
            ".zip" or ".rar" or ".7z" or ".tar" or ".gz" => "\uF012",
            ".exe" or ".msi" => "\uE756",
            ".cs" or ".js" or ".ts" or ".py" or ".java"
                or ".cpp" or ".c" or ".h" or ".rs" => "\uE943",
            _ => "\uE7C3",
        };
    }

    private static Media.SolidColorBrush Rgb(byte r, byte g, byte b)
        => new(Media.Color.FromRgb(r, g, b));

    private static Media.SolidColorBrush GetIconColor(string name, bool isDir)
    {
        if (isDir) return Rgb(0xF0, 0xC0, 0x40);
        var ext = System.IO.Path.GetExtension(name).ToLowerInvariant();
        return ext switch
        {
            ".pdf" => Rgb(0xE0, 0x40, 0x40),
            ".doc" or ".docx" or ".rtf" => Rgb(0x26, 0x5C, 0xB0),
            ".xls" or ".xlsx" or ".csv" => Rgb(0x21, 0x7D, 0x46),
            ".ppt" or ".pptx" => Rgb(0xD0, 0x4A, 0x2E),
            ".jpg" or ".jpeg" or ".png" or ".gif" or ".bmp"
                or ".svg" or ".webp" => Rgb(0x00, 0x96, 0xD6),
            ".mp4" or ".avi" or ".mkv" or ".mov" => Rgb(0x8B, 0x5C, 0xF6),
            ".mp3" or ".wav" or ".flac" => Rgb(0xE9, 0x1E, 0x63),
            ".zip" or ".rar" or ".7z" => Rgb(0xFF, 0x98, 0x00),
            ".exe" or ".msi" => Rgb(0x00, 0x78, 0xD4),
            _ => Rgb(0x99, 0x99, 0x99),
        };
    }
}

public sealed class ResultItem
{
    public string Name { get; init; } = "";
    /// <summary>名称列分段高亮；无关键词时仅一段、IsHighlight=false。</summary>
    public IReadOnlyList<HighlightPart> NameParts { get; init; } =
        Array.Empty<HighlightPart>();
    public string FullPath { get; init; } = "";
    public string Path { get; init; } = "";
    public bool IsDirectory { get; init; }
    public long Size { get; init; }
    public string SizeText { get; init; } = "";
    public string ModifiedText { get; init; } = "";
    public long ModifiedTicks { get; init; }
    public string Icon { get; init; } = "";
    public Media.SolidColorBrush IconColor { get; init; } = Media.Brushes.Gray;
}
