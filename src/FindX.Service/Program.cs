using System.Threading;

namespace FindX.Service;

public static class Program
{
    /// <summary>防止多开：多实例均监听同名管道会导致 CLI 连到随机实例、索引不一致。</summary>
    private const string SingleInstanceMutexName = @"Global\FindX.Service.SingleInstance";

    [STAThread]
    public static void Main(string[] args)
    {
        using var mutex = new Mutex(true, SingleInstanceMutexName, out var createdNew);
        if (!createdNew)
        {
            Console.Error.WriteLine(
                "FindX 已在运行，或无法获取单实例锁。请先退出托盘中的 FindX 或结束 FindX 进程后再启动。");
            Environment.Exit(10);
            return;
        }

        var host = new ServiceHost();
        host.Run(args);
    }
}
