using System.Text.Json;

namespace HermesFix;

/// <summary>
/// 引擎发出来的一行结构事件。字段是契约：Rust 侧 <c>hermesfix/src/events.rs</c>
/// 里写什么，这里就必须读什么，改名字要两边一起改。
/// </summary>
internal sealed class EngineEvent
{
    public string Event { get; init; } = "";
    public string Stage { get; init; } = "";
    public string StageLabel { get; init; } = "";
    public string State { get; init; } = "";
    public string Code { get; init; } = "";
    public string Title { get; init; } = "";
    public string Action { get; init; } = "";
    public bool Recoverable { get; init; }
    public string Retry { get; init; } = "";
    public string Detail { get; init; } = "";
    public string CacheId { get; init; } = "";

    // 开发者模式检测专用：state 是 enabled/disabled/unknown，reason 说明
    // 「为什么不知道」，watch 是连接层结论（WATCH_LOCKED / WATCH_UNREACHABLE…）。
    public string Reason { get; init; } = "";
    public string Watch { get; init; } = "";
    public string WatchDetail { get; init; } = "";
    public bool Ok { get; init; }

    /// <summary>pair 事件里是 phone 或 watch，说明现在轮到谁点「信任」。</summary>
    public string Scope { get; init; } = "";
}

internal static class EngineEvents
{
    public const string Prefix = "@@HF_EVENT@@";

    /// <summary>解析一行；不是事件行返回 null。</summary>
    public static EngineEvent? Parse(string line)
    {
        if (string.IsNullOrEmpty(line)) return null;
        var at = line.IndexOf(Prefix, StringComparison.Ordinal);
        if (at < 0) return null;

        var json = line[(at + Prefix.Length)..].Trim();
        if (json.Length == 0) return null;

        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object) return null;

            string Str(string name) =>
                root.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String
                    ? v.GetString() ?? ""
                    : "";

            bool Bool(string name) =>
                root.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.True;

            return new EngineEvent
            {
                Event = Str("event"),
                Stage = Str("stage"),
                // stage 事件把名字放在 label，failure 事件放在 stage_label
                StageLabel = Str("stage_label") is { Length: > 0 } named ? named : Str("label"),
                State = Str("state"),
                Code = Str("code"),
                Title = Str("title"),
                Action = Str("action"),
                Recoverable = Bool("recoverable"),
                Retry = Str("retry"),
                Detail = Str("detail"),
                // resume 事件里 id 字段是缓存 id；其余事件没有这个键
                CacheId = Str("id"),
                Reason = Str("reason"),
                Watch = Str("watch"),
                WatchDetail = Str("watch_detail"),
                Ok = Bool("ok"),
                Scope = Str("scope"),
            };
        }
        catch (JsonException)
        {
            // 引擎输出被截断或掺了别的东西时不该炸掉界面
            return null;
        }
    }

    /// <summary>阶段名 → 界面上的第几个步骤（1 起）。</summary>
    public static int StepIndex(string stage) => stage switch
    {
        "login" => 1,
        "sign" => 2,
        "iphone" => 3,
        "watch_connect" => 4,
        "watch_install" => 5,
        _ => 0,
    };
}
