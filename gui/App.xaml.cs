using System.Windows;
using System.Windows.Threading;

namespace HermesFix;

public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        DispatcherUnhandledException += OnUnhandled;
    }

    private void OnUnhandled(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        MessageBox.Show(
            "程序出现未预期的错误：\n\n" + e.Exception.Message +
            "\n\n如果正在签名，请拔插数据线后重试。",
            "爱马仕表盘助手",
            MessageBoxButton.OK,
            MessageBoxImage.Error);
        e.Handled = true;
    }
}
