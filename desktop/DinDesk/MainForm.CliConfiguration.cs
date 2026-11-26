namespace MuseDesk;

internal sealed partial class MainForm
{
    private const string CliBackupFilter = "Bản sao Din Desk (*.dindesk-backup)|*.dindesk-backup";

    private Control CliConfigurationActions()
    {
        var row = Flow();
        var export = Action("Sao lưu cấu hình CLI", width: 165);
        var restore = Action("Khôi phục từ file", width: 148);
        var reset = Action("Reset cấu hình CLI", width: 154);
        export.Click += async (_, _) => await RunAction(ExportCliConfigurationAsync);
        restore.Click += async (_, _) => await RunAction(RestoreCliConfigurationAsync);
        reset.Click += async (_, _) => await RunAction(ResetCliConfigurationAsync);
        row.Controls.AddRange([export, restore, reset]);
        preferenceActions.AddRange([export, restore, reset]);
        return row;
    }

    private Task ExportCliConfigurationAsync()
    {
        using var picker = new SaveFileDialog
        {
            Title = "Sao lưu cấu hình CLI", Filter = CliBackupFilter, DefaultExt = CliConfiguration.Extension,
            AddExtension = true, FileName = $"DinDesk-CLI-{DateTime.Now:yyyy-MM-dd}.{CliConfiguration.Extension}"
        };
        if (picker.ShowDialog(this) == DialogResult.OK)
        {
            CliConfiguration.ForCurrentUser(store.DirectoryPath).Export(picker.FileName);
            Notify("Đã lưu bản sao cấu hình CLI.");
        }
        return Task.CompletedTask;
    }

    private Task RestoreCliConfigurationAsync()
    {
        using var picker = new OpenFileDialog { Title = "Chọn bản sao cấu hình CLI", Filter = CliBackupFilter, CheckFileExists = true };
        if (picker.ShowDialog(this) != DialogResult.OK) return Task.CompletedTask;
        var answer = MessageBox.Show(this, "Thay các file cấu hình CLI hiện tại bằng trạng thái trong bản sao đã chọn?",
            "Khôi phục cấu hình CLI", MessageBoxButtons.YesNo, MessageBoxIcon.Question, MessageBoxDefaultButton.Button2);
        if (answer == DialogResult.Yes)
        {
            int changed = CliConfiguration.ForCurrentUser(store.DirectoryPath).Restore(picker.FileName);
            Notify(changed == 0 ? "Cấu hình đã khớp với bản sao." : "Đã khôi phục cấu hình CLI. Mở PowerShell mới để áp dụng.");
        }
        return Task.CompletedTask;
    }

    private Task ResetCliConfigurationAsync()
    {
        var answer = MessageBox.Show(this, "Gỡ lệnh cy/cx do Din Desk thêm và đưa agy về cấu hình trước Setup nếu có bản gốc?",
            "Reset cấu hình CLI", MessageBoxButtons.YesNo, MessageBoxIcon.Question, MessageBoxDefaultButton.Button2);
        if (answer == DialogResult.Yes)
        {
            var (changes, incompleteAgy) = CliConfiguration.ForCurrentUser(store.DirectoryPath).Reset();
            if (incompleteAgy) Notify(changes == 0 ? "Thiếu bản gốc agy từ lần Setup cũ." : "Đã reset phần có bản gốc. Thiếu một phần cấu hình agy cũ.", true);
            else Notify(changes == 0 ? "Cấu hình CLI đã ở trạng thái trước Setup." : "Đã reset cấu hình CLI. Mở PowerShell mới để áp dụng.");
        }
        return Task.CompletedTask;
    }
}
