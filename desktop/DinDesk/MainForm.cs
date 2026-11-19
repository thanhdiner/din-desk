using System.Runtime.InteropServices;

namespace MuseDesk;

internal sealed partial class MainForm : Form
{
    private readonly SettingsStore store;
    private readonly ServerController server;
    private Settings settings;
    private readonly TextBox folder = Input("ProjectFolder", "Thư mục dự án", "Chọn thư mục bạn muốn làm việc");
    private readonly TextBox apiKey = Input("ProxyApiKey", "API key proxy");
    private readonly NumericUpDown port = new() { Minimum = 1024, Maximum = 65535, Value = 3001, Dock = DockStyle.Fill,
        Name = "ProxyPort", AccessibleName = "Cổng proxy" };
    private readonly ComboBox context = new() { DropDownStyle = ComboBoxStyle.DropDownList, Dock = DockStyle.Fill,
        Name = "ContextBudget", AccessibleName = "Ngân sách context" };
    private readonly CheckBox autoStart = new() { Text = "Tự kết nối khi mở Din Desk", AutoSize = true, Name = "AutoStart" };
    private readonly StatusBadge status = new() { Name = "ConnectionStatus", AccessibleName = "Trạng thái kết nối" };
    private readonly Label accountState = UiTheme.Label("Chưa kết nối", 13, bold: true);
    private readonly Label accountDetail = UiTheme.Label("Kết nối tài khoản để bắt đầu.", 9, UiTheme.Muted);
    private readonly Label claudeState = UiTheme.Label("Đang kiểm tra…", 13, bold: true);
    private readonly Label claudeDetail = UiTheme.Label("", 9, UiTheme.Muted);
    private readonly Label launchHint = UiTheme.Label("Kết nối Muse trước khi mở Claude Code.", 9, UiTheme.Muted);
    private readonly Label endpoint = UiTheme.Label("", 9, UiTheme.Muted);
    private readonly Label footerState = UiTheme.Label("Mọi thứ chạy trên máy của bạn.", 9, UiTheme.Muted);
    private readonly Label notice = UiTheme.Label("", 9);
    private readonly DeskButton connect = Action("Lưu & kết nối", primary: true, width: 172);
    private readonly DeskButton check = Action("Kiểm tra kết nối", width: 158);
    private readonly DeskButton stop = Action("Dừng kết nối", width: 140);
    private readonly DeskButton launch = Action("Mở Claude Code  →", primary: true, width: 202);
    private readonly DeskButton accountAction = Action("Kết nối tài khoản", width: 174);
    private readonly DeskButton claudeAction = Action("Cài Claude Code", width: 168);
    private readonly List<Control> pages = new();
    private readonly NotifyIcon tray;
    private readonly System.Windows.Forms.Timer refresh = new() { Interval = 1500 };
    private bool busy, verified, exiting, resourcesDisposed;
    private static readonly int[] ContextSizes = { 64000, 128000, 256000, 500000, 1000000 };

    internal MainForm(string? dataDirectory = null, AccountQuotaClient? quotaClient = null)
    {
        this.quotaClient = quotaClient ?? new AccountQuotaClient();
        store = new SettingsStore(dataDirectory ?? SettingsStore.DefaultDirectory);
        string? loadError = null;
        try { settings = store.Load(); }
        catch { settings = new Settings(); loadError = "Không đọc được cấu hình cũ. Hãy nhập lại cookie và lưu trên tài khoản Windows này."; }
        server = new ServerController(store.DirectoryPath);
        Text = "Din Desk · Bản xem trước"; Name = "DinDesk"; StartPosition = FormStartPosition.CenterScreen;
        AutoScaleMode = AutoScaleMode.Dpi; AutoScaleDimensions = new SizeF(96, 96);
        DoubleBuffered = true;
        ClientSize = new Size(720, 580); MinimumSize = new Size(700, 620);
        Font = SystemFonts.MessageBoxFont; BackColor = UiTheme.Background; ForeColor = UiTheme.Text; Icon = CreateIcon();
        apiKey.UseSystemPasswordChar = true;
        folder.ReadOnly = apiKey.ReadOnly = true;
        context.Items.AddRange(new object[] { "64k", "128k", "256k · mặc định", "500k · thử nghiệm", "1M · thử nghiệm" });
        folder.Text = settings.ProjectFolder; port.Value = settings.Port;
        int selected = Array.IndexOf(ContextSizes, settings.ContextTokens);
        context.SelectedIndex = selected >= 0 ? selected : 2;
        autoStart.Checked = settings.AutoStart; apiKey.Text = settings.ApiKey;
        BuildLayout();
        var menu = new ContextMenuStrip();
        menu.Items.Add("Mở Din Desk", null, (_, _) => RestoreWindow());
        menu.Items.Add("Dừng kết nối", null, (_, _) => { if (!busy) StopServer(); });
        menu.Items.Add("Thoát", null, (_, _) => ExitApp());
        tray = new NotifyIcon { Icon = Icon, Text = "Din Desk", Visible = true, ContextMenuStrip = menu };
        tray.DoubleClick += (_, _) => RestoreWindow();
        refresh.Tick += (_, _) => { if (!busy) UpdateStatus(); }; refresh.Start();
        Shown += async (_, _) =>
        {
            if (loadError != null) Notify(loadError, error: true);
            else if (settings.AutoStart && settings.Cookie.Length > 0) await RunAction(ConnectAsync);
        };
        FormClosing += (_, e) =>
        {
            if (busy && !exiting) { e.Cancel = true; Notify("Đang xử lý kết nối. Đợi thao tác hoàn tất rồi đóng ứng dụng."); return; }
            if (!exiting && server.Running)
            {
                e.Cancel = true; Hide();
                tray.ShowBalloonTip(3000, "Din Desk vẫn đang chạy", "Mở lại từ khay hệ thống. Chọn Thoát để dừng kết nối.", ToolTipIcon.Info);
            }
        };
        UpdateStatus();
    }

    private Settings ReadSettings()
    {
        var result=settings.Copy();result.Port=(int)port.Value;result.ContextTokens=ContextSizes[Math.Max(0,context.SelectedIndex)];
        result.ProjectFolder=folder.Text;result.AutoStart=autoStart.Checked;return result;
    }

    private async Task ConnectAsync()
    {
        var candidate = ReadSettings();
        string id=SelectedAccount?.Id??settings.ActiveAccountId??throw new ArgumentException("Thêm và chọn tài khoản trước khi kết nối.");
        candidate.SelectAccount(id);
        bool wasVerified=verified;
        try { await AccountSwitch.ApplyAsync(server,settings,candidate,store.Save); }
        catch {verified=wasVerified&&server.Running;throw;}
        settings=candidate;verified=true;RefreshAccounts(id);
        Navigate(0); Notify($"Đã kết nối {settings.ModelId}. Chọn thư mục dự án và mở Claude Code.");
    }

    private async Task SavePreferencesAsync()
    {
        var candidate = ReadSettings(); candidate.Cookie = settings.Cookie;
        SettingsStore.Validate(candidate, requireCookie: false);
        bool restart = server.Running && candidate.Port != settings.Port;
        if (restart) { server.Stop(); verified = false; }
        store.Save(candidate); settings = candidate;
        if (restart) { await server.StartAsync(settings); await server.CheckMuseAsync(); verified = true; }
        Notify(restart ? "Đã lưu và kết nối lại trên cổng mới." : "Đã lưu cài đặt. Context mới áp dụng cho cửa sổ Claude mở tiếp theo.");
    }

    private async Task RunAction(Func<Task> action)
    {
        if (busy) return;
        busy = true; notice.Text = ""; SetBusy(true);
        try { await action(); }
        catch (TaskCanceledException) { Notify("Kết nối quá lâu. Kiểm tra mạng rồi thử lại.", error: true); }
        catch (Exception error)
        {
            Notify(error is ArgumentException or InvalidOperationException ? error.Message : "Không hoàn tất được thao tác. Kiểm tra mạng, cookie hoặc quyền ghi thư mục rồi thử lại.", error: true);
        }
        finally { busy = false; SetBusy(false); UpdateStatus(); }
    }

    private void SetBusy(bool value)
    {
        connect.Text = value ? "Đang xử lý…" : "Chuyển tài khoản";
        foreach (Control control in new Control[] { connect, check, stop, launch, port, context, autoStart, accountAction, claudeAction }) control.Enabled = !value;
        foreach(var control in accountControls)control.Enabled=!value;
        if (value) { status.Text = "Đang kết nối"; status.Tone = UiTheme.Warning; status.Invalidate(); }
        UseWaitCursor = value;
    }

    private void UpdateStatus()
    {
        bool running = server.Running;
        if (!running) verified = false;
        bool installed = RuntimeFiles.FindClaude() != null;
        var current=settings.Accounts.FirstOrDefault(a=>a.Id==settings.ActiveAccountId);
        activeAccountLabel.Text="Tài khoản đang dùng: "+(current?.Name??"Chưa chọn")+(running?" (đang chạy)":" (đã dừng)");
        connect.Enabled=!busy && SelectedAccount!=null;
        if(!busy)connect.Text=settings.ActiveAccountId!=null && SelectedAccount?.Id==settings.ActiveAccountId?"Kết nối lại":"Chuyển tài khoản";
        string backend=current?.ProviderLabel??"AI";
        status.Text = verified ? $"Đã kết nối {backend}" : running ? $"Cần kiểm tra {backend}" : "Chưa kết nối";
        status.AccessibleName = "Trạng thái: " + status.Text;
        status.Tone = verified ? UiTheme.Success : running ? UiTheme.Warning : UiTheme.Muted; status.Invalidate();
        accountState.Text = verified ? $"Đã kết nối {backend}" : running ? "Chưa xác thực" : "Chưa kết nối";
        accountState.ForeColor = verified ? UiTheme.Success : UiTheme.Text;
        accountDetail.Text = current!=null?$"{current.Name} · {backend}":"Thêm tài khoản Muse hoặc Gemini để bắt đầu.";
        accountAction.Text = current!=null ? "Quản lý tài khoản" : "Kết nối tài khoản";
        claudeState.Text = installed ? "Đã cài Claude Code" : "Chưa cài Claude Code";
        claudeDetail.Text = installed ? "Đã tìm thấy trên máy của bạn." : "Cài một lần để bắt đầu làm việc.";
        claudeAction.Text = installed ? "Hướng dẫn Claude  ↗" : "Cài Claude Code  ↗";
        stop.Enabled = check.Enabled = !busy && running; stop.Visible = running;
        launch.Enabled = !busy && verified && installed && Directory.Exists(folder.Text);
        launchHint.Text = !verified ? "Kết nối tài khoản để mở Claude Code." : !installed ? "Cài Claude Code theo hướng dẫn ở trên." : !Directory.Exists(folder.Text) ? "Chọn một thư mục dự án còn tồn tại." : "Claude sẽ mở trong terminal tại thư mục đã chọn.";
        endpoint.Text = $"http://127.0.0.1:{settings.Port}";
        apiKey.Text=settings.ApiKey;
        footerState.Text = running ? $"Chạy trên máy · cổng {settings.Port}   /   Giữ Din Desk mở trong lúc dùng Claude." : "Chạy trên máy của bạn · Cấu hình được mã hóa bằng Windows DPAPI.";
        if (tray != null) tray.Text = "Din Desk · " + (verified ? "Đã kết nối" : running ? "Đang chạy" : "Đã dừng");
    }

    private void StopServer() { server.Stop(); verified = false; Notify("Đã dừng kết nối. Bạn có thể kết nối lại khi cần."); UpdateStatus(); }
    private void Notify(string text, bool error = false) { notice.Text = text; notice.ForeColor = error ? UiTheme.Error : UiTheme.Success; }
    private void RestoreWindow() { Show(); WindowState = FormWindowState.Normal; Activate(); }
    private void ExitApp()
    {
        if (busy) { Notify("Đợi thao tác kết nối hoàn tất rồi thoát."); return; }
        if (server.Running && MessageBox.Show(this, "Thoát sẽ dừng kết nối của các cửa sổ Claude đang mở. Tiếp tục?", "Thoát Din Desk", MessageBoxButtons.YesNo, MessageBoxIcon.Question) != DialogResult.Yes) return;
        exiting = true; Close();
    }
    private void OpenLink(string url) { try { RuntimeFiles.OpenLink(url); } catch { Notify("Không mở được trình duyệt. Truy cập: " + url, error: true); } }
    private static Icon CreateIcon()
    {
        using var stream = typeof(MainForm).Assembly.GetManifestResourceStream("MuseDesk.icon.ico");
        return stream == null ? (Icon)SystemIcons.Application.Clone() : new Icon(stream);
    }
    protected override void Dispose(bool disposing)
    {
        if (disposing && !resourcesDisposed) { resourcesDisposed = true; quotaLifetime.Cancel(); quotaLifetime.Dispose(); refresh.Dispose(); tray?.Dispose(); server.Dispose(); Icon?.Dispose(); }
        base.Dispose(disposing);
    }
}
