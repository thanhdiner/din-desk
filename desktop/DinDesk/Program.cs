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
        using var singleInstance = new Mutex(true, "Local\\DinDesk-" + Environment.UserName + "-" + identity[..16], out bool first);
        if (!first)
        {
            MessageBox.Show("Din Desk đang mở. Hãy tìm biểu tượng Din Desk trên thanh tác vụ.", "Din Desk");
            return 0;
        }
        try
        {
            using var window = new MainForm(dataDirectory);
            Application.Run(window);
            return 0;
        }
        catch (Exception)
        {
            MessageBox.Show("Không thể mở Din Desk. Hãy giải nén lại bản phát hành vào một thư mục mới.", "Din Desk", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 1;
        }
    }
}
