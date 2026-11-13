using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Shapes;

namespace HermesFix;

public partial class CheckWindow : Window
{
    private readonly string _ipa;
    private List<CheckItem> _items = new();

    public CheckWindow(string ipaPath)
    {
        InitializeComponent();
        _ipa = ipaPath;
        Loaded += async (_, _) => await RefreshAsync();
    }

    private async Task RefreshAsync()
    {
        RerunBtn.IsEnabled = false;
        Subtitle.Text = "正在检测…";

        try
        {
            Rows.Children.Clear();

            _items = await EnvCheck.RunAllAsync(_ipa);

            // 上游版本是锦上添花的一行：它出任何问题都不能影响本地这几项的结论，
            // 更不能让整个窗口停在半截。所以这里再兜一层。
            try
            {
                _items.Add(await ReleaseCheck.QueryAsync());
            }
            catch (Exception ex)
            {
                _items.Add(new CheckItem("上游版本", CheckState.Warn,
                    "检查出错：" + ex.GetType().Name, "不影响签名，跳过即可"));
            }

            Render(_items);

            var fail = _items.Count(i => i.State == CheckState.Fail);
            var warn = _items.Count(i => i.State == CheckState.Warn);
            Subtitle.Text = fail > 0
                ? $"有 {fail} 项未通过，按提示补上再开始签名。"
                : warn > 0
                    ? $"全部必需项通过，另有 {warn} 项提醒。"
                    : "全部通过，可以开始签名。";
        }
        catch (Exception ex)
        {
            // 自检窗口本身不能静默失败——停在半截比报错更难查。
            Subtitle.Text = $"检测中断：{ex.GetType().Name} · {ex.Message}";
        }
        finally
        {
            RerunBtn.IsEnabled = true;
        }
    }

    private void Render(List<CheckItem> items)
    {
        foreach (var item in items)
        {
            var card = new Border
            {
                Background = Hex("#161A21"),
                BorderBrush = Hex("#242A34"),
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(9),
                Padding = new Thickness(14, 11, 14, 11),
                Margin = new Thickness(0, 0, 0, 8),
            };

            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

            var badge = new Border
            {
                Background = BadgeBg(item.State),
                BorderBrush = BadgeFg(item.State),
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(20),
                Padding = new Thickness(9, 2, 9, 2),
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 1, 12, 0),
                Child = new TextBlock
                {
                    Text = item.Badge,
                    FontSize = 11,
                    Foreground = BadgeFg(item.State),
                },
            };
            Grid.SetColumn(badge, 0);
            grid.Children.Add(badge);

            var text = new StackPanel();
            text.Children.Add(new TextBlock
            {
                Text = item.Name,
                FontSize = 13,
                FontWeight = FontWeights.SemiBold,
                Foreground = Hex("#E8ECF2"),
            });
            text.Children.Add(new TextBlock
            {
                Text = item.Detail,
                FontSize = 11.5,
                Foreground = Hex("#8A94A6"),
                TextWrapping = TextWrapping.Wrap,
                Margin = new Thickness(0, 4, 0, 0),
            });
            if (!string.IsNullOrWhiteSpace(item.Hint))
            {
                text.Children.Add(new TextBlock
                {
                    Text = "→ " + item.Hint,
                    FontSize = 11.5,
                    Foreground = Hex("#7C8595"),
                    TextWrapping = TextWrapping.Wrap,
                    Margin = new Thickness(0, 5, 0, 0),
                });
            }
            Grid.SetColumn(text, 1);
            grid.Children.Add(text);

            card.Child = grid;
            Rows.Children.Add(card);
        }
    }

    private static SolidColorBrush Hex(string hex) =>
        new((Color)ColorConverter.ConvertFromString(hex));

    private static SolidColorBrush BadgeFg(CheckState s) => s switch
    {
        CheckState.Ok => Hex("#3FB950"),
        CheckState.Warn => Hex("#E3B341"),
        CheckState.Fail => Hex("#F85149"),
        _ => Hex("#8A94A6"),
    };

    private static SolidColorBrush BadgeBg(CheckState s) => s switch
    {
        CheckState.Ok => Hex("#131C16"),
        CheckState.Warn => Hex("#241F10"),
        CheckState.Fail => Hex("#1F1414"),
        _ => Hex("#171B22"),
    };

    private async void Rerun_Click(object sender, RoutedEventArgs e) => await RefreshAsync();

    private void Copy_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            Clipboard.SetText(EnvCheck.Report(_items));
            Subtitle.Text = "报告已复制到剪贴板。";
        }
        catch
        {
            Subtitle.Text = "复制失败，剪贴板被其它程序占用。";
        }
    }

    private void Close_Click(object sender, RoutedEventArgs e) => Close();
}
