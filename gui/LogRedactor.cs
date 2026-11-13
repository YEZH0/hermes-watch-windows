using System.Text.RegularExpressions;

namespace HermesFix;

/// <summary>
/// 把日志里不该外传的东西换成掩码。在 AppendLog 入口统一过一遍：
/// 这样屏幕上、_logAll 里、以及以后导出的文件里，都是同一份脱敏内容。
///
/// 只处理能确定格式的几种，宁可漏也不误伤——日志是用来排错的，
/// 把阶段标记和错误码改坏了，比漏掉一个号码更麻烦。
/// 验证码和密码不在这里管：它们在 Send() 里就只回显动作，从来没写进来过。
/// </summary>
internal static class LogRedactor
{
    // 引擎在 main.rs:162 打的是 number_with_dial_code（完整号码）。
    // 这一行的形状是固定的，按整行形状改最稳。
    //    trusted number: id=1 +86 13812345678 (***78) push=true
    private static readonly Regex TrustedNumber = new(
        @"(trusted number:\s*id=\d+\s+)(.+?)(\s+\(\*\*\*\d+\))",
        RegexOptions.Compiled);

    // Apple ID 邮箱：保留首字符和域名，中间抹掉。
    private static readonly Regex Email = new(
        @"([A-Za-z0-9._%+-])[A-Za-z0-9._%+-]*(@[A-Za-z0-9.-]+\.[A-Za-z]{2,})",
        RegexOptions.Compiled);

    // Windows 用户目录：C:\Users\张三\... → C:\Users\<user>\...
    private static readonly Regex UserPath = new(
        @"([A-Za-z]:[\\/]Users[\\/])[^\\/\r\n\t ]+",
        RegexOptions.Compiled);

    // 设备标识。main.rs:283 打的是 "using iPhone {udid}"，那是设备的唯一编号，
    // 换台设备就换一个，留着等于留了个可追踪的标识。
    // 只掩码，不整行删：排错时还要靠这行确认"连上的是不是这台"。
    private static readonly Regex DeviceLine = new(
        @"(using\s+(?:iPhone|iPad|iPod|Apple\s*Watch)\s+)([0-9A-Fa-f][0-9A-Fa-f-]{14,})",
        RegexOptions.Compiled);

    // 带标签的形式（引擎出错信息里可能出现 UDID: xxx），按标签认，不猜。
    private static readonly Regex LabelledUdid = new(
        @"(\budid\s*[:=]\s*)([0-9A-Fa-f][0-9A-Fa-f-]{14,})",
        RegexOptions.Compiled | RegexOptions.IgnoreCase);

    /// <summary>留头 4 位尾 4 位：够判断是不是同一台设备，又不足以还原本体。</summary>
    private static string MaskId(string id)
    {
        var s = id.Trim();
        return s.Length <= 10 ? "***" : s[..4] + "…" + s[^4..];
    }

    /// <summary>清空日志时也要一起复位的是调用方的事，这里只做文本替换。</summary>
    public static string Redact(string text)
    {
        if (string.IsNullOrEmpty(text)) return text;

        // 绝大多数 chunk 不含敏感内容，先花常数时间把它们挡掉。
        var maybe = text.IndexOf('@') >= 0
                    || text.IndexOf("trusted number", StringComparison.Ordinal) >= 0
                    || text.IndexOf("using ", StringComparison.Ordinal) >= 0
                    || text.IndexOf("udid", StringComparison.OrdinalIgnoreCase) >= 0
                    || text.IndexOf("Users\\", StringComparison.Ordinal) >= 0
                    || text.IndexOf("Users/", StringComparison.Ordinal) >= 0;
        if (!maybe) return text;

        var s = TrustedNumber.Replace(text, "$1***$3");
        s = UserPath.Replace(s, "$1<user>");
        s = DeviceLine.Replace(s, m => m.Groups[1].Value + MaskId(m.Groups[2].Value));
        s = LabelledUdid.Replace(s, m => m.Groups[1].Value + MaskId(m.Groups[2].Value));
        s = Email.Replace(s, "$1***$2");
        return s;
    }
}
