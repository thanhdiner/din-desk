namespace MuseDesk;

internal sealed partial class MainForm
{
    private readonly Label envKeyTarget = UiTheme.Label("Chưa chọn file .env.");
    private readonly DeskButton syncEnvKey = Action("Cập nhật key vào .env", width: 186);

    private Control EnvKeyCard()
    {
        var card = Card();
        var content = Stack();
        envKeyTarget.AutoEllipsis = true;
        Add(content, envKeyTarget, 25);
        var buttons = Flow();
        var configure = Action("Chọn file .env", width: 144);
        configure.Click += async (_, _) => await RunAction(ConfigureEnvKeyAsync);
        syncEnvKey.Click += async (_, _) => await RunAction(() =>
        {
            EnvKeyFile.Update(settings);
            Notify($"Đã cập nhật {settings.EnvKeyVariable} trong file .env.");
            return Task.CompletedTask;
        });
        buttons.Controls.AddRange([configure, syncEnvKey]);
        Add(content, buttons, 38);
        card.Controls.Add(content);
        preferenceActions.AddRange([configure, syncEnvKey]);
        return card;
    }

    private void UpdateEnvKeyControls()
    {
        envKeyTarget.Text = settings.EnvKeyFilePath is { } path
            ? $"{settings.EnvKeyVariable} · {path}"
            : "Chưa chọn file .env.";
        syncEnvKey.Enabled = !busy && settings.EnvKeyFilePath != null;
    }

    private Task ConfigureEnvKeyAsync()
    {
        using var dialog = new Form
        {
            Text = "Cấu hình file .env", StartPosition = FormStartPosition.CenterParent,
            FormBorderStyle = FormBorderStyle.FixedDialog, ClientSize = new Size(550, 194),
            MaximizeBox = false, MinimizeBox = false, ShowInTaskbar = false, Font = Font,
            AutoScaleMode = AutoScaleMode.Dpi, AutoScaleDimensions = new SizeF(96, 96)
        };
        var layout = Stack();
        layout.Padding = new Padding(12);
        var path = Input("EnvKeyFile", "Đường dẫn file .env");
        path.ReadOnly = true;
        path.Text = settings.EnvKeyFilePath ?? "";
        var variable = Input("EnvKeyVariable", "Tên biến .env");
        variable.Text = settings.EnvKeyVariable;
        var browse = Action("Duyệt…", width: 100);
        browse.Dock = DockStyle.Fill;
        browse.Margin = new Padding(8, 0, 0, 4);
        browse.Click += (_, _) =>
        {
            using var picker = new OpenFileDialog
            {
                Title = "Chọn file .env", Filter = "File .env|*.env;*.env.*|Tất cả file|*.*",
                CheckFileExists = true, FileName = path.Text
            };
            if (picker.ShowDialog(dialog) == DialogResult.OK) path.Text = picker.FileName;
        };
        var fileRow = Grid(2, 1);
        fileRow.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        fileRow.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 108));
        fileRow.Controls.Add(path, 0, 0);
        fileRow.Controls.Add(browse, 1, 0);
        Add(layout, UiTheme.Label("File .env"), 22);
        Add(layout, fileRow, 31);
        Add(layout, UiTheme.Label("Tên biến cần cập nhật"), 22);
        Add(layout, variable, 27);
        var error = UiTheme.Label("", color: UiTheme.Error);
        Add(layout, error, 24);
        var buttons = Flow();
        buttons.FlowDirection = FlowDirection.RightToLeft;
        var cancel = Action("Hủy", width: 84);
        cancel.DialogResult = DialogResult.Cancel;
        var save = Action("Lưu", primary: true, width: 84);
        save.Click += (_, _) =>
        {
            if (!File.Exists(path.Text)) { error.Text = "Chọn file .env hiện có."; return; }
            if (!EnvKeyFile.IsVariableName(variable.Text.Trim())) { error.Text = "Tên biến .env không hợp lệ."; return; }
            dialog.DialogResult = DialogResult.OK;
        };
        buttons.Controls.AddRange([save, cancel]);
        Add(layout, buttons, 39);
        dialog.AcceptButton = save;
        dialog.CancelButton = cancel;
        dialog.Controls.Add(layout);

        if (dialog.ShowDialog(this) != DialogResult.OK) return Task.CompletedTask;
        var candidate = settings.Copy();
        candidate.EnvKeyFilePath = Path.GetFullPath(path.Text);
        candidate.EnvKeyVariable = variable.Text.Trim();
        store.Save(candidate);
        settings = candidate;
        Notify("Đã lưu file .env và tên biến.");
        return Task.CompletedTask;
    }
}
