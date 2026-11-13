using System.Net.Http;
using System.Text.Json;

namespace HermesFix;

/// <summary>
/// 查上游 HermesEnabler 的最新 release 版本。纯只读：不下载、不替换、不校验上游哈希
/// （上游 release 本身也不提供 sha256），更不会因为版本旧就拦签名。
///
/// 只在用户主动打开「环境自检」时调用，8 小时内复用结果——GitHub 未认证接口
/// 每小时只有 60 次额度，不值得在每次启动时消耗。
/// </summary>
internal static class ReleaseCheck
{
    /// <summary>
    /// 仓库 <c>ipa/HermesEnabler.ipa</c> 里带的版本。这里只说"附带的"，
    /// 不说"验证过的"——真机那次跑通的是 1.5.1，1.5.4 只做了结构比对
    /// （bundle id、可执行名、fat 架构、entitlements 依赖的结构都没变）。
    /// </summary>
    private const string BundledBaseline = "1.5.4";

    private const string Api = "https://api.github.com/repos/pookjw/HermesEnabler/releases/latest";

    private static readonly HttpClient Http = new() { Timeout = TimeSpan.FromSeconds(6) };
    private static CheckItem? _cached;
    private static DateTime _cachedAt = DateTime.MinValue;

    public static async Task<CheckItem> QueryAsync()
    {
        if (_cached is not null && DateTime.UtcNow - _cachedAt < TimeSpan.FromHours(8))
            return _cached;

        var item = await FetchAsync();
        _cached = item;
        _cachedAt = DateTime.UtcNow;
        return item;
    }

    private static async Task<CheckItem> FetchAsync()
    {
        try
        {
            using var req = new HttpRequestMessage(HttpMethod.Get, Api);
            req.Headers.UserAgent.ParseAdd("hermes-watch-windows");
            req.Headers.Accept.ParseAdd("application/vnd.github+json");

            using var resp = await Http.SendAsync(req).ConfigureAwait(false);
            if (!resp.IsSuccessStatusCode)
                return Offline($"查不到上游版本（HTTP {(int)resp.StatusCode}）");

            using var stream = await resp.Content.ReadAsStreamAsync().ConfigureAwait(false);
            using var doc = await JsonDocument.ParseAsync(stream).ConfigureAwait(false);

            var tag = doc.RootElement.TryGetProperty("tag_name", out var t)
                ? (t.GetString() ?? "").TrimStart('v', 'V')
                : "";

            if (tag.Length == 0) return Offline("上游返回里没有版本号");

            return tag == BundledBaseline
                ? new CheckItem("上游版本", CheckState.Ok,
                    $"上游最新 {tag}，与仓库里附带的一致")
                : new CheckItem("上游版本", CheckState.Warn,
                    $"上游已发布 {tag}，仓库里附带的是 {BundledBaseline}",
                    $"新版本通常也能签，但没经过本仓库验证。想稳妥就用 ipa/ 里那份 {BundledBaseline}");
        }
        catch (Exception ex)
        {
            // 离线、限流、DNS 挂掉都走这儿。这不是错误，不拦签名。
            return Offline("连不上 GitHub：" + ex.GetType().Name);
        }
    }

    private static CheckItem Offline(string why) =>
        new("上游版本", CheckState.Warn, why, "不影响签名，跳过即可");
}
