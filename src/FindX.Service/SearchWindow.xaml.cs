using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Windows;
using Media = System.Windows.Media;
using System.Windows.Threading;
using FindX.Core.Search;

namespace FindX.Service;

public partial class SearchWindow : Window
{
    private readonly ServiceHost _host;
    private readonly Action _openSettings;
    private readonly DispatcherTimer _debounce;
    private string _currentTypeFilter = "";
    private string _currentTimeFilter = "";
    private bool _initialized;

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

    private void DoSearch()
    {
        var raw = SearchBox.Text.Trim();
        if (string.IsNullOrEmpty(raw) && string.IsNullOrEmpty(_currentTypeFilter) && string.IsNullOrEmpty(_currentTimeFilter))
        {
            ResultsList.ItemsSource = null;
            StatusText.Text = "就绪";
            return;
        }

        var parts = new List<string>();
        if (!string.IsNullOrEmpty(raw)) parts.Add(raw);
        if (!string.IsNullOrEmpty(_currentTypeFilter)) parts.Add(_currentTypeFilter);
        if (!string.IsNullOrEmpty(_currentTimeFilter)) parts.Add(_currentTimeFilter);
        var query = string.Join(" ", parts);

        var sw = Stopwatch.StartNew();
        List<SearchResult> results;
        try
        {
            results = _host.Search(query, 200);
        }
        catch
        {
            StatusText.Text = "搜索出错";
            return;
        }
        sw.Stop();

        var items = new List<ResultItem>(results.Count);
        foreach (var r in results)
        {
            items.Add(new ResultItem
            {
                Name = r.Name,
                FullPath = r.FullPath,
                Path = TruncatePath(r.FullPath),
                IsDirectory = r.IsDirectory,
                Size = r.Size,
                ModifiedText = r.LastModified > DateTime.MinValue
                    ? r.LastModified.ToString("yyyy/M/d h:mm:ss tt")
                    : "",
                Icon = GetIcon(r.Name, r.IsDirectory),
                IconColor = GetIconColor(r.Name, r.IsDirectory),
            });
        }

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
        if (ResultsList.ItemsSource is not List<ResultItem> items || items.Count == 0) return;
        var selected = ResultsList.SelectedItems.Cast<ResultItem>().ToList();
        var list = selected.Count > 1 ? selected : items;
        System.Windows.Clipboard.SetText(string.Join(Environment.NewLine, list.Select(i => i.FullPath)));
    }

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
    public string FullPath { get; init; } = "";
    public string Path { get; init; } = "";
    public bool IsDirectory { get; init; }
    public long Size { get; init; }
    public string ModifiedText { get; init; } = "";
    public string Icon { get; init; } = "";
    public Media.SolidColorBrush IconColor { get; init; } = Media.Brushes.Gray;
}
