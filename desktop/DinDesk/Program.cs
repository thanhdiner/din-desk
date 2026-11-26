namespace MuseDesk;

internal static class Program
{
    internal static string Version => typeof(Program).Assembly
        .GetCustomAttributes(typeof(System.Reflection.AssemblyInformationalVersionAttribute), false)
        .OfType<System.Reflection.AssemblyInformationalVersionAttribute>()
        .Single().InformationalVersion.Split('+')[0];

    [STAThread]
    private static int Main(string[] args)
    {
        ApplicationConfiguration.Initialize();
        if (args.Contains("--self-test")) return SmokeTests.Run(args);
        string? dataDirectory = args.SkipWhile(a => a != "--data-dir").Skip(1).FirstOrDefault();
        string identity = Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(Path.GetFullPath(dataDirectory ?? SettingsStore.DefaultDirectory))));
        string instanceName = "Local\\DinDesk-" + Environment.UserName + "-" + identity[..16];
        using var showWindow = new EventWaitHandle(false, EventResetMode.AutoReset, instanceName + "-show");
        using var singleInstance = new Mutex(true, instanceName, out bool first);
        if (!first)
        {
            showWindow.Set();
            return 0;
        }
        try
        {
            using var window = new MainForm(dataDirectory);
            _ = window.Handle;
            var showRequest = ThreadPool.RegisterWaitForSingleObject(showWindow, (_, _) =>
            {
                try { window.BeginInvoke((Action)window.RestoreWindow); }
                catch (InvalidOperationException) { /* The form can close while an activation is queued. */ }
            }, null, Timeout.Infinite, executeOnlyOnce: false);
            try { Application.Run(window); }
            finally { showRequest.Unregister(null); }
            return 0;
        }
        catch (Exception)
        {
            MessageBox.Show("Không thể mở Din Desk. Hãy giải nén lại bản phát hành vào một thư mục mới.", "Din Desk", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 1;
        }
    }
}
