using System.IO;
using System.Reflection;
using System.Security.Cryptography;

namespace HermesFix;

/// <summary>
/// 定位要签名的 .ipa。首选内嵌在 exe 里的那一份——不到 1 MB，跟着程序走，
/// 用户双击就能签，不用先去哪个目录里翻文件。手动选过的路径优先于内嵌副本。
/// </summary>
internal static class IpaBundle
{
    private const string ResourceName = "HermesEnabler.ipa";
    private const string Prefix = "HermesEnabler-";

    public static bool HasEmbedded =>
        Assembly.GetExecutingAssembly().GetManifestResourceInfo(ResourceName) is not null;

    /// <summary>内嵌 IPA 的释放目录。文件名带内容指纹，换版本互不覆盖。</summary>
    public static string EmbeddedDir => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "HermesWatch", "ipa");

    /// <summary>这个路径是不是我们自己从 exe 里释放出来的那一份。</summary>
    public static bool IsEmbeddedCopy(string? path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        try
        {
            var full = Path.GetFullPath(path);
            var dir = Path.GetDirectoryName(full);
            return string.Equals(dir, EmbeddedDir, StringComparison.OrdinalIgnoreCase)
                   && Path.GetFileName(full).StartsWith(Prefix, StringComparison.OrdinalIgnoreCase)
                   && Path.GetFileName(full).EndsWith(".ipa", StringComparison.OrdinalIgnoreCase);
        }
        catch { return false; }
    }

    /// <summary>
    /// 把内嵌 IPA 释放到本地缓存并返回路径；同内容已在磁盘上就直接复用。
    /// 没有内嵌资源、或解出来对不上哈希，返回 null。
    /// </summary>
    public static string? ExtractEmbedded()
    {
        try
        {
            using var stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(ResourceName);
            if (stream is null) return null;

            using var ms = new MemoryStream();
            stream.CopyTo(ms);
            var bytes = ms.ToArray();
            if (bytes.Length == 0) return null;

            var hash = Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();
            var target = Path.Combine(EmbeddedDir, $"{Prefix}{hash[..8]}.ipa");

            // 同内容不重写：省一次磁盘写，也避免杀软反复扫同一个文件。
            if (File.Exists(target)
                && new FileInfo(target).Length == bytes.Length
                && string.Equals(Engine.HashFile(target), hash, StringComparison.OrdinalIgnoreCase))
                return target;

            Directory.CreateDirectory(EmbeddedDir);
            File.WriteAllBytes(target, bytes);
            return string.Equals(Engine.HashFile(target), hash, StringComparison.OrdinalIgnoreCase)
                ? target
                : null;
        }
        catch
        {
            // 释放失败不该让程序起不来，下面还有"在旁边找找"这条兜底路。
            return null;
        }
    }

    /// <summary>手动指定过的路径 → 内嵌副本 → 程序附近的 .ipa。全都没有返回空串。</summary>
    public static string Resolve(string? remembered)
    {
        // 用户自己选的优先；但"上次记的是我们释放出来的副本"时不信任它，
        // 重新释放一次，免得换了版本还拿着旧的那份签。
        if (!string.IsNullOrWhiteSpace(remembered)
            && File.Exists(remembered)
            && !IsEmbeddedCopy(remembered))
            return remembered;

        if (ExtractEmbedded() is { Length: > 0 } embedded)
            return embedded;

        return ScanBeside() ?? "";
    }

    /// <summary>
    /// 兜底：源仓库/绿色包那种"ipa 就在旁边"的布局还能用。
    /// 往上级目录走，每层都看 <c>ipa/</c> 子目录，最后看下载目录。
    /// </summary>
    public static string? ScanBeside()
    {
        var found = new List<string>();
        var dir = AppContext.BaseDirectory;

        for (int up = 0; up <= 6; up++)
        {
            var root = dir;
            for (int i = 0; i < up; i++) root = Path.GetFullPath(Path.Combine(root, ".."));

            foreach (var folder in new[] { root, Path.Combine(root, "ipa") })
            {
                var exact = Path.Combine(folder, "HermesEnabler.ipa");
                if (File.Exists(exact)) return exact;

                try
                {
                    found.AddRange(Directory.EnumerateFiles(folder, "*.ipa", SearchOption.TopDirectoryOnly));
                }
                catch { /* 目录没权限就算了，继续往上找 */ }
            }
        }

        try
        {
            var downloads = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads");
            found.AddRange(Directory.EnumerateFiles(downloads, "*.ipa", SearchOption.TopDirectoryOnly));
        }
        catch { }

        // 名字里带 Hermes 的优先，别把别的 App 的 ipa 当成签名对象。
        return found
            .OrderByDescending(p => Path.GetFileName(p).Contains("Hermes", StringComparison.OrdinalIgnoreCase))
            .FirstOrDefault();
    }
}
