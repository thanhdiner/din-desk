using System.Runtime.InteropServices;

namespace MuseDesk;

internal sealed partial class MainForm
{
    private readonly TabControl tabs = new() { Dock = DockStyle.Fill, Name = "MainTabs" };
    private void BuildLayout()
    {
        var content = Grid(1, 4); content.Padding = new Padding(12);
        content.RowStyles.Add(new RowStyle(SizeType.Absolute, 30)); content.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
        content.RowStyles.Add(new RowStyle(SizeType.Absolute, 30)); content.RowStyles.Add(new RowStyle(SizeType.Absolute, 28));
        status.Dock = DockStyle.Fill;
        content.Controls.Add(status, 0, 0);
        pages.AddRange(new[] { OverviewPage(), SavedAccountsPage(), PreferencesPage(), HelpPage() });
        string[] names = { "Bắt đầu", "Tài khoản", "Cài đặt", "Hướng dẫn" };
        for (int i = 0; i < pages.Count; i++)
        {
            var tab = new TabPage(names[i]) { Padding = new Padding(8), BackColor = SystemColors.Window };
            tab.Controls.Add(pages[i]); tabs.TabPages.Add(tab);
        }
        tabs.SelectedIndexChanged += (_, _) => notice.Text = "";
        content.Controls.Add(tabs, 0, 1); content.Controls.Add(notice, 0, 2);
        var footer = Grid(2, 1);
        footer.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100)); footer.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 100));
        footer.Controls.Add(footerState, 0, 0);
        var exit = new Button { Text = "Thoát", Dock = DockStyle.Fill, Name = "Thoát ứng dụng", AccessibleName = "Thoát ứng dụng" };
        exit.Click += (_, _) => ExitApp(); footer.Controls.Add(exit, 1, 0);
        content.Controls.Add(footer, 0, 3); Controls.Add(content); Navigate(0);
    }

    private Control OverviewPage()
    {
        var page = Stack();
        var cards = Grid(3, 1); cards.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 50)); cards.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 16)); cards.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 50));
        var muse = Card(); var museBody = Stack();
        Add(museBody, UiTheme.Label("01   TÀI KHOẢN AI", 8.5f, UiTheme.Accent, true), 25);
        Add(museBody, accountState, 28); Add(museBody, accountDetail, 25, 4);
        accountAction.Click += (_, _) => Navigate(1); Add(museBody, accountAction, 34); muse.Controls.Add(museBody);
        var claude = Card(); var claudeBody = Stack();
        Add(claudeBody, UiTheme.Label("02   CLAUDE CODE", 8.5f, UiTheme.Accent, true), 25);
        Add(claudeBody, claudeState, 28); Add(claudeBody, claudeDetail, 25, 4);
        claudeAction.Click += (_, _) => OpenLink("https://code.claude.com/docs/en/setup");
        Add(claudeBody, claudeAction, 34); claude.Controls.Add(claudeBody);
        cards.Controls.Add(muse, 0, 0); cards.Controls.Add(claude, 2, 0); Add(page, cards, 136, 12);
        var work = Card(); var workBody = Stack();
        Add(workBody, UiTheme.Label("Không gian làm việc", 15, bold: true), 30);
        Add(workBody, UiTheme.Label("Chọn thư mục dự án bạn muốn làm việc cùng Claude.", 9, UiTheme.Muted), 25, 4);
        var project = Grid(2, 1); project.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100)); project.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 125));
        project.Controls.Add(Field(folder), 0, 0);
        var choose = Action("Chọn thư mục", width: 115); choose.Dock = DockStyle.Fill; choose.Margin = new Padding(10, 8, 0, 8);
        choose.Click += async (_, _) => await RunAction(() =>
        {
            using var dialog = new FolderBrowserDialog { Description = "Chọn thư mục dự án", UseDescriptionForTitle = true, InitialDirectory = folder.Text };
            if (dialog.ShowDialog(this) == DialogResult.OK) { folder.Text = dialog.SelectedPath; settings.ProjectFolder = folder.Text; store.Save(settings); }
            return Task.CompletedTask;
        });
        project.Controls.Add(choose, 1, 0); Add(workBody, project, 46, 4);
        var launchRow = Grid(2, 1); launchRow.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100)); launchRow.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 202));
        launchRow.Controls.Add(launchHint, 0, 0); launch.Dock = DockStyle.Fill; launch.Margin = new Padding(0, 7, 0, 7);
        launch.Click += async (_, _) => await RunAction(() => { settings.ProjectFolder = folder.Text; store.Save(settings); RuntimeFiles.LaunchClaude(settings, server.RuntimeDirectory); return Task.CompletedTask; });
        launchRow.Controls.Add(launch, 1, 0); Add(workBody, launchRow, 44); work.Controls.Add(workBody); Add(page, work, 170);
        return page;
    }

    private Control PreferencesPage()
    {
        var page = Stack(); var preferences = Card(); var body = Stack();
        Add(body, UiTheme.Label("Tùy chọn kết nối", 14, bold: true), 28);
        var labels = Grid(3, 1); EqualColumns(labels); labels.Controls.Add(UiTheme.Label("Cổng localhost", 9, UiTheme.Muted), 0, 0); labels.Controls.Add(UiTheme.Label("Context cho Claude Code", 9, UiTheme.Muted), 2, 0); Add(body, labels, 24);
        var inputs = Grid(3, 1); EqualColumns(inputs); inputs.Controls.Add(Field(port), 0, 0); inputs.Controls.Add(Field(context), 2, 0); Add(body, inputs, 46, 4);
        Add(body, UiTheme.Label("Context là budget của Claude Code; giới hạn thực tế tùy backend.", 8.5f, UiTheme.Muted), 25, 4);
        Add(body, autoStart, 28); preferences.Controls.Add(body); Add(page, preferences, 175, 8);
        var api = Card(); var apiBody = Stack();
        Add(apiBody, UiTheme.Label("Kết nối ứng dụng khác", 13, bold: true), 28); Add(apiBody, endpoint, 25, 4);
        var key = Grid(2, 1); key.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100)); key.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 125)); key.Controls.Add(Field(apiKey), 0, 0);
        var copy = Action("Sao chép key", width: 115); copy.Dock = DockStyle.Fill; copy.Margin = new Padding(10, 7, 0, 7);
        copy.Click += (_, _) => { try { Clipboard.SetText(settings.ApiKey); Notify("Đã sao chép API key."); } catch (ExternalException) { Notify("Clipboard đang bận. Thử lại sau.", true); } };
        key.Controls.Add(copy, 1, 0); Add(apiBody, key, 44); api.Controls.Add(apiBody); Add(page, api, 118, 8);
        var actions = Flow(); var save = Action("Lưu cài đặt", true, 145); save.Click += async (_, _) => await RunAction(SavePreferencesAsync);
        var diagnostic = Action("Xuất chẩn đoán", width: 155); diagnostic.Click += async (_, _) => await RunAction(() =>
        {
            using var dialog = new SaveFileDialog { Filter = "Text file (*.txt)|*.txt", FileName = "DinDesk-diagnostics.txt" };
            if (dialog.ShowDialog(this) == DialogResult.OK) { File.WriteAllText(dialog.FileName, server.Diagnostics()); Notify("Đã xuất chẩn đoán, cookie và key được che."); }
            return Task.CompletedTask;
        });
        var shortcut = Action("Tạo shortcut", width: 140); shortcut.Click += async (_, _) => await RunAction(() => { CreateShortcut(); Notify("Đã tạo shortcut Din Desk trên Desktop."); return Task.CompletedTask; });
        var agySetup = Action("Setup agy CLI", width: 145); agySetup.Click += async (_, _) => await RunAction(() =>
        {
            var result = AgyCliSetup.Install(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile));
            string backupNote = result.BackupPath != null ? " (đã backup bản cũ)" : "";
            Notify($"Đã setup agy CLI{backupNote}: {result.SettingsPath}");
            return Task.CompletedTask;
        });
        actions.Controls.AddRange(new Control[] { save, diagnostic, shortcut, agySetup }); Add(page, actions, 43);
        return page;
    }

    private Control HelpPage()
    {
        var page = Stack(); var guide = Card(); var body = Stack(); Add(body, UiTheme.Label("Lấy cookie Muse", 15, bold: true), 28);
        foreach (var text in new[] { "1. Đăng nhập tại muse.ai, nhấn F12 hoặc Ctrl + Shift + I.", "2. Mở tab Network, tải lại trang bằng Ctrl + R.", "3. Chọn một request tới muse.ai → Headers → Request Headers.", "4. Sao chép toàn bộ giá trị Cookie trên một dòng (có hatch_sess).", "5. Quay về Tài khoản, dán cookie rồi bấm Lưu & kết nối." }) Add(body, UiTheme.Label(text, 9.5f), 26);
        var back = Action("Tới tài khoản", true, 188); back.Click += (_, _) => Navigate(1); Add(body, back, 34); guide.Controls.Add(body); Add(page, guide, 208, 8);
        var gemini = Card(); var geminiBody=Stack(); Add(geminiBody,UiTheme.Label("Lấy cookie Gemini Web",13,bold:true),28);
        foreach(var text in new[]{"1. Đăng nhập gemini.google.com/app bằng tài khoản Google của bạn.","2. F12 → Network → tải lại trang → chọn request tới gemini.google.com.","3. Sao chép header Cookie có __Secure-1PSID và __Secure-1PSIDTS nếu có.","4. Tài khoản → Thêm → chọn Gemini Web → dán cookie → Lưu → Kết nối."}) Add(geminiBody,UiTheme.Label(text,9),25);
        gemini.Controls.Add(geminiBody);Add(page,gemini,144,8);
        var daily = Card(); var dailyBody = Stack(); Add(dailyBody, UiTheme.Label("Trong lúc sử dụng", 13, bold: true), 28);
        Add(dailyBody, UiTheme.Label("Giữ Din Desk chạy khi dùng Claude. Đóng cửa sổ sẽ thu app xuống khay hệ thống.\r\nĐể dừng hoàn toàn, chọn Thoát ứng dụng. Cookie hết hạn thì cập nhật ở Tài khoản.", 9, UiTheme.Muted), 62);
        daily.Controls.Add(dailyBody); Add(page, daily, 106);
        page.Dock=DockStyle.Top;page.Height=500;
        var scroll=new Panel{Dock=DockStyle.Fill,AutoScroll=true,AutoScrollMinSize=new Size(0,500)};
        scroll.Controls.Add(page);return scroll;
    }

    private void Navigate(int index)
    {
        tabs.SelectedIndex = index;
    }

    private static TextBox Input(string name, string accessibleName, string placeholder = "") => new() { Name = name, AccessibleName = accessibleName, PlaceholderText = placeholder,
        Dock = DockStyle.Fill, Margin = Padding.Empty };
    private static Panel Field(Control control)
    {
        var field = new SurfacePanel { Dock = DockStyle.Fill, Fill = UiTheme.Input, BackColor = UiTheme.Input, Padding = new Padding(0, 12, 0, 7) }; field.Controls.Add(control); return field;
    }
    private static DeskButton Action(string text, bool primary = false, int width = 150) => new() { Text = text, Name = text, AccessibleName = text, Primary = primary, Width = width };
    private static SurfacePanel Card() => new() { Dock = DockStyle.Fill };
    private static TableLayoutPanel Grid(int columns, int rows)
    {
        var grid = new DeskGrid { Dock = DockStyle.Fill, ColumnCount = columns, RowCount = rows, Margin = Padding.Empty, Padding = Padding.Empty, BackColor = Color.Transparent };
        if (columns == 1) grid.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        if (rows == 1) grid.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
        return grid;
    }
    private static TableLayoutPanel Stack()
    {
        // Keep a flexible final row so WinForms cannot stretch the final card/button.
        return Grid(1, 1);
    }
    private static FlowLayoutPanel Flow() => new() { Dock = DockStyle.Fill, WrapContents = false, Margin = Padding.Empty, Padding = Padding.Empty, BackColor = Color.Transparent };
    private static void EqualColumns(TableLayoutPanel grid) { grid.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 50)); grid.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 20)); grid.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 50)); }
    private static void Add(TableLayoutPanel page, Control control, int height, int gap = 0)
    {
        int row = page.RowCount - 1; page.RowCount++;
        page.RowStyles.Insert(row, new RowStyle(SizeType.Absolute, height + gap));
        int inset = control is Button ? Math.Max(0, (height - 30) / 2) : 0;
        control.Margin = new Padding(0, inset, 0, gap + inset); page.Controls.Add(control, 0, row);
        if (control is Button) control.Height = 30;
    }
    private static void CreateShortcut()
    {
        Type type = Type.GetTypeFromProgID("WScript.Shell") ?? throw new InvalidOperationException(); dynamic shell = Activator.CreateInstance(type)!; object? shortcut = null;
        try
        {
            shortcut = shell.CreateShortcut(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.DesktopDirectory), "Din Desk.lnk"));
            dynamic link = shortcut; link.TargetPath = Environment.ProcessPath!; link.WorkingDirectory = AppContext.BaseDirectory; link.Description = "Din Desk · Proxy AI cho Claude Code & agy CLI"; link.Save();
        }
        finally { if (shortcut != null) Marshal.FinalReleaseComObject(shortcut); Marshal.FinalReleaseComObject(shell); }
    }
}
