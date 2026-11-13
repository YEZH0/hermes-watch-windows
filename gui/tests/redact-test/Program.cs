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
Check("四步标记全部走完", log.Contains("[1/4]") && log.Contains("[2/4]") && log.Contains("[3/4]") && log.Contains("[4/4]"));
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
Check("设备型号保留", log.Contains("using iPhone"));
Check("UDID 头尾保留，可判断是否同一台", log.Contains("0000") && log.Contains("801E"));
Check("进度百分比保留", log.Contains("Installing: 97%"));

Console.WriteLine("\n=== 5. 脱敏规则：逐条 ===");
Check(@"C:\Users\张三\x → <user>",
    LogRedactor.Redact(@"见 C:\Users\张三\.cargo\config") == @"见 C:\Users\<user>\.cargo\config");
Check("正斜杠路径同样处理",
    LogRedactor.Redact("path C:/Users/someone/app") == "path C:/Users/<user>/app");
Check("老式 40 位 UDID 也挡住",
    !LogRedactor.Redact("using iPhone 0000803000A1B2C3D4E5F60718293A4B5C6D7E8F").Contains("D4E5F60718293A4B5C6D7E8F"));
Check("带 udid: 标签的形式挡住",
    !LogRedactor.Redact("udid: 00008110-000A1D2E3F40801E").Contains("000A1D2E3F40801E"));
Check("大写 UDID 标签同样认",
    !LogRedactor.Redact("UDID=00008110-000A1D2E3F40801E").Contains("000A1D2E3F40801E"));
Check("普通文本原样返回（不误伤）",
    LogRedactor.Redact("[2/4] Creating developer session ...") == "[2/4] Creating developer session ...");
Check("空文本不炸", LogRedactor.Redact("") == "");
Check("错误码不被吃掉",
    LogRedactor.Redact("[FATAL] aborted by user") == "[FATAL] aborted by user");

Console.WriteLine(fail == 0 ? "\n全部通过" : $"\n{fail} 项失败");
return fail == 0 ? 0 : 1;

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
