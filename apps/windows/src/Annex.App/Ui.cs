using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace Annex.App;

/// <summary>
/// Building blocks for Annex's black-and-white look (the same as the Mac app), built in code: plain
/// page titles, small uppercase section labels, flat grouped rows (title and description on the left,
/// a control on the right), white main buttons and the green "live" dot.
/// </summary>
internal static class Ui
{
    public static T Res<T>(string key) => (T)Application.Current.Resources[key];

    public static Brush Secondary => Res<Brush>("TextFillColorSecondaryBrush");
    public static Brush Tertiary => Res<Brush>("TextFillColorTertiaryBrush");

    /// <summary>White at <paramref name="alpha"/>: every fill and line in the app is one of these.</summary>
    public static SolidColorBrush White(double alpha) =>
        new(Windows.UI.Color.FromArgb((byte)Math.Round(alpha * 255), 255, 255, 255));

    public static SolidColorBrush Black(double alpha) =>
        new(Windows.UI.Color.FromArgb((byte)Math.Round(alpha * 255), 5, 5, 5));

    /// <summary>The one colour: something live (a session, a computer that's sharing).</summary>
    public static SolidColorBrush Live => new(Windows.UI.Color.FromArgb(255, 61, 209, 133));

    public static string AssetPath(string name) => Path.Combine(AppContext.BaseDirectory, "Assets", name);

    public static Image AppIcon(double size) => new()
    {
        Source = new BitmapImage(new Uri(AssetPath("Annex.png"))),
        Width = size,
        Height = size,
    };

    public static FontIcon Glyph(string glyph, double size = 16) => new() { Glyph = glyph, FontSize = size };

    public static TextBlock Text(string text, string? style = null, Brush? brush = null, bool wrap = true)
    {
        var t = new TextBlock { Text = text, TextWrapping = wrap ? TextWrapping.Wrap : TextWrapping.NoWrap, TextTrimming = TextTrimming.CharacterEllipsis };
        if (style is not null) t.Style = Res<Style>(style);
        if (brush is not null) t.Foreground = brush;
        return t;
    }

    /// <summary>The page's content column: a scrolling stack with Settings' margins.</summary>
    public static StackPanel Page(out ScrollViewer scroller)
    {
        var stack = new StackPanel { Spacing = 4, Padding = new Thickness(40, 28, 40, 40), MaxWidth = 1000 };
        scroller = new ScrollViewer { Content = stack, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        return stack;
    }

    /// <summary>The top of a page: a plain title and one line saying what the page is for. Content,
    /// not chrome, so no card and no icon (<paramref name="glyph"/> is kept for callers, unused).</summary>
    public static UIElement Hero(string glyph, string title, string description, UIElement? icon = null)
    {
        var text = new StackPanel { Spacing = 4, Margin = new Thickness(0, 4, 0, 18) };
        if (icon is FrameworkElement fe)
        {
            fe.Margin = new Thickness(0, 0, 0, 10);
            fe.HorizontalAlignment = HorizontalAlignment.Left;
            text.Children.Add(fe);
        }
        text.Children.Add(Title(title));
        text.Children.Add(Text(description, "BodyTextBlockStyle", Secondary));
        return text;
    }

    /// <summary>A page title: big and semibold.</summary>
    public static TextBlock Title(string text)
    {
        var t = Text(text, wrap: false);
        t.FontSize = 28;
        t.FontWeight = FontWeights.SemiBold;
        return t;
    }

    /// <summary>A small uppercase label over a group.</summary>
    public static TextBlock Eyebrow(string text)
    {
        var t = Text(text.ToUpperInvariant(), "CaptionTextBlockStyle", Secondary, wrap: false);
        t.FontWeight = FontWeights.SemiBold;
        t.CharacterSpacing = 120;
        return t;
    }

    public static TextBlock Section(string title)
    {
        var t = Eyebrow(title);
        t.Margin = new Thickness(2, 22, 0, 8);
        return t;
    }

    /// <summary>A section heading with a small control (e.g. refresh) at its right end.</summary>
    public static Grid Section(string title, UIElement action)
    {
        var grid = new Grid { Margin = new Thickness(2, 16, 0, 4) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var t = Eyebrow(title);
        t.VerticalAlignment = VerticalAlignment.Center;
        grid.Children.Add(t);
        if (action is FrameworkElement fe)
        {
            fe.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(fe, 1);
            grid.Children.Add(fe);
        }
        return grid;
    }

    /// <summary>A quiet icon-only button (no fill until hovered), with a tooltip.</summary>
    public static Button IconButton(string glyph, string tooltip, Action click)
    {
        var b = new Button
        {
            Content = Glyph(glyph, 14),
            Padding = new Thickness(6),
            MinWidth = 0,
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
            BorderThickness = new Thickness(0),
            Foreground = Secondary,
        };
        ToolTipService.SetToolTip(b, tooltip);
        AutomationProperties.SetName(b, tooltip);
        b.Click += (_, _) => click();
        return b;
    }

    /// <summary>Marks a <see cref="Card"/>; consecutive ones on a page become one group (<see cref="JoinRows"/>).</summary>
    public const string RowTag = "annex-row";

    /// <summary>A setting row: title and description on the left, the control on the right. Rows carry no
    /// icon, like the About page; <paramref name="glyph"/> shows only as a status icon, when
    /// <paramref name="glyphBrush"/> colours it. Consecutive rows on a page are joined into one rounded group.</summary>
    public static Border Card(string glyph, string title, string? description = null, UIElement? control = null, Brush? glyphBrush = null)
    {
        FontIcon? status = null;
        if (glyphBrush is not null)
        {
            status = Glyph(glyph, 18);
            status.Foreground = glyphBrush;
        }
        return new Border
        {
            Tag = RowTag,
            Child = GroupRow(title, description, control, status),
            Padding = new Thickness(16, 10, 16, 10),
            CornerRadius = new CornerRadius(14),
            Background = White(0.045),
            BorderBrush = White(0.08),
            BorderThickness = new Thickness(1),
            Margin = new Thickness(0, 0, 0, 2),
        };
    }

    /// <summary>Joins each run of consecutive <see cref="Card"/> rows on a page into one <see cref="Group"/>
    /// with thin dividers, so every page uses the same grouped design as About.</summary>
    public static void JoinRows(Panel page)
    {
        var children = page.Children.ToList();
        page.Children.Clear();
        var run = new List<UIElement>();
        void Flush()
        {
            if (run.Count == 0) return;
            page.Children.Add(Group([.. run]));
            run.Clear();
        }
        foreach (var child in children)
        {
            if (child is Border { Tag: RowTag } row && row.Child is { } content)
            {
                row.Child = null;
                run.Add(content);
            }
            else
            {
                Flush();
                page.Children.Add(child);
            }
        }
        Flush();
    }

    /// <summary>One row of a <see cref="Group"/>: title and description on the left, a control on the right.</summary>
    public static Grid GroupRow(string title, string? description = null, UIElement? control = null, UIElement? leading = null)
    {
        var grid = new Grid { ColumnSpacing = 12, MinHeight = 40 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        if (leading is FrameworkElement le)
        {
            le.VerticalAlignment = VerticalAlignment.Center;
            grid.Children.Add(le);
        }
        var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Spacing = 2 };
        text.Children.Add(Text(title, "BodyTextBlockStyle"));
        if (!string.IsNullOrEmpty(description)) text.Children.Add(Text(description, "CaptionTextBlockStyle", Secondary));
        Grid.SetColumn(text, 1);
        grid.Children.Add(text);
        if (control is FrameworkElement fe)
        {
            fe.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(fe, 2);
            Add(grid, fe);
        }
        return grid;
    }

    /// <summary>Rows in one rounded card, separated by thin lines.</summary>
    public static Border Group(params UIElement?[] rows)
    {
        var stack = new StackPanel();
        foreach (var row in rows)
        {
            if (row is null) continue;
            if (stack.Children.Count > 0)
                stack.Children.Add(new Border { Height = 1, Background = White(0.07), Margin = new Thickness(16, 0, 16, 0) });
            var cell = new Border { Padding = new Thickness(16, 10, 16, 10), Child = row };
            stack.Children.Add(cell);
        }
        return new Border
        {
            Child = stack,
            CornerRadius = new CornerRadius(14),
            Background = White(0.045),
            BorderBrush = White(0.08),
            BorderThickness = new Thickness(1),
            Margin = new Thickness(0, 0, 0, 2),
        };
    }

    /// <summary>A switch with its "On"/"Off" label on the left, like Settings.</summary>
    public static StackPanel Switch(bool on, Action<bool> changed, bool enabled = true)
    {
        var label = Text(on ? "On" : "Off", "BodyTextBlockStyle", Secondary, wrap: false);
        label.VerticalAlignment = VerticalAlignment.Center;
        var t = new ToggleSwitch { IsOn = on, IsEnabled = enabled, OnContent = "", OffContent = "", MinWidth = 0 };
        t.Toggled += (_, _) =>
        {
            label.Text = t.IsOn ? "On" : "Off";
            changed(t.IsOn);
        };
        var p = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        p.Children.Add(label);
        p.Children.Add(t);
        return p;
    }

    public static Button Button(string text, Action click, bool accent = false)
    {
        var b = new Button { Content = text, MinWidth = 96 };
        if (accent) b.Style = Res<Style>("AccentButtonStyle");
        b.Click += (_, _) => click();
        return b;
    }

    /// <summary>A button with its own fill, hover and text colours (WinUI's lightweight styling), so it
    /// looks the same in every state: white with black text for the main action, faint white otherwise.</summary>
    public static Button Flat(object content, Brush fill, Brush hover, Brush text, Action? click = null, double radius = 10)
    {
        var b = new Button
        {
            Content = content,
            Background = fill,
            Foreground = text,
            BorderThickness = new Thickness(0),
            CornerRadius = new CornerRadius(radius),
            HorizontalContentAlignment = HorizontalAlignment.Left,
        };
        b.Resources["ButtonBackgroundPointerOver"] = hover;
        b.Resources["ButtonBackgroundPressed"] = fill;
        b.Resources["ButtonForegroundPointerOver"] = text;
        b.Resources["ButtonForegroundPressed"] = text;
        b.Resources["ButtonBackgroundDisabled"] = fill;
        b.Resources["ButtonForegroundDisabled"] = Secondary;
        b.Resources["ButtonBorderBrushPointerOver"] = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        b.Resources["ButtonBorderBrushPressed"] = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        b.Resources["ButtonBorderBrushDisabled"] = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        if (click is not null) b.Click += (_, _) => click();
        return b;
    }

    /// <summary>A small capsule button: white for the main action, faint for the other.</summary>
    public static Button Pill(string text, Action click, bool prominent)
    {
        var label = Text(text, wrap: false);
        label.FontSize = 12;
        label.FontWeight = FontWeights.SemiBold;
        var b = Flat(label, prominent ? White(0.92) : White(0.1), prominent ? White(1) : White(0.16),
            prominent ? Black(1) : White(1), click, radius: 13);
        b.Padding = new Thickness(12, 3, 12, 4);
        b.MinHeight = 26;
        return b;
    }

    /// <summary>A green dot: something live.</summary>
    public static Microsoft.UI.Xaml.Shapes.Ellipse LiveDot(double size = 8) => new()
    {
        Width = size,
        Height = size,
        Fill = Live,
        VerticalAlignment = VerticalAlignment.Center,
    };

    /// <summary>A rounded, faintly filled panel.</summary>
    public static Border Panel(UIElement child, double radius = 18, double fill = 0.045, Thickness? padding = null) => new()
    {
        Child = child,
        CornerRadius = new CornerRadius(radius),
        Background = White(fill),
        BorderBrush = White(0.08),
        BorderThickness = new Thickness(1),
        Padding = padding ?? new Thickness(16),
    };

    public static StackPanel Row(params UIElement[] children)
    {
        var p = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        foreach (var c in children) Add(p, c);
        return p;
    }

    public static InfoBar Info(string title, string message, InfoBarSeverity severity) => new()
    {
        Title = title,
        Message = message,
        Severity = severity,
        IsOpen = true,
        IsClosable = false,
        Margin = new Thickness(0, 0, 0, 8),
    };

    public static TextBlock Bold(string text)
    {
        var t = Text(text, "BodyTextBlockStyle");
        t.FontWeight = FontWeights.SemiBold;
        return t;
    }

    // Which panel each element was last added to. FrameworkElement.Parent isn't reliably set for
    // elements added from code, so long-lived controls are detached through this record.
    private static readonly System.Runtime.CompilerServices.ConditionalWeakTable<UIElement, Panel> s_owners = new();

    private static void Add(Panel panel, UIElement child)
    {
        panel.Children.Add(child);
        s_owners.AddOrUpdate(child, panel);
    }

    /// <summary>Detaches a long-lived control (e.g. a text box being typed in) before it joins a rebuilt page.</summary>
    public static T Reuse<T>(T element) where T : FrameworkElement
    {
        if (s_owners.TryGetValue(element, out var owner))
        {
            owner.Children.Remove(element);
            s_owners.Remove(element);
        }
        // Belt and braces: whatever WinUI reports as the parent.
        switch (element.Parent ?? VisualTreeHelper.GetParent(element))
        {
            case Panel p: p.Children.Remove(element); break;
            case Border b: b.Child = null; break;
            case ContentControl c: c.Content = null; break;
        }
        return element;
    }
}
