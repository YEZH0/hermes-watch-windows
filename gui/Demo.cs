using System.Diagnostics;
using System.IO;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;

namespace HermesFix;

/// <summary>
/// 无头自测钩子。只在设置了 HF_SHOT / HF_DEMO 环境变量时生效，正常使用完全不受影响。
/// 用途：不接真机、不碰 Apple ID，也能验证「解析输出 → 弹 2FA → 回传 → 收尾」整条链路。
/// </summary>
public partial class MainWindow
{
    private void HandleTestHooks()
    {
        var shot = Environment.GetEnvironmentVariable("HF_SHOT");
        var demo = Environment.GetEnvironmentVariable("HF_DEMO") == "1";
        var engine = Environment.GetEnvironmentVariable("HF_ENGINE");
        var ipa = Environment.GetEnvironmentVariable("HF_IPA");

        if (!string.IsNullOrEmpty(engine))
        {
            _engine = engine;
            UpdateEngineText();
        }
        if (!string.IsNullOrEmpty(ipa)) IpaBox.Text = ipa;

        // HF_CHECK=1：启动后自动弹出环境自检窗口，用于无头验证它的渲染
        if (Environment.GetEnvironmentVariable("HF_CHECK") == "1")
            _ = Task.Delay(1200).ContinueWith(_ => Dispatcher.BeginInvoke(new Action(OpenSelfCheck)));

        if (string.IsNullOrEmpty(shot) && !demo) return;

        Dispatcher.BeginInvoke(
            async () => await RunTestAsync(demo, shot),
            DispatcherPriority.ApplicationIdle);
    }

    private async Task RunTestAsync(bool demo, string? shot)
    {
        await Task.Delay(500);

        if (!demo)
        {
            if (!string.IsNullOrEmpty(shot)) SaveShot(shot);
            Application.Current.Shutdown();
            return;
        }

        // 上一次自测可能留下了「手表已解锁」标记，先清掉，从故障状态开始。
        var engineDir = Path.GetDirectoryName(_engine);
        if (!string.IsNullOrEmpty(engineDir))
        {
            try { File.Delete(Path.Combine(engineDir, "probe-ready.flag")); } catch { }
        }

        EmailBox.Text = "demo@example.com";
        PwdBox.Password = "demo-password";
        Start_Click(this, new RoutedEventArgs());

        if (!await WaitAsync(() => TwoFaBar.Visibility == Visibility.Visible, 20000))
        {
            AppendLog("\r\n» [自测] 没有等到 2FA 提示\r\n");
            if (!string.IsNullOrEmpty(shot)) SaveShot(shot + ".fail.png");
            Application.Current.Shutdown();
            return;
        }

        await Task.Delay(350);
        if (!string.IsNullOrEmpty(shot)) SaveShot(shot + ".2fa.png");

        TwoFaInput.Text = "123456";
        // 单独留一张"已输入、未提交"的图：验证码打进去看不见这类问题，
        // 只有在这个瞬间截图才露得出来。
        if (!string.IsNullOrEmpty(shot))
        {
            await Task.Delay(250);
            SaveShot(shot + ".typed.png");
        }
        SubmitCurrent();

        if (!await WaitAsync(() => !_running, 30000))
        {
            AppendLog("\r\n» [自测] 引擎没有按时结束\r\n");
            if (!string.IsNullOrEmpty(shot)) SaveShot(shot + ".hang.png");
            Application.Current.Shutdown();
            return;
        }

        await Task.Delay(350);
        if (!string.IsNullOrEmpty(shot))
        {
            SaveShot(shot + ".done.png");
            DumpDiag(shot + ".diag.txt");
        }

        // HF_DEMO_RETRY=1：手表阶段失败后，模拟「用户把手表解锁了」，
        // 走一遍「重新检查 → 继续安装到手表」，验证续装链路。
        if (Environment.GetEnvironmentVariable("HF_DEMO_RETRY") == "1")
        {
            await RunRetryAsync(shot);
        }

        Application.Current.Shutdown();
    }

    private async Task RunRetryAsync(string? shot)
    {
        var engineDir = Path.GetDirectoryName(_engine);
        if (string.IsNullOrEmpty(engineDir)) return;
        var flag = Path.Combine(engineDir, "probe-ready.flag");

        try
        {
            // 这个标记由测试替身自己读：文件在，就当作手表已经解锁。
            File.WriteAllText(flag, "unlocked");
            AppendLog("\r\n» [自测] 模拟已解锁手表，点「我已处理，重新检查」\r\n");

            Recheck_Click(this, new RoutedEventArgs());
            if (!await WaitAsync(() => ResumeBtn.IsEnabled, 25000))
            {
                AppendLog("» [自测] 重新检查没有通过\r\n");
                if (!string.IsNullOrEmpty(shot)) SaveShot(shot + ".recheck-fail.png");
                return;
            }

            await Task.Delay(250);
            if (!string.IsNullOrEmpty(shot)) SaveShot(shot + ".recheck.png");

            AppendLog("» [自测] 点「继续安装到手表」\r\n");
            ResumeWatch_Click(this, new RoutedEventArgs());
            if (!await WaitAsync(() => !_running && _engineDone, 30000))
            {
                AppendLog("» [自测] 续装没有按时结束\r\n");
                if (!string.IsNullOrEmpty(shot)) SaveShot(shot + ".resume-fail.png");
                return;
            }

            await Task.Delay(350);
            if (!string.IsNullOrEmpty(shot))
            {
                SaveShot(shot + ".resumed.png");
                DumpDiag(shot + ".retry-diag.txt");
            }
        }
        finally
        {
            // 这个标记会把测试替身切到"永远成功"，留着会污染后续的无头测试。
            try { File.Delete(flag); } catch { }
        }
    }

    private void DumpDiag(string path)
    {
        if (_diag == null) return;
        try { File.WriteAllText(path, _diag.ToString()); } catch { }
    }

    private static async Task<bool> WaitAsync(Func<bool> condition, int timeoutMs)
    {
        var sw = Stopwatch.StartNew();
        while (sw.ElapsedMilliseconds < timeoutMs)
        {
            if (condition()) return true;
            await Task.Delay(100);
        }
        return condition();
    }

    private void SaveShot(string path)
    {
        try
        {
            UpdateLayout();
            var w = Math.Max(1, (int)Math.Ceiling(ActualWidth));
            var h = Math.Max(1, (int)Math.Ceiling(ActualHeight));
            var bmp = new RenderTargetBitmap(w, h, 96, 96, PixelFormats.Pbgra32);
            bmp.Render(this);

            var encoder = new PngBitmapEncoder();
            encoder.Frames.Add(BitmapFrame.Create(bmp));

            var full = Path.GetFullPath(path);
            var dir = Path.GetDirectoryName(full);
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);
            using var fs = File.Create(full);
            encoder.Save(fs);
        }
        catch (Exception ex)
        {
            AppendLog($"\r\n» [自测] 截图失败：{ex.Message}\r\n");
        }
    }
}
