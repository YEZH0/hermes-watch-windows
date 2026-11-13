// 测试替身：不用真机、不用 Apple ID，也能验证 GUI 的输出解析、2FA 往返、
// 阶段事件与退出码处理。它逐字复刻 hermesfix 的关键输出（含不带换行的
// "Two-factor code: " 提示与 @@HF_EVENT@@ 结构事件）。
//
// HF_SCENARIO 可以换掉结局：
//   （空）            全流程成功，退出 0
//   watch-locked      手表锁屏，Watch 连接阶段失败，退出 3
//   watch-unreachable 手表连不上，退出 3
//   watch-busy        安装协调器冲突，退出 3
//   iphone-locked     iPhone 锁屏，iPhone 安装阶段失败，退出 2

using System.Text;

Console.OutputEncoding = Encoding.UTF8;

var argsList = args.ToList();
// HF_SCENARIO 用来复现故障；一旦「用户」把手表解锁了，测试需要能切回成功路径。
// 同目录下的 probe-ready.flag 就表示这件事，不必改父进程的环境变量。
var recovered = File.Exists(Path.Combine(AppContext.BaseDirectory, "probe-ready.flag"));
var scenario = recovered ? "" : Environment.GetEnvironmentVariable("HF_SCENARIO") ?? "";
var resume = argsList.Contains("--resume-watch");
var probe = argsList.Contains("--probe-watch");
var ipa = argsList.FirstOrDefault(a => !a.StartsWith("--")) ?? "HermesEnabler.ipa";

// 真引擎用 Rust 的 println!，每行都会立即刷出；这里必须仿真这一点，
// 否则文本会攒到最后一次性出现，日志顺序就失真了。
void Out(string line)
{
    Console.Out.WriteLine(line);
    Console.Out.Flush();
}

void Err(string line)
{
    Console.Error.WriteLine(line);
    Console.Error.Flush();
}

// 与 hermesfix/src/events.rs 里的字段名逐字对齐
void Event(string json) => Out($"@@HF_EVENT@@ {json}");

void Stage(string stage, string label, string state) =>
    Event($"{{\"v\":1,\"event\":\"stage\",\"stage\":\"{stage}\",\"label\":\"{label}\",\"state\":\"{state}\"}}");

void WatchState(string code, string detail) =>
    Event($"{{\"v\":1,\"event\":\"watch\",\"code\":\"{code}\",\"detail\":\"{detail}\"}}");

void Die(string stage, string label, string code, string title, string action, string retry, int exit)
{
    var recoverable = retry != "none" ? "true" : "false";
    Err("");
    Err($"[FATAL] {code} — {title}（阶段：{label}）");
    Err($"        {action}");
    Event(
        "{\"v\":1,\"event\":\"failure\"," +
        $"\"stage\":\"{stage}\",\"stage_label\":\"{label}\"," +
        $"\"code\":\"{code}\",\"title\":\"{title}\",\"action\":\"{action}\"," +
        $"\"recoverable\":{recoverable},\"retry\":\"{retry}\"," +
        "\"detail\":\"IdeviceError(PasswordProtected) at watch_install.rs:92\"}");
    Environment.Exit(exit);
}

void WatchFailure()
{
    switch (scenario)
    {
        case "watch-unreachable":
            WatchState("WATCH_UNREACHABLE", "手表 lockdown 端口连不上（试了 20 次）");
            Die("watch_connect", "Watch 连接", "WATCH_UNREACHABLE",
                "连不上手表",
                "确认手表已解锁、戴在手上、在 iPhone 附近；iPhone 保持解锁并用数据线连着电脑。",
                "watch_only", 3);
            break;
        case "watch-busy":
            WatchState("READY", "watchOS 11.2");
            Stage("watch_connect", "Watch 连接", "done");
            Stage("watch_install", "Watch 安装", "start");
            Die("watch_install", "Watch 安装", "WATCH_INSTALL_BUSY",
                "手表的安装协调器还被上一次安装占用",
                "iPhone 刚装完 App，watchOS 还在收尾。等 30 秒再点重试；连续失败就把手表重启一次。",
                "watch_only", 3);
            break;
        default:
            WatchState("WATCH_LOCKED", "watchOS 报告 PasswordProtected（锁屏）");
            Die("watch_connect", "Watch 连接", "WATCH_LOCKED",
                "Apple Watch 已锁定，需要先在手表上输入密码",
                "抬起手表输入解锁密码，保持戴在手上、贴近 iPhone、屏幕别灭。解锁后点「我已处理，重新检查」。",
                "watch_only", 3);
            break;
    }
}

// ---------------------------------------------------------------- 只探测手表

if (probe)
{
    if (scenario == "watch-locked")
    {
        WatchState("WATCH_LOCKED", "watchOS 报告 PasswordProtected（锁屏）");
        Out("watch: WATCH_LOCKED — watchOS 报告 PasswordProtected（锁屏）");
        Die("watch_connect", "Watch 连接", "WATCH_LOCKED",
            "Apple Watch 已锁定，需要先在手表上输入密码",
            "抬起手表输入解锁密码，保持戴在手上、贴近 iPhone。解锁后点「我已处理，重新检查」。",
            "watch_only", 3);
    }
    WatchState("READY", "watchOS 11.2");
    Out("watch: READY — watchOS 11.2");
    return 0;
}

// ---------------------------------------------------------------- 只续装手表

if (resume)
{
    Event("{\"v\":1,\"event\":\"resume\",\"available\":true,\"id\":\"cafebabecafebabe\",\"age_secs\":420,\"used\":true}");
    Out("=== hermesfix: watch-only retry ===");
    Out("cache   : cafebabecafebabe（7 分钟前签的）");
    Out("bundle  : com.example.hermes");

    Stage("watch_connect", "Watch 连接", "start");
    Out("[4/5] Connecting to the Apple Watch ...");
    Thread.Sleep(150);

    if (scenario is "watch-locked" or "watch-unreachable" or "watch-busy")
        WatchFailure();

    WatchState("READY", "watchOS 11.2");
    Out("      watch reachable and unlocked");
    Stage("watch_connect", "Watch 连接", "done");

    Stage("watch_install", "Watch 安装", "start");
    Out("[5/5] Installing the Watch app ...");
    Thread.Sleep(300);
    Out("Installing Apple Watch app: 100% InstallComplete");
    Stage("watch_install", "Watch 安装", "done");
    Event("{\"v\":1,\"event\":\"done\",\"resumed\":true}");
    Out("");
    Out("[DONE] HermesEnabler was signed and installed.");
    return 0;
}

// ---------------------------------------------------------------- 完整流程

Out("");
Out("=== hermesfix: re-sign HermesEnabler with Watch App Groups ===");
Out($"IPA     : {ipa}");
Out($"Account : {Environment.GetEnvironmentVariable("HERMES_EMAIL")}");
Out("Anisette: https://ani.jkcoxson.com");
Out("");

Stage("login", "登录", "start");
Out("[1/5] Signing in to Apple ID ...");
// 与真实引擎逐字对齐：Rust 的 bool 是小写 true/false，号码是完整号码
// （main.rs 打的是 number_with_dial_code）。早先这里写成了已掩码的假号码，
// 比真实情况更"干净"，脱敏逻辑在测试里就看不出问题。
Err("    sms supported : true");
Err("    trusted number: id=1 +86 13812345678 (***78) push=true");
Err("    trusted number: id=2 +86 13900001111 (***11) push=false");
Err("    preselected   : id=1");
Err("    input: 6-digit code | push | resend | sms [id] | abort");
Console.Out.Write("Two-factor code: ");
Console.Out.Flush();

var code = Console.ReadLine();
if (code is null || code.Trim() == "abort")
{
    Err("");
    Err("[FATAL] aborted by user");
    return 3;
}

Out("      login ok");
Thread.Sleep(60);
Out("      session ok");
Stage("login", "登录", "done");

Stage("sign", "签名", "start");
Out("[2/5] Signing the app (watch App Groups) ...");
Thread.Sleep(200);
Out("      using iPhone 00008110-000A1D2E3F40801E");
// 真引擎里 sign_app 会通过 tracing 打一行 "App signed!"，main 再打一行路径
Out("App signed!");
Out("      App signed: C:\\Temp\\hermes\\Payload\\HermesCompanion.app");
Event("{\"v\":1,\"event\":\"resume\",\"available\":true,\"id\":\"cafebabecafebabe\",\"expires_at\":1893456000}");
Out("      signed copy cached as cafebabecafebabe");
Stage("sign", "签名", "done");

Stage("iphone", "iPhone 安装", "start");
Out("[3/5] Installing to iPhone ...");
Thread.Sleep(200);
if (scenario == "iphone-locked")
{
    Err("IdeviceError(PasswordProtected)");
    Die("iphone", "iPhone 安装", "IPHONE_LOCKED",
        "iPhone 处于锁定状态",
        "解锁 iPhone，看到「信任此电脑」就点信任，然后重试。",
        "full", 2);
}
Out("Installing: 97%");
Out("      iPhone install ok");
Stage("iphone", "iPhone 安装", "done");

Stage("watch_connect", "Watch 连接", "start");
Out("[4/5] Connecting to the Apple Watch ...");
Thread.Sleep(200);

if (scenario is "watch-locked" or "watch-unreachable" or "watch-busy")
    WatchFailure();

WatchState("READY", "watchOS 11.2");
Out("      watch reachable and unlocked");
Stage("watch_connect", "Watch 连接", "done");

Stage("watch_install", "Watch 安装", "start");
Out("[5/5] Installing the Watch app ...");
Thread.Sleep(400);
Out("Installing Apple Watch app: 100% InstallComplete");
Stage("watch_install", "Watch 安装", "done");
Event("{\"v\":1,\"event\":\"done\",\"resumed\":false}");
Out("");
Out("[DONE] HermesEnabler was signed and installed.");
return 0;
