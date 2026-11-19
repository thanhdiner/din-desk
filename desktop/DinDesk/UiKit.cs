namespace MuseDesk;

internal static class UiTheme
{
    public static readonly Color Background = SystemColors.Window;
    public static readonly Color Sidebar = SystemColors.Window;
    public static readonly Color Surface = SystemColors.Window;
    public static readonly Color Input = SystemColors.Window;
    public static readonly Color Text = SystemColors.WindowText;
    public static readonly Color Muted = SystemColors.WindowText;
    public static readonly Color Accent = SystemColors.WindowText;
    public static readonly Color Success = SystemColors.WindowText;
    public static readonly Color Warning = SystemColors.WindowText;
    public static readonly Color Error = SystemColors.WindowText;

    public static Label Label(string text, float size = 10, Color? color = null, bool bold = false)
        => new() { Text = text, UseMnemonic = false, AutoSize = false, Dock = DockStyle.Fill,
            ForeColor = color ?? Text, BackColor = Color.Transparent, Margin = Padding.Empty,
            Font = new Font(SystemFonts.MessageBoxFont ?? Control.DefaultFont, bold ? FontStyle.Bold : FontStyle.Regular),
            TextAlign = ContentAlignment.MiddleLeft };
}

internal sealed class SurfacePanel : Panel
{
    public Color Fill { get; set; } = UiTheme.Surface;
    public SurfacePanel() { BackColor = UiTheme.Surface; Padding = new Padding(8); Margin = Padding.Empty; }
}

internal class DeskButton : Button
{
    public bool Primary { get; init; }
    public string? Glyph { get; init; }
    public bool Navigation { get; init; }
    public bool Selected
    {
        get => selected;
        set { selected = value; Font = new Font(SystemFonts.MessageBoxFont ?? Control.DefaultFont, value ? FontStyle.Bold : FontStyle.Regular); }
    }
    private bool selected;
    public DeskButton()
    {
        FlatStyle = FlatStyle.System; UseMnemonic = false;
        Size = new Size(150, 30); Margin = new Padding(0, 6, 10, 0);
    }
    protected override void OnTextChanged(EventArgs e) { AccessibleName = Text; base.OnTextChanged(e); }
}

internal sealed class StatusBadge : Label
{
    public Color Tone { get => ForeColor; set => ForeColor = value; }
    public StatusBadge() { Size = new Size(172, 32); TextAlign = ContentAlignment.MiddleLeft; }
}

internal sealed class DeskGrid : TableLayoutPanel { }
