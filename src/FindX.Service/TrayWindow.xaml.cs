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
            _notifyIcon.Icon = Icon.ExtractAssociatedIcon(Environment.ProcessPath!);
        }
        catch
        {
            _notifyIcon.Icon = SystemIcons.Application;
        }

        _updateMenuItem = new WinForms.ToolStripMenuItem("检查更新");
        _updateMenuItem.Click += async (_, _) => await CheckUpdateFromMenu();

        var menu = new WinForms.ContextMenuStrip();
        menu.Items.Add("搜索", null, (_, _) => ShowSearchWindow());
        menu.Items.Add("显示状态", null, (_, _) => ShowWindow());
        menu.Items.Add("重建索引", null, (_, _) => _host.SetAutoStart(true));
        menu.Items.Add(_updateMenuItem);
        menu.Items.Add(new WinForms.ToolStripSeparator());
        menu.Items.Add("退出", null, (_, _) => ExitApp());
        _notifyIcon.ContextMenuStrip = menu;
        _notifyIcon.DoubleClick += (_, _) => ShowSearchWindow();

        _refreshTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(2) };
        _refreshTimer.Tick += (_, _) => RefreshStatus();
        _refreshTimer.Start();

        AutoStartCheck.IsChecked = _host.IsAutoStartEnabled();
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
                _notifyIcon.ShowBalloonTip(5000, "FindX 更新可用",
                    $"新版本 v{updateInfo.LatestVersion} 已发布，点击托盘菜单查看详情。",
                    WinForms.ToolTipIcon.Info);
            }
        }
        else
        {
            UpdatePanel.Visibility = Visibility.Collapsed;
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
                _notifyIcon.ShowBalloonTip(5000, "FindX 更新可用",
                    $"新版本 v{info.LatestVersion} 已发布！", WinForms.ToolTipIcon.Info);
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
                    StatusText.Text = $"状态: 发现新版本 v{info.LatestVersion}";
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
            _searchWindow = new SearchWindow(_host);

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

    private void Reindex_Click(object sender, RoutedEventArgs e)
    {
        StatusText.Text = "状态: 正在重建索引...";
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
