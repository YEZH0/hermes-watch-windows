// 测试替身：不用真机、不用 Apple ID，也能验证 GUI 的输出解析、2FA 往返与退出码处理。
// 它逐字复刻 hermesfix 的关键输出（含不带换行的 "Two-factor code: " 提示）。

Console.OutputEncoding = System.Text.Encoding.UTF8;

var ipa = args.Length > 0 ? args[0] : "HermesEnabler.ipa";

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

Out("");
Out("=== hermesfix: re-sign HermesEnabler with Watch App Groups ===");
Out($"IPA    : {ipa}");
Out($"Account: {Environment.GetEnvironmentVariable("HERMES_EMAIL")}");
Out("Anisette: https://ani.jkcoxson.com");
Out("");

Out("[1/4] Signing in to Apple ID ...");
// 与真实引擎逐字对齐：Rust 的 bool 是小写 true/false，号码是完整号码
// （main.rs:162 打的是 number_with_dial_code）。早先这里写成了已掩码的假号码，
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
Out("[2/4] Creating developer session ...");
Thread.Sleep(250);
Out("      session ok");
Out("[3/4] Locating the USB iPhone ...");
Thread.Sleep(250);
Out("      using iPhone 00008110-000A1D2E3F40801E");
Out("[4/4] Signing and installing to iPhone + Apple Watch ...");
Out("      (keep the iPhone unlocked and connected until this finishes)");
Thread.Sleep(700);
Out("      App signed!");
Out("Installing: 97%");
Out("Installing Apple Watch app: 100% InstallComplete");
Out("");
Out("[DONE] HermesEnabler was signed and installed.");
return 0;
