using System.IO;
using System.Reflection;
using System.Security.Cryptography;
using System.Text;

namespace HermesFix;

/// <summary>
/// 定位签名引擎。优先用内嵌在界面程序里的那一份（解出来放到 LocalAppData），
/// 这样别人只需要一个 exe；找不到内嵌资源时退回"同目录 / 上级目录里的 hermesfix.exe"。
/// </summary>
internal static class Engine
{
    private const string ResourceName = "hermesfix.exe";

    /// <summary>最近一次释放/校验算出的内嵌哈希；没走释放流程时为 null。</summary>
    public static string? LastVerifiedHash { get; private set; }

    public static bool HasEmbedded =>
        Assembly.GetExecutingAssembly().GetManifestResourceInfo(ResourceName) is not null;

    /// <summary>内嵌引擎会被释放到哪里。只算路径，不触发释放。</summary>
    public static string EmbeddedPath
    {
        get
        {
            var version = Assembly.GetExecutingAssembly().GetName().Version?.ToString() ?? "0";
            return Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "HermesWatch", "engine", "v" + version, ResourceName);
        }
    }

    /// <summary>这个路径是不是我们自己释放出来的那一份。</summary>
    public static bool IsEmbeddedCopy(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        try { return string.Equals(Path.GetFullPath(path), EmbeddedPath, StringComparison.OrdinalIgnoreCase); }
        catch { return false; }
    }

    /// <summary>给界面看的一行描述：来源 + 版本 + 指纹（只对可信来源给指纹）。</summary>
    public static string Describe(string path)
    {
        if (string.IsNullOrWhiteSpace(path) || !File.Exists(path)) return "未找到 hermesfix.exe";
        if (!IsEmbeddedCopy(path)) return "外部引擎 · 未校验";

        var version = Assembly.GetExecutingAssembly().GetName().Version?.ToString(3) ?? "0";
        return LastVerifiedHash is { Length: >= 8 } h
            ? $"内嵌 v{version} · {h[..8]}"
            : $"内嵌 v{version}";
    }

    /// <summary>把内嵌引擎释放到磁盘，返回可执行文件路径。失败返回 null。</summary>
    public static string? ExtractEmbedded()
    {
        try
        {
            var asm = Assembly.GetExecutingAssembly();
            using var stream = asm.GetManifestResourceStream(ResourceName);
            if (stream is null) return null;

            var target = EmbeddedPath;
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            var marker = target + ".sha256";

            var hash = Hash(stream);
            LastVerifiedHash = hash;
            // 标记文件只证明"上次释放过什么"，证明不了"现在磁盘上是什么"。
            // 再算一次实际哈希，堵住同尺寸替换的路子——这个引擎要拿 Apple ID 密码。
            if (File.Exists(target)
                && new FileInfo(target).Length == stream.Length
                && File.Exists(marker)
                && File.ReadAllText(marker, Encoding.ASCII).Trim() == hash
                && HashFile(target) == hash)
            {
                return target;
            }

            // 内容不一致（或首次运行）就重新释放
            stream.Position = 0;
            var temp = target + ".tmp";
            using (var output = File.Create(temp))
                stream.CopyTo(output);
            File.Move(temp, target, overwrite: true);
            File.WriteAllText(marker, hash, Encoding.ASCII);
            return target;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>同目录 / 上级 6 层里的 hermesfix.exe，用于没有内嵌资源的构建。</summary>
    public static string? ProbeBeside()
    {
        var dir = AppContext.BaseDirectory;
        var candidates = new List<string> { Path.Combine(dir, ResourceName) };
        for (int up = 1; up <= 6; up++)
        {
            var root = dir;
            for (int i = 0; i < up; i++) root = Path.GetFullPath(Path.Combine(root, ".."));
            candidates.Add(Path.Combine(root, ResourceName));
            candidates.Add(Path.Combine(root, "bin", ResourceName));
        }
        return candidates.FirstOrDefault(File.Exists);
    }

    /// <summary>先试内嵌，再试磁盘；都没有返回 null。</summary>
    public static string? Resolve() => ExtractEmbedded() ?? ProbeBeside();

    public static string Hash(Stream stream)
    {
        using var sha = SHA256.Create();
        var bytes = sha.ComputeHash(stream);
        return Convert.ToHexString(bytes).ToLowerInvariant();
    }

    public static string HashFile(string path)
    {
        using var fs = File.OpenRead(path);
        return Hash(fs);
    }

    /// <summary>释放到磁盘后的大小，用于自检展示。</summary>
    public static long EmbeddedLength
    {
        get
        {
            try
            {
                using var s = Assembly.GetExecutingAssembly().GetManifestResourceStream(ResourceName);
                return s?.Length ?? 0;
            }
            catch { return 0; }
        }
    }
}
