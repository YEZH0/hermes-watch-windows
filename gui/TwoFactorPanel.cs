using System.Text.RegularExpressions;

namespace HermesFix;

/// <summary>
/// 从引擎的 stderr 里读出「有哪些受信任号码」。
///
/// 引擎在 main.rs:160-165 每次 2FA 回调都会打：
///     sms supported : true
///     trusted number: id=1 +86 13812345678 (***78) push=true
/// 所以号码列表本来就在，不需要改 Rust，也不需要额外请求 Apple。
/// </summary>
internal static class TwoFactorPanel
{
    internal sealed record TrustedNumber(uint Id, string Masked, bool CanPush);

    private static readonly Regex NumberLine = new(
        @"trusted number:\s*id=(\d+)\s+(.+?)\(\*\*\*(\d+)\)\s+push=(\w+)",
        RegexOptions.Compiled);

    private static readonly Regex SmsFlag = new(
        @"sms supported\s*:\s*(\w+)",
        RegexOptions.Compiled);

    /// <summary>
    /// 解析出号码列表。回调可能被多次调用，同一个 id 以最后一次为准。
    /// 只保留引擎给的后两位做掩码——完整号码是引擎打出来的，这里不碰。
    /// </summary>
    public static List<TrustedNumber> Parse(string raw)
    {
        var byId = new Dictionary<uint, TrustedNumber>();
        if (string.IsNullOrEmpty(raw)) return new List<TrustedNumber>();

        foreach (Match m in NumberLine.Matches(raw))
        {
            if (!uint.TryParse(m.Groups[1].Value, out var id)) continue;
            var canPush = m.Groups[4].Value.Equals("true", StringComparison.OrdinalIgnoreCase);
            byId[id] = new TrustedNumber(id, "***" + m.Groups[3].Value, canPush);
        }

        return byId.Values.OrderBy(n => n.Id).ToList();
    }

    /// <summary>
    /// Apple 是否允许改走短信。没写这一行时按「允许」处理——宁可让用户点一下被拒，
    /// 也不要因为解析不到就把入口藏了。
    /// </summary>
    public static bool SmsSupported(string raw)
    {
        var m = SmsFlag.Match(raw ?? "");
        return !m.Success || m.Groups[1].Value.Equals("true", StringComparison.OrdinalIgnoreCase);
    }
}
