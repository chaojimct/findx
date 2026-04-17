using System.Diagnostics;
using System.Windows;
using System.Windows.Threading;
using System.Drawing;
using WinForms = System.Windows.Forms;

namespace FindX.Service;

public partial class TrayWindow : Window
{
    private readonly ServiceHost _host;
    private readonly WinForms.NotifyIcon _notifyIcon;
    private readonly DispatcherTimer _refreshTimer;
    private readonly WinForms.ToolStripMenuItem _updateMenuItem;
    private SearchWindow? _searchWindow;
    private bool _updateNotified;
    /// <summary>本轮已弹过「是否升级」的版本号，避免定时器重复弹窗。</summary>
    private string? _autoUpgradeDialogVersionOffered;

    public TrayWindow(ServiceHost host)
    {
        _host = host;
        InitializeComponent();

        _notifyIcon = new WinForms.NotifyIcon
        {
            Text = "FindX 文件搜索引擎",
            Visible = true,
        };

        try
        {
            _notifyIcon.Icon = System.Drawing.Icon.ExtractAssociatedIcon(Environment.ProcessPath!);
        }
        catch
        {
            _notifyIcon.Icon = SystemIcons.Application;
        }

        _updateMenuItem = new WinForms.ToolStripMenuItem("检查更新");
        _updateMenuItem.Click += async (_, _) => await CheckUpdateFromMenu();

        var menu = new WinForms.ContextMenuStrip();
        menu.Items.Add("搜索", null, (_, _) => ShowSearchWindow());
        menu.Items.Add("设置", null, (_, _) => ShowWindow());
        menu.Items.Add("重建索引", null, (_, _) => _ = Task.Run(() => _host.ForceReindexAsync()));
        menu.Items.Add(_updateMenuItem);
        menu.Items.Add(new WinForms.ToolStripSeparator());
        menu.Items.Add("退出", null, (_, _) => ExitApp());
        _notifyIcon.ContextMenuStrip = menu;
        _notifyIcon.DoubleClick += (_, _) => ShowSearchWindow();

        _refreshTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(2) };
        _refreshTimer.Tick += (_, _) => RefreshStatus();
        _refreshTimer.Start();

        AutoStartCheck.IsChecked = _host.IsAutoStartEnabled();
        PreferPinyinSortCheck.IsChecked = _host.PreferPinyinForAsciiQueries;
        HydrateMetadataCheck.IsChecked = _host.HydrateSearchResultMetadata;
    }

    private void ShowWindow()
    {
        Show();
        WindowState = WindowState.Normal;
        Activate();
        RefreshStatus();
    }

    private void RefreshStatus()
    {
        CountText.Text = _host.IndexBuildInProgress
            ? $"索引文件数: {_host.IndexCount:N0}（建立中…）"
            : $"索引文件数: {_host.IndexCount:N0}";

        var updateInfo = _host.LatestUpdateInfo;
        if (updateInfo?.HasUpdate == true)
        {
            UpdatePanel.Visibility = Visibility.Visible;
            UpdateText.Text = $"新版本 v{updateInfo.LatestVersion} 可用（当前 v{updateInfo.CurrentVersion}）";

            if (!_updateNotified)
            {
                _updateNotified = true;
                _updateMenuItem.Text = $"检查更新 ✦ v{updateInfo.LatestVersion}";
            }

            if (_autoUpgradeDialogVersionOffered != updateInfo.LatestVersion)
            {
                var latest = updateInfo.LatestVersion;
                var current = updateInfo.CurrentVersion;
                _autoUpgradeDialogVersionOffered = latest;
                Dispatcher.BeginInvoke(() => ShowAutoUpgradeOfferDialog(latest, current),
                    System.Windows.Threading.DispatcherPriority.ApplicationIdle);
            }
        }
        else
        {
            UpdatePanel.Visibility = Visibility.Collapsed;
            if (updateInfo != null && !updateInfo.HasUpdate)
                _autoUpgradeDialogVersionOffered = null;
        }

        var logs = _host.RecentLogs;
        LogList.Items.Clear();
        foreach (var log in logs)
            LogList.Items.Add(log);
        if (LogList.Items.Count > 0)
            LogList.ScrollIntoView(LogList.Items[^1]);
    }

    private async Task CheckUpdateFromMenu()
    {
        _updateMenuItem.Enabled = false;
        _updateMenuItem.Text = "正在检查...";
        try
        {
            var info = await _host.CheckForUpdateAsync();
            if (info == null)
            {
                _notifyIcon.ShowBalloonTip(3000, "FindX", "检查更新失败，请稍后重试。", WinForms.ToolTipIcon.Warning);
            }
            else if (info.HasUpdate)
            {
                _autoUpgradeDialogVersionOffered = info.LatestVersion;
                await Dispatcher.InvokeAsync(() =>
                {
                    if (System.Windows.MessageBox.Show(
                            $"发现新版本 v{info.LatestVersion}（当前 v{info.CurrentVersion}）。\n\n是否下载安装包并打开安装向导升级？",
                            "FindX 更新",
                            MessageBoxButton.YesNo,
                            MessageBoxImage.Question)
                        != MessageBoxResult.Yes)
                        return;
                    _ = RunDownloadInstallCoreAsync();
                });
            }
            else
            {
                _notifyIcon.ShowBalloonTip(3000, "FindX", $"当前已是最新版本 (v{info.CurrentVersion})。", WinForms.ToolTipIcon.Info);
            }
        }
        finally
        {
            var updateInfo = _host.LatestUpdateInfo;
            _updateMenuItem.Text = updateInfo?.HasUpdate == true
                ? $"检查更新 ✦ v{updateInfo.LatestVersion}"
                : "检查更新";
            _updateMenuItem.Enabled = true;
        }
    }

    private void OpenReleasePage_Click(object sender, RoutedEventArgs e)
    {
        var url = _host.LatestUpdateInfo?.ReleaseUrl;
        if (!string.IsNullOrEmpty(url))
        {
            try { Process.Start(new ProcessStartInfo(url) { UseShellExecute = true }); }
            catch { }
        }
    }

    private void ShowAutoUpgradeOfferDialog(string latest, string current)
    {
        if (System.Windows.MessageBox.Show(
                $"发现新版本 v{latest}（当前 v{current}）。\n\n是否下载安装包并打开安装向导进行升级？",
                "FindX 更新",
                MessageBoxButton.YesNo,
                MessageBoxImage.Question)
            != MessageBoxResult.Yes)
            return;
        _ = RunDownloadInstallCoreAsync();
    }

    private async Task RunDownloadInstallCoreAsync()
    {
        DownloadInstallBtn.IsEnabled = false;
        StatusText.Text = "状态: 正在下载安装包…";
        try
        {
            var (ok, err) = await _host.TryDownloadAndApplyUpdateAsync();
            if (!ok && !string.IsNullOrEmpty(err))
            {
                System.Windows.MessageBox.Show(this, err, "FindX", MessageBoxButton.OK, MessageBoxImage.Error);
                DownloadInstallBtn.IsEnabled = true;
                StatusText.Text = "状态: 运行中";
            }
        }
        catch (Exception ex)
        {
            System.Windows.MessageBox.Show(this, ex.Message, "FindX", MessageBoxButton.OK, MessageBoxImage.Error);
            DownloadInstallBtn.IsEnabled = true;
            StatusText.Text = "状态: 运行中";
        }
    }

    private async void DownloadAndInstall_Click(object sender, RoutedEventArgs e)
    {
        var info = _host.LatestUpdateInfo;
        if (info?.HasUpdate != true || string.IsNullOrEmpty(info.DownloadUrl))
        {
            StatusText.Text = "状态: 正在检查更新…";
            info = await _host.CheckForUpdateAsync();
            RefreshStatus();
        }

        if (info?.HasUpdate != true)
        {
            System.Windows.MessageBox.Show(this, "当前已是最新版本。", "FindX", MessageBoxButton.OK, MessageBoxImage.Information);
            StatusText.Text = "状态: 运行中";
            return;
        }

        if (string.IsNullOrEmpty(info.DownloadUrl))
        {
            System.Windows.MessageBox.Show(this, "发布中未包含 setup 安装包，请使用「发布页」在浏览器中下载。", "FindX",
                MessageBoxButton.OK, MessageBoxImage.Warning);
            StatusText.Text = "状态: 运行中";
            return;
        }

        if (System.Windows.MessageBox.Show(this,
                $"将下载 v{info.LatestVersion}，并启动安装向导（可按提示完成升级，可能出现 UAC）。下载完成后 FindX 会在约一秒后退出。是否继续？",
                "FindX 更新",
                MessageBoxButton.YesNo,
                MessageBoxImage.Question)
            != MessageBoxResult.Yes)
        {
            StatusText.Text = "状态: 运行中";
            return;
        }

        await RunDownloadInstallCoreAsync();
    }

    private async void CheckUpdate_Click(object sender, RoutedEventArgs e)
    {
        CheckUpdateBtn.IsEnabled = false;
        CheckUpdateBtn.Content = "检查中...";
        try
        {
            var info = await _host.CheckForUpdateAsync();
            Dispatcher.Invoke(() =>
            {
                if (info == null)
                    StatusText.Text = "状态: 检查更新失败";
                else if (info.HasUpdate)
                {
                    StatusText.Text = $"状态: 发现新版本 v{info.LatestVersion}";
                    _autoUpgradeDialogVersionOffered = info.LatestVersion;
                    if (System.Windows.MessageBox.Show(
                            $"发现新版本 v{info.LatestVersion}（当前 v{info.CurrentVersion}）。\n\n是否下载安装包并打开安装向导升级？",
                            "FindX 更新",
                            MessageBoxButton.YesNo,
                            MessageBoxImage.Question)
                        == MessageBoxResult.Yes)
                        _ = RunDownloadInstallCoreAsync();
                }
                else
                    StatusText.Text = $"状态: 已是最新版本 (v{info.CurrentVersion})";
            });
        }
        finally
        {
            Dispatcher.Invoke(() =>
            {
                CheckUpdateBtn.Content = "检查更新";
                CheckUpdateBtn.IsEnabled = true;
            });
        }
    }

    private void ShowSearchWindow()
    {
        if (_searchWindow == null || !_searchWindow.IsLoaded)
            _searchWindow = new SearchWindow(_host, ShowWindow);

        if (_searchWindow.IsVisible)
        {
            _searchWindow.Activate();
        }
        else
        {
            _searchWindow.Show();
            _searchWindow.Activate();
        }
    }

    private void AutoStart_Changed(object sender, RoutedEventArgs e)
    {
        _host.SetAutoStart(AutoStartCheck.IsChecked == true);
    }

    private void PreferPinyinSort_Changed(object sender, RoutedEventArgs e)
    {
        _host.SetPreferPinyinForAsciiQueries(PreferPinyinSortCheck.IsChecked == true);
    }

    private void HydrateMetadata_Changed(object sender, RoutedEventArgs e)
    {
        _host.SetHydrateSearchResultMetadata(HydrateMetadataCheck.IsChecked == true);
    }

    private async void Reindex_Click(object sender, RoutedEventArgs e)
    {
        if (_host.IndexBuildInProgress)
        {
            StatusText.Text = "状态: 索引构建中，请稍候...";
            return;
        }
        StatusText.Text = "状态: 正在重建索引...";
        await Task.Run(() => _host.ForceReindexAsync());
        StatusText.Text = $"状态: 重建完成，索引 {_host.IndexCount:N0} 条";
    }

    private void Hide_Click(object sender, RoutedEventArgs e) => Hide();

    private void Exit_Click(object sender, RoutedEventArgs e) => ExitApp();

    private void ExitApp()
    {
        _searchWindow?.Close();
        _notifyIcon.Visible = false;
        _notifyIcon.Dispose();
        _refreshTimer.Stop();
        _host.RequestShutdown();
    }

    protected override void OnClosing(System.ComponentModel.CancelEventArgs e)
    {
        e.Cancel = true;
        Hide();
    }
}
