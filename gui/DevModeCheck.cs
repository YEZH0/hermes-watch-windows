using System.Diagnostics;
using System.IO;
using System.Text;

namespace HermesFix;

internal enum DevModeState { Enabled, Disabled, Unknown }

/// <summary>
/// 手表开发者模式的一次检测结论。
/// <para>
/// <see cref="DevModeState.Unknown"/> 不是「没开」，是「问不出来」——两者在界面上
/// 必须长得不一样：前者让用户去开开关，后者要让用户先解决手表连接或改用第一阶段。
/// </para>
/// </summary>
internal sealed record DevModeResult(DevModeState State, string Reason, string WatchCode, string WatchDetail)
{
    public bool CanInstall => State == DevModeState.Enabled;

    public string Headline => State switch
    {
        DevModeState.Enabled => "已开启",
        DevModeState.Disabled => "未开启",
        _ => "无法确认",
    };

    /// <summary>引擎根本没跑出结论时用它——别把「界面出问题」说成「手表没开」。</summary>
    public static DevModeResult Failed(string why) =>
        new(DevModeState.Unknown, why, "PROBE_FAILED", "");
}

internal sealed record RevealOutcome(bool Ok, string Reason);

/// <summary>
/// 调引擎做两件事：只读检测开发者模式（<c>--probe-dev-mode</c>），
/// 以及请手表把设置入口显示出来（<c>--reveal-dev-mode</c>）。
/// <para>
/// 两件事都不 enable、不重启、不改系统设置。显示入口成功也不等于已开启——
/// 这句提示必须由界面原样传达给用户。
/// </para>
/// </summary>
internal static class DevModeCheck
{
    /// <summary>两段 30 秒握手加一次 12 秒查询，留出余量。</summary>
    private static readonly TimeSpan Budget = TimeSpan.FromSeconds(90);

    public static async Task<DevModeResult> ProbeAsync(string engine, CancellationToken ct = default)
    {
        var (exit, output, error) = await RunAsync(engine, "--probe-dev-mode", ct);
        if (exit is null) return DevModeResult.Failed(error);

        var hit = FindLast(output, "dev_mode");
        if (hit is null)
            return DevModeResult.Failed($"引擎没有给出检测结论（退出码 {exit}）");

        var state = hit.State switch
        {
            "enabled" => DevModeState.Enabled,
            "disabled" => DevModeState.Disabled,
            _ => DevModeState.Unknown,
        };

        var reason = hit.Reason;
        if (state == DevModeState.Unknown && reason.Length == 0)
            reason = "手表没有给出明确答复";

        return new DevModeResult(state, reason, hit.Watch, hit.WatchDetail);
    }

    public static async Task<RevealOutcome> RevealAsync(string engine, CancellationToken ct = default)
    {
        var (exit, output, error) = await RunAsync(engine, "--reveal-dev-mode", ct);
        if (exit is null) return new RevealOutcome(false, error);

        var hit = FindLast(output, "reveal");
        if (hit is null)
            return new RevealOutcome(false, $"引擎没有给出结果（退出码 {exit}）");

        return new RevealOutcome(hit.Ok, hit.Reason);
    }

    private static EngineEvent? FindLast(string output, string name)
    {
        EngineEvent? found = null;
        foreach (var line in output.Split('\n'))
        {
            var ev = EngineEvents.Parse(line);
            if (ev is not null && ev.Event == name) found = ev;
        }
        return found;
    }

    private static async Task<(int? ExitCode, string Output, string Error)> RunAsync(
        string engine, string arg, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(engine) || !File.Exists(engine))
            return (null, "", "没有找到签名引擎，先在右下角确认引擎路径");

        var psi = new ProcessStartInfo
        {
            FileName = engine,
            WorkingDirectory = Path.GetDirectoryName(engine) ?? AppContext.BaseDirectory,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = new UTF8Encoding(false),
            StandardErrorEncoding = new UTF8Encoding(false),
        };
        psi.ArgumentList.Add(arg);

        using var proc = new Process { StartInfo = psi };
        try
        {
            proc.Start();
            var stdout = proc.StandardOutput.ReadToEndAsync(CancellationToken.None);
            var stderr = proc.StandardError.ReadToEndAsync(CancellationToken.None);

            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
            timeout.CancelAfter(Budget);
            await proc.WaitForExitAsync(timeout.Token);

            return (proc.ExitCode, await stdout, await stderr);
        }
        catch (OperationCanceledException)
        {
            try { proc.Kill(entireProcessTree: true); } catch { }
            return (null, "", $"检测超时（超过 {Budget.TotalSeconds:F0} 秒），手表可能没有连上 iPhone");
        }
        catch (Exception ex)
        {
            return (null, "", "无法运行引擎：" + ex.Message);
        }
    }
}
