namespace FindX.Core.Diagnostics;

/// <summary>
/// 可选搜索性能诊断输出。由 FindX.Service 在启动时挂接到立即落盘的日志。
/// </summary>
public static class SearchPerfLog
{
    public static Action<string>? WriteLine;
}
