using System.Diagnostics;
using System.IO;
using System.Net.Http;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;

namespace HermesFix;

internal enum CheckState { Ok, Warn, Fail, Info }

internal sealed record CheckItem(string Name, CheckState State, string Detail, string? Hint = null)
{
    public string Badge => State switch
    {
        CheckState.Ok => "通过",
        CheckState.Warn => "注意",
        CheckState.Fail => "未通过",
        _ => "需人工",
    };
}

/// <summary>
/// 启动前的环境自检：把"跑起来才知道缺什么"提前成"打开就知道"。
/// 只做能确定判断的检查，判不了的如实标成"需人工"，不假装通过。
/// </summary>
internal static class EnvCheck
{
    /// <summary>
    /// 仓库里 <c>ipa/HermesEnabler.ipa</c> 那一份的指纹，用来判断"你选的是不是这一份"。
    /// 升级 IPA 时这三行要一起改，否则自检会把仓库自带的文件报成"不一致"。
    /// </summary>
    private const string KnownIpaVersion = "1.5.4";
    private const string KnownIpaSha = "b8cf1cfaa129a61411fc0b7780fe85d2b2f8dab070d01eb8596cf45d883a43f9";
    private const long KnownIpaSize = 959386;

    public static async Task<List<CheckItem>> RunAllAsync(string ipaPath)
    {
        var items = new List<CheckItem>
        {
            CheckRuntime(),
            CheckEngine(),
            CheckIpa(ipaPath),
        };

        var (deviceOk, deviceDetail) = await Task.Run(CheckDeviceService);
        items.Add(new CheckItem("iPhone 连接服务", deviceOk, deviceDetail,
            deviceOk == CheckState.Ok
                ? null
                : "装 iTunes 或 Apple Devices 应用，插上数据线并在 iPhone 上点「信任」"));

        var (netOk, netDetail) = await CheckAnisette();
        items.Add(new CheckItem("Anisette 服务", netOk, netDetail,
            netOk == CheckState.Ok ? null : "这是第三方登录辅助服务，不通就没法登录 Apple ID；换网络或稍后重试"));

        items.Add(new CheckItem("手表开发者模式", CheckState.Info,
            "无法在签名前读取，运行时由日志判定",
            "日志出现「Device is a development device」即通过；没有就先按 docs/00 阶段 1 打开"));

        return items;
    }

    private static CheckItem CheckRuntime()
    {
        var desc = RuntimeInformation.FrameworkDescription;
        if (!Environment.Is64BitProcess)
            return new CheckItem("运行环境", CheckState.Fail, $"{desc}（32 位进程）", "需要 64 位 Windows");
        return new CheckItem("运行环境", CheckState.Ok,
            $"{desc} · {RuntimeInformation.OSDescription.Split(' ')[0]} · x64");
    }

    private static CheckItem CheckEngine()
    {
        var path = Engine.Resolve();
        if (path is null || !File.Exists(path))
            return new CheckItem("签名引擎", CheckState.Fail, "没有找到 hermesfix.exe",
                "这个版本本应自带引擎；若被安全软件删掉，请重新下载");
        var mb = new FileInfo(path).Length / 1024.0 / 1024.0;
        // 完整路径里的 Windows 用户名不该出现在界面上——「复制报告」会原样把它带出去，
        // 而这个报告本来就是准备发给别人看的。走和日志导出同一套掩码。
        return new CheckItem("签名引擎", CheckState.Ok, $"{mb:F1} MB · {LogRedactor.Redact(path)}");
    }

    private static CheckItem CheckIpa(string ipaPath)
    {
        if (string.IsNullOrWhiteSpace(ipaPath) || !File.Exists(ipaPath))
            return new CheckItem("签名文件", CheckState.Fail, "还没有选择 .ipa",
                $"仓库 ipa/HermesEnabler.ipa 就是 {KnownIpaVersion}，直接选它；也可以把文件拖进窗口");

        try
        {
            var size = new FileInfo(ipaPath).Length;
            if (size == KnownIpaSize && Engine.HashFile(ipaPath) == KnownIpaSha)
                return new CheckItem("签名文件", CheckState.Ok,
                    $"HermesEnabler {KnownIpaVersion} · {size:N0} 字节 · 指纹匹配");

            return new CheckItem("签名文件", CheckState.Warn,
                $"{Path.GetFileName(ipaPath)} · {size:N0} 字节 · 与仓库里的 {KnownIpaVersion} 不一致",
                $"不是 {KnownIpaVersion} 也能签，但签名脚本里的 entitlements 是按 {KnownIpaVersion} 的结构验证过的");
        }
        catch (Exception ex)
        {
            return new CheckItem("签名文件", CheckState.Fail, "读取失败：" + ex.Message);
        }
    }

    private static (CheckState, string) CheckDeviceService()
    {
        bool portOpen = false;
        try
        {
            using var client = new TcpClient();
            var task = client.ConnectAsync("127.0.0.1", 27015);
            portOpen = task.Wait(TimeSpan.FromMilliseconds(700)) && client.Connected;
        }
        catch { }

        bool procAlive;
        try { procAlive = Process.GetProcessesByName("AppleMobileDeviceService").Length > 0; }
        catch { procAlive = false; }

        if (portOpen) return (CheckState.Ok, "Apple 移动设备服务在运行（端口 27015 可连）");
        if (procAlive) return (CheckState.Warn, "服务进程在，但端口 27015 连不上");
        return (CheckState.Fail, "没有检测到 Apple 移动设备服务");
    }

    private static async Task<(CheckState, string)> CheckAnisette()
    {
        try
        {
            using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(4) };
            using var resp = await http.GetAsync("https://ani.jkcoxson.com/");
            return (CheckState.Ok, $"可访问（HTTP {(int)resp.StatusCode}）");
        }
        catch (Exception ex)
        {
            return (CheckState.Warn, "连不上：" + ex.GetType().Name);
        }
    }

    public static string Report(IEnumerable<CheckItem> items)
    {
        var sb = new StringBuilder();
        sb.AppendLine($"HermesFixGui 环境自检 · {DateTime.Now:yyyy-MM-dd HH:mm:ss}");
        sb.AppendLine(new string('-', 60));
        foreach (var i in items)
            sb.AppendLine($"[{i.Badge}] {i.Name}：{i.Detail}");
        return sb.ToString();
    }
}
