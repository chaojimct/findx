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
            _notifyIcon.Icon = SystemIcons.Application;
        }
        catch { }

        var menu = new WinForms.ContextMenuStrip();
        menu.Items.Add("显示状态", null, (_, _) => ShowWindow());
        menu.Items.Add("重建索引", null, (_, _) => _host.SetAutoStart(true));
        menu.Items.Add(new WinForms.ToolStripSeparator());
        menu.Items.Add("退出", null, (_, _) => ExitApp());
        _notifyIcon.ContextMenuStrip = menu;
        _notifyIcon.DoubleClick += (_, _) => ShowWindow();

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

        var logs = _host.RecentLogs;
        LogList.Items.Clear();
        foreach (var log in logs)
            LogList.Items.Add(log);
        if (LogList.Items.Count > 0)
            LogList.ScrollIntoView(LogList.Items[^1]);
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
