using System.Text;

namespace HermesFix;

/// <summary>
/// 把管道里任意切分的输出块还原成"整行"再交出去。
///
/// 引擎是按行输出的（eprintln!/println!），但子进程管道给过来的块边界是随机的，
/// 一行可能横跨两个块。脱敏规则和阶段标记都是按一行的形状写的——比如
/// <c>trusted number: id=1 +86 13812345678 (***78) push=true</c>，从中间切两半
/// 分别过正则就认不出号码了。所以必须先攒够一整行，再过 AppendLog。
///
/// 唯一不按行的是提示串（引擎用 print! 打，故意不带换行），它会留在尾巴里，
/// 由调用方做尾部匹配。
/// </summary>
internal static class LineAssembler
{
    /// <summary>没有换行时最多攒这么多就硬吐出去，避免畸形输出把内存吃满。</summary>
    public const int MaxTail = 8192;

    /// <summary>
    /// 取出 <paramref name="pending"/> 里已经成行的部分（含换行符），剩下的留在里面。
    /// 一行都没成、又没超上限时返回 null。
    /// </summary>
    public static string? TakeCompleteLines(StringBuilder pending, int maxTail = MaxTail)
    {
        if (pending.Length == 0) return null;

        var buf = pending.ToString();
        var lastNl = buf.LastIndexOf('\n');
        if (lastNl >= 0)
        {
            pending.Remove(0, lastNl + 1);
            return buf[..(lastNl + 1)];
        }

        if (pending.Length > maxTail)
        {
            pending.Clear();
            return buf;
        }

        return null;
    }
}
