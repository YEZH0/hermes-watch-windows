using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using Microsoft.Win32;

namespace HermesFix;

public partial class MainWindow : Window
{
    private enum PromptKind { None, Code, Email, Password }

    private static readonly string[] CodeMarkers = { "Two-factor code: ", "Apple ID (email): ", "Apple ID password: " };

    private Process? _proc;
    private StreamWriter? _stdin;
    private readonly StringBuilder _pending = new();
    private readonly StringBuilder _logAll = new();
    private readonly StringBuilder _rawErr = new();   // 脱敏前的 stderr，只用来读号码列表
    private bool _running;
    private bool _userStopped;
    private int _activeStep;
    private int _scanPos;          // _logAll 里已经扫描过的位置，只往前推
    private PromptKind _awaiting = PromptKind.None;
    private string _engine = "";
    private readonly StringBuilder? _diag =
        Environment.GetEnvironmentVariable("HF_DIAG") is null ? null : new StringBuilder();

    public MainWindow()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Closing += OnClosing;
    }

    protected override void OnSourceInitialized(EventArgs e)
    {
        base.OnSourceInitialized(e);
        // 让标题栏跟随深色界面；系统不支持时静默忽略。
        try
        {
            var hwnd = new System.Windows.Interop.WindowInteropHelper(this).Handle;
            int on = 1;
            DwmSetWindowAttribute(hwnd, 20, ref on, sizeof(int));
        }
        catch { }
    }

    [System.Runtime.InteropServices.DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

    // ---------------------------------------------------------------- 启动

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        ClampToScreen();

        var st = Settings.Load();
        RememberEmail.IsChecked = st.RememberEmail;
        if (st.RememberEmail && !string.IsNullOrWhiteSpace(st.Email))
            EmailBox.Text = st.Email;

        _engine = ResolveEngine(st.EnginePath);
        UpdateEngineText();

        IpaBox.Text = ResolveIpa(st.LastIpa);
        UpdateIpaHint();

        AppendLog("就绪。\r\n");
        AppendLog("引擎优先复用 iLoader 留在凭据管理器里的证书；找不到也会自己申请一张（免费账号共 3 张额度）。\r\n");
        if (!File.Exists(_engine))
            AppendLog("警告：没有找到签名引擎，点底部「环境自检」看原因。\r\n");

        HandleTestHooks();
        await RunSelfCheckAsync(auto: true);
    }

    /// <summary>自检。auto=true 时只在"拦路"的问题上自动弹窗，避免每次启动都打扰。</summary>
    private async Task RunSelfCheckAsync(bool auto)
    {
        try
        {
            var items = await EnvCheck.RunAllAsync(IpaBox.Text.Trim());
            int fail = items.Count(i => i.State == CheckState.Fail);
            int warn = items.Count(i => i.State == CheckState.Warn);

            AppendLog(fail > 0
                ? $"[自检] {fail} 项未通过，点底部「环境自检」看详情。\r\n"
                : warn > 0
                    ? $"[自检] 必需项通过，另有 {warn} 项提醒。\r\n"
                    : "[自检] 环境全部通过。\r\n");

            // 运行时/引擎都不对的话，签名必然失败，直接弹出来
            bool blocking = items.Take(2).Any(i => i.State == CheckState.Fail);
            if (auto && blocking) OpenSelfCheck();
        }
        catch (Exception ex)
        {
            AppendLog($"[自检] 出错：{ex.Message}\r\n");
        }
    }

    private void OpenSelfCheck()
    {
        var win = new CheckWindow(IpaBox.Text.Trim()) { Owner = this };
        win.ShowDialog();
    }

    private void SelfCheck_Click(object sender, RoutedEventArgs e) => OpenSelfCheck();

    /// <summary>
    /// 导出当前日志。内容在 AppendLog 时就已经脱敏了，这里只补一段表头说明。
    /// 只落本地文件，不上传任何地方。
    /// </summary>
    private void ExportLog_Click(object sender, RoutedEventArgs e)
    {
        var dlg = new SaveFileDialog
        {
            Title = "导出脱敏日志",
            FileName = $"hermes-log-{DateTime.Now:yyyyMMdd-HHmm}.txt",
            Filter = "文本文件|*.txt|所有文件|*.*",
            DefaultExt = ".txt",
        };
        if (dlg.ShowDialog(this) != true) return;

        try
        {
            var head =
                "HermesFixGui 日志（已脱敏）\r\n" +
                $"导出时间：{DateTime.Now:yyyy-MM-dd HH:mm:ss}\r\n" +
                "已自动掩码：Apple ID 邮箱、受信任手机号、Windows 用户名。\r\n" +
                "发出去之前仍建议自己扫一眼。\r\n" +
                new string('-', 58) + "\r\n\r\n";

            File.WriteAllText(dlg.FileName, head + _logAll.ToString(), new UTF8Encoding(true));
            SetStatus("日志已导出", "#3FB950");
        }
        catch (Exception ex)
        {
            Warn("导出失败：" + ex.Message);
        }
    }

    private void OnClosing(object? sender, System.ComponentModel.CancelEventArgs e)
    {
        SaveSettings();
        KillEngine();
    }

    /// <summary>小屏幕上别让窗口比工作区还大，否则底部按钮点不到。</summary>
    private void ClampToScreen()
    {
        var wa = SystemParameters.WorkArea;
        var maxW = Math.Max(MinWidth, wa.Width - 40);
        var maxH = Math.Max(MinHeight, wa.Height - 40);
        if (Width > maxW) Width = maxW;
        if (Height > maxH) Height = maxH;
        if (Left + Width > wa.Right) Left = Math.Max(wa.Left, wa.Right - Width);
        if (Top + Height > wa.Bottom) Top = Math.Max(wa.Top, wa.Bottom - Height);
    }

    private string ResolveEngine(string? remembered)
    {
        // 手动换过引擎就尊重用户的选择
        if (!string.IsNullOrWhiteSpace(remembered) && File.Exists(remembered))
            return remembered;

        // 内嵌引擎优先：这才是"只需一个 exe"的来源
        var embedded = Engine.Resolve();
        if (embedded is not null && File.Exists(embedded))
            return embedded;

        return Path.Combine(AppContext.BaseDirectory, "hermesfix.exe");
    }

    private static string ResolveIpa(string? remembered)
    {
        if (!string.IsNullOrWhiteSpace(remembered) && File.Exists(remembered))
            return remembered;

        var dir = AppContext.BaseDirectory;
        var probe = new List<string> { Path.Combine(dir, "HermesEnabler.ipa") };
        for (int up = 1; up <= 6; up++)
        {
            var root = dir;
            for (int i = 0; i < up; i++) root = Path.GetFullPath(Path.Combine(root, ".."));
            probe.Add(Path.Combine(root, "HermesEnabler.ipa"));
            probe.Add(Path.Combine(root, "ipa", "HermesEnabler.ipa"));
        }
        var downloads = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads");
        probe.Add(Path.Combine(downloads, "HermesEnabler.ipa"));

        return probe.FirstOrDefault(File.Exists) ?? "";
    }

    private void UpdateEngineText()
    {
        var exists = File.Exists(_engine);
        EngineText.Text = Engine.Describe(_engine);
        // 外部引擎一律给警示色：它可能是旧版，也可能不是本仓库的版本。
        EngineText.Foreground = !exists ? Hex("#F85149")
            : Engine.IsEmbeddedCopy(_engine) ? Hex("#7C8595")
            : Hex("#E3B341");
        EngineText.ToolTip = exists ? _engine : null;   // 路径放悬浮提示里，底栏没那么宽
    }

    // ---------------------------------------------------------------- 表单

    private void BrowseIpa_Click(object sender, RoutedEventArgs e)
    {
        var dlg = new OpenFileDialog
        {
            Title = "选择 HermesEnabler.ipa",
            Filter = "iOS 应用包 (*.ipa)|*.ipa|所有文件 (*.*)|*.*",
        };
        if (dlg.ShowDialog(this) == true) IpaBox.Text = dlg.FileName;
    }

    private void IpaBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (IpaHint is not null) UpdateIpaHint();
    }

    private void UpdateIpaHint()
    {
        var path = IpaBox.Text.Trim();
        if (string.IsNullOrEmpty(path))
        {
            IpaHint.Text = "可以直接把 .ipa 拖进这个窗口";
            IpaHint.Foreground = Hex("#8A94A6");
            return;
        }
        if (!File.Exists(path))
        {
            IpaHint.Text = "文件不存在";
            IpaHint.Foreground = Hex("#F85149");
            return;
        }
        var mb = new FileInfo(path).Length / 1024.0 / 1024.0;
        IpaHint.Text = $"{Path.GetFileName(path)} · {mb:F1} MB";
        IpaHint.Foreground = Hex("#8A94A6");
    }

    private void RememberEmail_Changed(object sender, RoutedEventArgs e)
    {
        if (!IsLoaded) return;
        if (RememberEmail.IsChecked == true) SaveSettings();
    }

    private void Window_DragOver(object sender, DragEventArgs e)
    {
        e.Effects = e.Data.GetDataPresent(DataFormats.FileDrop) ? DragDropEffects.Copy : DragDropEffects.None;
        e.Handled = true;
    }

    private void Window_Drop(object sender, DragEventArgs e)
    {
        if (!e.Data.GetDataPresent(DataFormats.FileDrop)) return;
        if (e.Data.GetData(DataFormats.FileDrop) is not string[] files) return;
        var ipa = files.FirstOrDefault(f => f.EndsWith(".ipa", StringComparison.OrdinalIgnoreCase));
        if (ipa != null) IpaBox.Text = ipa;
    }

    private void ChangeEngine_Click(object sender, RoutedEventArgs e)
    {
        var dlg = new OpenFileDialog
        {
            Title = "选择 hermesfix.exe",
            Filter = "可执行文件 (*.exe)|*.exe",
            FileName = File.Exists(_engine) ? Path.GetFileName(_engine) : "hermesfix.exe",
        };
        if (dlg.ShowDialog(this) != true) return;
        _engine = dlg.FileName;
        UpdateEngineText();
        SaveSettings();
        AppendLog($"» 签名引擎已切换为 {_engine}\r\n");
    }

    private void OpenFolder_Click(object sender, RoutedEventArgs e)
    {
        var dir = File.Exists(_engine) ? Path.GetDirectoryName(_engine)! : AppContext.BaseDirectory;
        try { Process.Start(new ProcessStartInfo("explorer.exe", $"\"{dir}\"") { UseShellExecute = true }); }
        catch (Exception ex) { AppendLog($"» 无法打开目录：{ex.Message}\r\n"); }
    }

    private void ClearLog_Click(object sender, RoutedEventArgs e)
    {
        LogBox.Clear();
        _logAll.Clear();
        _scanPos = 0;   // 清的是屏幕，不是进度：游标归零，后面的标记照常认
    }

    // ---------------------------------------------------------------- 运行

    private void Start_Click(object sender, RoutedEventArgs e)
    {
        if (_running) return;

        var ipa = IpaBox.Text.Trim();
        var email = EmailBox.Text.Trim();
        var pwd = PwdBox.Password;

        if (!File.Exists(ipa)) { Warn("签名文件不存在，请先选择 HermesEnabler.ipa。"); return; }
        if (!File.Exists(_engine)) { Warn("没有找到 hermesfix.exe，请点右下角「更换」指定。"); return; }
        if (email.Length == 0) { Warn("请填写 Apple ID 邮箱。"); EmailBox.Focus(); return; }
        if (pwd.Length == 0) { Warn("请填写 Apple ID 密码。"); PwdBox.Focus(); return; }

        SaveSettings();

        _running = true;
        _userStopped = false;
        StartBtn.IsEnabled = false;
        StopBtn.IsEnabled = true;
        SetFormEnabled(false);
        ResetSteps();
        SetStatus("正在启动", "#F2731C");
        AppendLog("\r\n──────────────────────────────────────────────\r\n");
        AppendLog($"» 签名引擎：{_engine}\r\n» 目标文件：{ipa}\r\n» 账号：{email}\r\n\r\n");

        // 关键：stdout 与 stderr 是两条独立管道，谁先到谁后到没有保证，
        // 直接分别读会把 2FA 提示块排到 "[1/4]" 前面。用 cmd 的 2>&1 并成一条流，
        // 顺序就与引擎写入的顺序完全一致。
        var comspec = Environment.GetEnvironmentVariable("ComSpec") ?? "cmd.exe";
        var psi = new ProcessStartInfo
        {
            FileName = comspec,
            Arguments = $"/d /s /c \"\"{_engine}\" \"{ipa}\" 2>&1\"",
            WorkingDirectory = Path.GetDirectoryName(_engine) ?? AppContext.BaseDirectory,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            StandardOutputEncoding = new UTF8Encoding(false),
        };
        // 密码只通过子进程环境变量传递，不落盘、不进日志。
        psi.Environment["HERMES_EMAIL"] = email;
        psi.Environment["HERMES_PW"] = pwd;

        PwdBox.Clear();
        _pending.Clear();
        _rawErr.Clear();

        try
        {
            _proc = new Process { StartInfo = psi, EnableRaisingEvents = true };
            _proc.Exited += (_, _) => Dispatcher.BeginInvoke(() => OnEngineExited());
            _proc.Start();
            _stdin = _proc.StandardInput;
            _stdin.AutoFlush = true;

            _ = PumpAsync(_proc.StandardOutput, true);
        }
        catch (Exception ex)
        {
            AppendLog($"» 启动失败：{ex.Message}\r\n");
            Finish(false, "无法启动签名引擎");
        }
    }

    private async Task PumpAsync(StreamReader reader, bool isStdout)
    {
        var buf = new char[1024];
        while (true)
        {
            int n;
            try { n = await reader.ReadAsync(buf, 0, buf.Length).ConfigureAwait(false); }
            catch { break; }
            if (n <= 0) break;
            var chunk = new string(buf, 0, n);
            try { Dispatcher.Invoke(() => OnChunk(chunk, isStdout)); } catch { }
        }
    }

    private void OnChunk(string chunk, bool isStdout)
    {
        // 引擎的 stdout 与 stderr 已经被 cmd 的 2>&1 并成一条流，
        // 所以号码列表也是从这儿过的——不管 isStdout 是什么都要留底。
        if (_rawErr.Length > 16384) _rawErr.Remove(0, _rawErr.Length - 8192);
        _rawErr.Append(chunk);

        if (_diag != null)
        {
            var head = chunk.Length > 36 ? chunk[..36] : chunk;
            _diag.AppendLine($"{DateTime.Now:HH:mm:ss.fff} {(isStdout ? "out" : "err")} " +
                             $"len={chunk.Length,4} :: {head.Replace("\r", "").Replace("\n", "|")}");
        }
        if (!isStdout) { AppendLog(chunk); return; }

        _pending.Append(chunk);

        // 提示串不会带换行，靠尾部匹配判断；其余内容及时刷出，保留少量尾巴防止被截断。
        const int keep = 40;
        if (_pending.Length > keep)
        {
            var flush = _pending.ToString(0, _pending.Length - keep);
            _pending.Remove(0, flush.Length);
            AppendLog(flush);
        }

        var tail = _pending.ToString();
        foreach (var marker in CodeMarkers)
        {
            if (!tail.EndsWith(marker, StringComparison.Ordinal)) continue;
            AppendLog(tail[..^marker.Length]);
            _pending.Clear();
            OnPrompt(marker switch
            {
                "Two-factor code: " => PromptKind.Code,
                "Apple ID (email): " => PromptKind.Email,
                _ => PromptKind.Password,
            });
            return;
        }
    }

    private void Stream_FlushPending()
    {
        if (_pending.Length == 0) return;
        AppendLog(_pending.ToString());
        _pending.Clear();
    }

    private void AppendLog(string text)
    {
        if (string.IsNullOrEmpty(text)) return;
        text = text.Replace("\r\n", "\n").Replace("\n", "\r\n");
        text = LogRedactor.Redact(text);   // 屏幕、_logAll、以后的导出，都是同一份
        _logAll.Append(text);
        LogBox.AppendText(text);
        LogBox.ScrollToEnd();
        ScanSteps();
    }

    // ---------------------------------------------------------------- 进度

    private void ScanSteps()
    {
        // 只扫新增的那一段：日志会一直变长，全量重扫是 O(n²)；
        // 而且清空日志后从头扫就再也看不到已经过去的标记了。
        if (_scanPos >= _logAll.Length) return;
        var s = _logAll.ToString(_scanPos, _logAll.Length - _scanPos);
        _scanPos = _logAll.Length;

        if (s.Contains("[1/4]")) MarkStep(1);
        if (s.Contains("login ok")) MarkStep(1, true);
        if (s.Contains("[2/4]")) MarkStep(2);
        if (s.Contains("session ok")) MarkStep(2, true);
        if (s.Contains("[3/4]")) MarkStep(3);
        if (s.Contains("using iPhone")) MarkStep(3, true);
        if (s.Contains("[4/4]")) MarkStep(4);
        if (s.Contains("[DONE]")) MarkStep(4, true);
        if (s.Contains("[FATAL]")) MarkFailure();
    }

    private void MarkStep(int index, bool done = false)
    {
        if (!done) _activeStep = index;

        var chips = StepsPanel.Children.OfType<Border>().ToArray();
        for (int i = 0; i < chips.Length; i++)
        {
            int step = i + 1;
            if (step < index || (step == index && done))
                PaintChip(chips[i], "done");
            else if (step == index)
                PaintChip(chips[i], "active");
            else
                PaintChip(chips[i], "idle");
        }

        if (!done) SetStatus($"第 {index} / 4 步", "#F2731C");
    }

    private void MarkFailure()
    {
        var chips = StepsPanel.Children.OfType<Border>().ToArray();
        if (_activeStep >= 1 && _activeStep <= chips.Length)
            PaintChip(chips[_activeStep - 1], "fail");
        SetStatus("失败", "#F85149");
    }

    private static void PaintChip(Border chip, string state)
    {
        var label = (TextBlock)chip.Child;
        switch (state)
        {
            case "done":
                chip.BorderBrush = Hex("#1E3A24");
                chip.Background = Hex("#131C16");
                label.Foreground = Hex("#3FB950");
                break;
            case "active":
                chip.BorderBrush = Hex("#F2731C");
                chip.Background = Hex("#241A10");
                label.Foreground = Hex("#F0A25E");
                break;
            case "fail":
                chip.BorderBrush = Hex("#5A2020");
                chip.Background = Hex("#1F1414");
                label.Foreground = Hex("#F85149");
                break;
            default:
                chip.BorderBrush = Hex("#2A303B");
                chip.Background = Brushes.Transparent;
                label.Foreground = Hex("#6B7686");
                break;
        }
    }

    private void ResetSteps()
    {
        _activeStep = 0;
        foreach (var chip in StepsPanel.Children.OfType<Border>())
            PaintChip(chip, "idle");
    }

    // ---------------------------------------------------------------- 2FA

    private void OnPrompt(PromptKind kind)
    {
        _awaiting = kind;
        TwoFaBar.Visibility = Visibility.Visible;
        TwoFaInput.Clear();
        NumberRow.Visibility = Visibility.Collapsed;   // 邮箱/密码提示时不该留着号码

        switch (kind)
        {
            case PromptKind.Code:
                TwoFaTitle.Text = "等两步验证码";
                TwoFaHint.Text = "Apple 默认发「推送」到受信任设备，不一定是短信。也可以输入 sms 1 指定号码；输入 push 让 Apple 再推一次。";
                TwoFaInput.Tag = "6 位验证码";
                RenderNumbers();
                break;
            case PromptKind.Email:
                TwoFaTitle.Text = "需要 Apple ID 邮箱";
                TwoFaHint.Text = "程序没有收到邮箱，请在这里补填一次。";
                break;
            default:
                TwoFaTitle.Text = "需要 Apple ID 密码";
                TwoFaHint.Text = "程序没有收到密码，请在这里补填一次。";
                break;
        }

        SetStatus("等待输入", "#F2731C");
        TwoFaInput.Focus();
        Keyboard.Focus(TwoFaInput);
    }

    /// <summary>
    /// 把引擎报出来的受信任号码画成可点的胶囊。点一下就把短信发到那个号。
    /// Apple 说不能发短信、或者根本没报号码时，整行隐藏而不是画一堆禁用按钮。
    /// </summary>
    private void RenderNumbers()
    {
        NumberButtons.Children.Clear();

        var raw = _rawErr.ToString();
        var numbers = TwoFactorPanel.Parse(raw);
        if (numbers.Count == 0 || !TwoFactorPanel.SmsSupported(raw))
        {
            NumberRow.Visibility = Visibility.Collapsed;
            return;
        }

        foreach (var n in numbers)
        {
            var btn = new Button
            {
                Content = n.Masked,
                Style = (Style)FindResource("GhostButton"),
                Height = 26,
                Padding = new Thickness(10, 0, 10, 0),
                FontSize = 11,
                Margin = new Thickness(0, 0, 6, 0),
                Cursor = Cursors.Hand,
                ToolTip = n.CanPush ? "短信发到这个号码" : "短信发到这个号码（该号码不支持推送）",
            };
            btn.Click += (_, _) => SendSmsTo(n);
            NumberButtons.Children.Add(btn);
        }

        NumberRow.Visibility = Visibility.Visible;
    }

    private void SendSmsTo(TwoFactorPanel.TrustedNumber n)
    {
        // 只回显掩码：完整号码是引擎打的，不该在界面上多留一份。
        AppendLog($"» 请求短信到 {n.Masked}\r\n");
        Send($"sms {n.Id}");
    }

    private void Send(string value)
    {
        if (_stdin == null) return;
        try
        {
            _stdin.WriteLine(value);
            _stdin.Flush();
        }
        catch (Exception ex)
        {
            AppendLog($"» 无法回传输入：{ex.Message}\r\n");
        }

        // 验证码与密码都不进日志，只回显动作。
        var shown = value;
        if (_awaiting == PromptKind.Password) shown = "********";
        else if (_awaiting == PromptKind.Code)
            shown = value.Length == 0 ? "（空，视为重发）"
                 : value.All(char.IsDigit) ? new string('•', value.Length)
                 : value;
        AppendLog($"» 已提交：{shown}\r\n");

        _awaiting = PromptKind.None;
        TwoFaInput.Clear();
        TwoFaBar.Visibility = Visibility.Collapsed;
        SetStatus("处理中", "#F2731C");
    }

    private void Submit2FA_Click(object sender, RoutedEventArgs e) => SubmitCurrent();

    private void SubmitCurrent()
    {
        var text = TwoFaInput.Text.Trim();
        switch (_awaiting)
        {
            case PromptKind.Email:
                if (text.Length == 0) { TwoFaInput.Focus(); return; }
                EmailBox.Text = text;
                Send(text);
                break;
            case PromptKind.Password:
                if (text.Length == 0) { TwoFaInput.Focus(); return; }
                Send(text);
                break;
            case PromptKind.Code:
                // 空输入等于“再推一次”，和 CLI 的行为保持一致。
                Send(text.Length == 0 ? "" : text);
                break;
        }
    }

    private void TwoFaInput_KeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Enter) { SubmitCurrent(); e.Handled = true; }
        else if (e.Key == Key.Escape) { Send("abort"); e.Handled = true; }
    }

    private void Push2FA_Click(object sender, RoutedEventArgs e) => Send("push");

    private void Sms2FA_Click(object sender, RoutedEventArgs e) => Send("sms");

    private void Abort2FA_Click(object sender, RoutedEventArgs e) => Send("abort");

    // ---------------------------------------------------------------- 结束

    private void OnEngineExited()
    {
        Stream_FlushPending();
        var code = -1;
        try { code = _proc?.ExitCode ?? -1; } catch { }

        if (_userStopped)
        {
            AppendLog($"\r\n» 已按你的要求停止，退出码 {code}\r\n");
            Finish(false, "已停止");
            return;
        }

        bool ok = code == 0 && _logAll.ToString().Contains("[DONE]");
        AppendLog($"\r\n» 进程已退出，退出码 {code}\r\n");

        if (ok)
        {
            AppendLog("» 签名与安装已完成。接下来在手表上：HermesEnabler → Backup MG → 打开爱马仕表盘 → Restart Carousel。\r\n");
            Finish(true, "完成");
        }
        else
        {
            AppendLog("» 没有走完。常见原因：iPhone 未解锁 / 数据线松了 / 账号密码错 / 验证码超时。\r\n");
            Finish(false, code == 0 ? "未完成" : $"失败（{code}）");
        }
    }

    private void Stop_Click(object sender, RoutedEventArgs e)
    {
        _userStopped = true;
        AppendLog("» 正在停止…\r\n");
        KillEngine();
        Finish(false, "已停止");
    }

    private void KillEngine()
    {
        try
        {
            if (_proc is { HasExited: false })
                _proc.Kill(entireProcessTree: true);
        }
        catch { }
    }

    private void Finish(bool success, string status)
    {
        _running = false;
        _awaiting = PromptKind.None;
        TwoFaBar.Visibility = Visibility.Collapsed;
        StartBtn.IsEnabled = true;
        StopBtn.IsEnabled = false;
        SetFormEnabled(true);
        SetStatus(status, success ? "#3FB950" : "#F85149");
    }

    private void SetFormEnabled(bool enabled)
    {
        IpaBox.IsEnabled = enabled;
        EmailBox.IsEnabled = enabled;
        PwdBox.IsEnabled = enabled;
        RememberEmail.IsEnabled = enabled;
    }

    private void SetStatus(string text, string color)
    {
        StatusText.Text = text;
        StatusText.Foreground = Hex(color);
        StatusDot.Fill = Hex(color);
    }

    private void Warn(string msg)
    {
        MessageBox.Show(this, msg, "爱马仕表盘助手", MessageBoxButton.OK, MessageBoxImage.Warning);
    }

    // ---------------------------------------------------------------- 设置

    private void SaveSettings()
    {
        try
        {
            var st = new Settings
            {
                RememberEmail = RememberEmail.IsChecked == true,
                Email = RememberEmail.IsChecked == true ? EmailBox.Text.Trim() : "",
                LastIpa = IpaBox.Text.Trim(),
                EnginePath = _engine,
            };
            st.Save();
        }
        catch { }
    }

    private static Brush Hex(string hex) =>
        new SolidColorBrush((Color)ColorConverter.ConvertFromString(hex));
}

internal sealed class Settings
{
    public bool RememberEmail { get; set; } = true;
    public string Email { get; set; } = "";
    public string LastIpa { get; set; } = "";
    public string EnginePath { get; set; } = "";

    private static string PathFile => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "hermes-watch-windows", "gui.json");

    public static Settings Load()
    {
        try
        {
            var p = PathFile;
            if (!File.Exists(p)) return new Settings();
            return JsonSerializer.Deserialize<Settings>(File.ReadAllText(p)) ?? new Settings();
        }
        catch { return new Settings(); }
    }

    public void Save()
    {
        try
        {
            var p = PathFile;
            Directory.CreateDirectory(Path.GetDirectoryName(p)!);
            File.WriteAllText(p, JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true }));
        }
        catch { }
    }
}
