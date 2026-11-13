// 无头测试：不需要真机、不需要 Apple ID，验证两件和安全相关的事。
//   1) 2FA 号码列表能否从引擎输出里正确读出来（含掩码与能否推送）
//   2) 日志脱敏是否真的把号码/邮箱/UDID/用户目录挡住了，同时不破坏排错信息
//
// 直接跑：dotnet run -c Release
// 引擎路径默认取同级 fake-engine 的 Release 产物，可用第一个参数覆盖。

using System.Diagnostics;
using System.Text;
using HermesFix;

Console.OutputEncoding = Encoding.UTF8;

var fail = 0;
void Check(string name, bool ok, string detail = "")
{
    Console.WriteLine($"  {(ok ? "通过" : "失败")}  {name}{(detail.Length > 0 ? "   " + detail : "")}");
    if (!ok) fail++;
}

// ---- 1. 真跑一遍模拟引擎，按 GUI 的方式合并 stdout / stderr ----
var engine = args.Length > 0 ? args[0] : FindEngine();
if (engine == null)
{
    Console.WriteLine("找不到 fake-engine.exe，先构建它：dotnet build ../fake-engine -c Release");
    return 2;
}

var psi = new ProcessStartInfo(engine)
{
    RedirectStandardInput = true,
    RedirectStandardOutput = true,
    RedirectStandardError = true,
    UseShellExecute = false,
    StandardOutputEncoding = Encoding.UTF8,
    StandardErrorEncoding = Encoding.UTF8,
    CreateNoWindow = true,
};
psi.Environment["HERMES_EMAIL"] = "demo@example.com";

// 演示脚本会在模拟引擎旁边放一个 probe-ready.flag，用来模拟"手表已解锁"。
// 那个标记一存在，替身就会忽略 HF_SCENARIO 直接成功，把这里的故障用例全带偏。
var staleFlag = Path.Combine(Path.GetDirectoryName(engine)!, "probe-ready.flag");
if (File.Exists(staleFlag)) File.Delete(staleFlag);

using var p = Process.Start(psi)!;

var raw = new StringBuilder();     // 对应 MainWindow.xaml.cs 的 _rawErr（留原文，供解析）
var shown = new StringBuilder();   // 对应 _logAll（已脱敏，屏幕和导出都用它）

void Pump(string chunk)
{
    raw.Append(chunk);
    shown.Append(LogRedactor.Redact(chunk));   // 与 AppendLog 入口同一份逻辑
}

p.OutputDataReceived += (_, e) => { if (e.Data != null) Pump(e.Data + "\n"); };
p.ErrorDataReceived += (_, e) => { if (e.Data != null) Pump(e.Data + "\n"); };
p.BeginOutputReadLine();
p.BeginErrorReadLine();

// 等引擎打出 2FA 提示再回验证码——顺便验证"提示不带换行也能被识别"
var deadline = DateTime.UtcNow.AddSeconds(15);
while (DateTime.UtcNow < deadline && !raw.ToString().Contains("Two-factor code:"))
    await Task.Delay(50);

Console.WriteLine("=== 1. 号码解析（源：模拟引擎实际输出）===");
var nums = TwoFactorPanel.Parse(raw.ToString());
Check("解析出 2 个受信任号码", nums.Count == 2, $"实际 {nums.Count}");
Check("id=1 掩码 ***78 且可推送", nums.Count == 2 && nums[0].Id == 1 && nums[0].Masked == "***78" && nums[0].CanPush);
Check("id=2 掩码 ***11 且不可推送", nums.Count == 2 && nums[1].Id == 2 && nums[1].Masked == "***11" && !nums[1].CanPush);
Check("sms supported 被识别", TwoFactorPanel.SmsSupported(raw.ToString()));
Check("缺行时不隐藏入口（宁可多给一个按钮）", TwoFactorPanel.SmsSupported("无关文本"));

Console.WriteLine("\n=== 2. 进程往返与退出码 ===");
p.StandardInput.WriteLine("123456");
p.StandardInput.Flush();
await p.WaitForExitAsync();
Check("退出码为 0", p.ExitCode == 0, $"实际 {p.ExitCode}");
var log = shown.ToString();
Check("验证码 123456 未出现在日志里", !log.Contains("123456"));
Check("五步标记全部走完", log.Contains("[1/5]") && log.Contains("[2/5]") && log.Contains("[3/5]") && log.Contains("[4/5]") && log.Contains("[5/5]"));
Check("成功标记在（判成功要靠它）", log.Contains("App signed!") && log.Contains("login ok"));

Console.WriteLine("\n=== 3. 脱敏：该挡的挡住 ===");
Check("完整号码 13812345678 未出现", !log.Contains("13812345678"));
Check("完整号码 13900001111 未出现", !log.Contains("13900001111"));
Check("国家码前缀无残留", !log.Contains("+86 138") && !log.Contains("+86 139"));
Check("邮箱 demo@example.com 已掩码", !log.Contains("demo@example.com") && log.Contains("d***@example.com"));
Check("设备 UDID 已掩码", !log.Contains("000A1D2E3F40801E"));

Console.WriteLine("\n=== 4. 脱敏：该留的留住（否则日志没法排错）===");
Check("号码行的 id / push 字段保留", log.Contains("id=1") && log.Contains("push=true") && log.Contains("push=false"));
Check("掩码 ***78 / ***11 保留", log.Contains("(***78)") && log.Contains("(***11)"));
Check("不出现多余的 *** (***78)", !log.Contains("*** (***"));
Check("整行收拾干净", log.Contains("trusted number: id=1 (***78) push=true"));
Check("设备型号保留", log.Contains("using iPhone"));
Check("UDID 头尾保留，可判断是否同一台", log.Contains("0000") && log.Contains("801E"));
Check("进度百分比保留", log.Contains("Installing: 97%"));

Console.WriteLine("\n=== 5. 脱敏规则：逐条 ===");
Check(@"C:\Users\张三\x → <user>",
    LogRedactor.Redact(@"见 C:\Users\张三\.cargo\config") == @"见 C:\Users\<user>\.cargo\config");
Check("正斜杠路径同样处理",
    LogRedactor.Redact("path C:/Users/someone/app") == "path C:/Users/<user>/app");
// 环境自检里显示引擎位置用的就是这一串，它会跟着「复制报告」发出去
Check("引擎真实路径里的用户名被抹掉",
    LogRedactor.Redact(@"26.3 MB · C:\Users\张三\AppData\Local\hermes-watch-windows\engine\hermesfix.exe")
        == @"26.3 MB · C:\Users\<user>\AppData\Local\hermes-watch-windows\engine\hermesfix.exe");
Check("抹掉用户名后仍能看出装在哪一级目录",
    LogRedactor.Redact(@"C:\Users\张三\AppData\Local\hermes-watch-windows\engine\hermesfix.exe")
        .EndsWith(@"\AppData\Local\hermes-watch-windows\engine\hermesfix.exe"));
Check("老式 40 位 UDID 也挡住",
    !LogRedactor.Redact("using iPhone 0000803000A1B2C3D4E5F60718293A4B5C6D7E8F").Contains("D4E5F60718293A4B5C6D7E8F"));
Check("带 udid: 标签的形式挡住",
    !LogRedactor.Redact("udid: 00008110-000A1D2E3F40801E").Contains("000A1D2E3F40801E"));
Check("大写 UDID 标签同样认",
    !LogRedactor.Redact("UDID=00008110-000A1D2E3F40801E").Contains("000A1D2E3F40801E"));
Check("普通文本原样返回（不误伤）",
    LogRedactor.Redact("[2/5] Signing the app (watch App Groups) ...") == "[2/5] Signing the app (watch App Groups) ...");
Check("空文本不炸", LogRedactor.Redact("") == "");
Check("错误码不被吃掉",
    LogRedactor.Redact("[FATAL] aborted by user") == "[FATAL] aborted by user");

// ---- 6. 回归：管道把一行切成两半时，号码仍然挡得住 ----
// 这条是给一个真实踩过的坑兜底：早先从 _pending 里按 40 字符硬切再脱敏，
// 行被切成两半，正则匹配不到整行，完整号码就这么漏进日志了。
Console.WriteLine("\n=== 6. 任意切分点都不漏（回归）===");

var sample = string.Concat(
    "    sms supported : true\n",
    "    trusted number: id=1 +86 13812345678 (***78) push=true\n",
    "      using iPhone 00008110-000A1D2E3F40801E\n",
    "» 账号：demo@example.com\n");

string FeedInPieces(string text, int cut)
{
    var buf = new StringBuilder();
    var outp = new StringBuilder();
    foreach (var piece in new[] { text[..cut], text[cut..] })
    {
        buf.Append(piece);
        var complete = LineAssembler.TakeCompleteLines(buf);
        if (complete != null) outp.Append(LogRedactor.Redact(complete));
    }
    // 收尾：Stream_FlushPending 走的是同一份 Redact
    if (buf.Length > 0) outp.Append(LogRedactor.Redact(buf.ToString()));
    return outp.ToString();
}

var leaks = new List<int>();
for (var cut = 1; cut < sample.Length; cut++)
{
    var got = FeedInPieces(sample, cut);
    if (got.Contains("13812345678") || got.Contains("000A1D2E3F40801E") || got.Contains("demo@example.com"))
        leaks.Add(cut);
}
Check($"在全部 {sample.Length - 1} 个切分点上都未泄露", leaks.Count == 0,
    leaks.Count == 0 ? "" : "泄露切分点 " + string.Join(",", leaks.Take(8)));

Console.WriteLine("\n=== 7. 结构事件：手表锁屏要被认成 WATCH_LOCKED ===");
var locked = RunFake(engine, "watch-locked", "");

EngineEvent? failureEvent = null;
var completedStages = new List<string>();
var watchCodes = new List<string>();
var sawDone = false;

foreach (var line in locked.Output.Split('\n'))
{
    var ev = EngineEvents.Parse(line);
    if (ev == null) continue;
    switch (ev.Event)
    {
        case "failure": failureEvent = ev; break;
        case "stage" when ev.State == "done": completedStages.Add(ev.StageLabel); break;
        case "watch": watchCodes.Add(ev.Code); break;
        case "done": sawDone = true; break;
    }
}

Check("退出码 3（手表阶段失败但可续装）", locked.Code == 3, $"实际 {locked.Code}");
Check("收到 failure 事件", failureEvent != null);
Check("错误码是 WATCH_LOCKED", failureEvent?.Code == "WATCH_LOCKED", $"实际 {failureEvent?.Code}");
Check("失败落在 watch_connect 阶段", failureEvent?.Stage == "watch_connect", $"实际 {failureEvent?.Stage}");
Check("标了可续装（retry=watch_only）", failureEvent?.Retry == "watch_only", $"实际 {failureEvent?.Retry}");
Check("标题说的是手表解锁，不是 Apple ID",
    (failureEvent?.Title ?? "").Contains("手表") && !(failureEvent?.Title ?? "").Contains("Apple ID"),
    failureEvent?.Title ?? "");
Check("给的是「在手表上输入密码」这种能照做的动作",
    (failureEvent?.Action ?? "").Contains("解锁"), failureEvent?.Action ?? "");
Check("没有 done 事件（不能假装装完）", !sawDone);
Check("签名与 iPhone 安装仍然算完成", completedStages.Contains("签名") && completedStages.Contains("iPhone 安装"),
    string.Join(",", completedStages));
Check("手表连接检查拿到了锁屏结论", watchCodes.Contains("WATCH_LOCKED"), string.Join(",", watchCodes));

Console.WriteLine("\n=== 8. 结构事件：解析的边界情况 ===");
Check("普通日志不误判", EngineEvents.Parse("[1/5] Signing in to Apple ID ...") == null);
Check("截断的 JSON 不抛异常", EngineEvents.Parse("@@HF_EVENT@@ {\"v\":1,\"event\":") == null);
Check("行首有杂音也能定位",
    EngineEvents.Parse("噪音 @@HF_EVENT@@ {\"v\":1,\"event\":\"done\"}")?.Event == "done");
Check("阶段名到步骤号的映射",
    EngineEvents.StepIndex("login") == 1 && EngineEvents.StepIndex("watch_install") == 5 && EngineEvents.StepIndex("???") == 0);
Check("失败事件里的 recoverable 读得出来",
    failureEvent is { Recoverable: true });

Console.WriteLine(fail == 0 ? "\n全部通过" : $"\n{fail} 项失败");
return fail == 0 ? 0 : 1;

// 跑一个指定结局的模拟引擎，返回退出码与 stdout。
static (int Code, string Output) RunFake(string engine, string scenario, string arg)
{
    var psi = new ProcessStartInfo(engine)
    {
        RedirectStandardInput = true,
        RedirectStandardOutput = true,
        UseShellExecute = false,
        StandardOutputEncoding = Encoding.UTF8,
        CreateNoWindow = true,
    };
    psi.Environment["HERMES_EMAIL"] = "demo@example.com";
    if (scenario.Length > 0) psi.Environment["HF_SCENARIO"] = scenario;
    if (arg.Length > 0) psi.ArgumentList.Add(arg);

    using var p = Process.Start(psi)!;
    var reader = p.StandardOutput.ReadToEndAsync();
    Thread.Sleep(600);
    try { p.StandardInput.WriteLine("123456"); p.StandardInput.Flush(); } catch { /* 探测流程不读 stdin */ }
    if (!p.WaitForExit(20000)) { try { p.Kill(true); } catch { } }
    return (p.ExitCode, reader.GetAwaiter().GetResult());
}

// 从测试程序所在目录往回找 fake-engine 的产物
static string? FindEngine()
{
    var dir = new DirectoryInfo(AppContext.BaseDirectory);
    for (var i = 0; i < 8 && dir != null; i++, dir = dir.Parent)
    {
        foreach (var cfg in new[] { "Release", "Debug" })
        {
            var p = Path.Combine(dir.FullName, "fake-engine", "bin", cfg, "net9.0", "fake-engine.exe");
            if (File.Exists(p)) return p;
        }
    }
    return null;
}
