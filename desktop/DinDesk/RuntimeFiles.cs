using System.Diagnostics;
using System.Reflection;
using System.Security.Cryptography;

namespace MuseDesk;

internal static class RuntimeFiles
{
    internal static readonly string[] CliFiles = ["claude-muse.ps1", "codex-muse.ps1", "codex-model-catalog.json", "dindesk-connection.ps1"];

    public static string Extract(string dataDirectory)
    {
        string directory = Path.Combine(dataDirectory, "runtime", Program.Version);
        ExtractFiles(directory, ["muse-api.exe", ..CliFiles]);
        return directory;
    }

    internal static void UpdateCli(string dataDirectory) => ExtractFiles(Path.Combine(dataDirectory, "cli"), CliFiles);

    private static void ExtractFiles(string directory, IEnumerable<string> names)
    {
        Directory.CreateDirectory(directory);
        foreach (string name in names)
        {
            string resource = name == "muse-api.exe" ? "MuseDesk.server.exe" : "MuseDesk." + name;
            using Stream stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(resource)
                ?? throw new InvalidOperationException("Bản phát hành thiếu tệp chạy. Hãy tải lại gói Din Desk.");
            string path = Path.Combine(directory, name);
            // Verify embedded bytes on every launch; do not trust stale extraction.
            byte[] expected = SHA256.HashData(stream);
            stream.Position = 0;
            if (File.Exists(path))
            {
                using var existing = File.OpenRead(path);
                if (CryptographicOperations.FixedTimeEquals(expected, SHA256.HashData(existing))) continue;
            }
            string temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try
            {
                using (var file = File.Create(temp)) stream.CopyTo(file);
                File.Move(temp, path, overwrite: true);
            }
            finally { if (File.Exists(temp)) File.Delete(temp); }
        }
    }

    public static string? FindClaude() => FindCli("claude");

    public static string? FindCli(string command)
    {
        string profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var roots = (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator).ToList();
        roots.AddRange(new[] { Path.Combine(profile, ".local", "bin"), Path.Combine(profile, ".claude", "local"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "npm"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "nodejs") });
        foreach (string directory in roots.Where(p => !string.IsNullOrWhiteSpace(p)).Distinct(StringComparer.OrdinalIgnoreCase))
        {
            foreach (string extension in new[] { ".exe", ".cmd", ".ps1" })
            {
                string file = Path.Combine(directory.Trim('"'), command + extension);
                if (File.Exists(file)) return file;
            }
        }
        return null;
    }

    public static void LaunchCli(Settings settings, string runtimeDirectory, string dataDirectory, string command)
    {
        Process.Start(CliStartInfo(settings, runtimeDirectory, command, dataDirectory))?.Dispose();
    }

    internal static ProcessStartInfo ClaudeStartInfo(Settings settings, string runtimeDirectory) => CliStartInfo(settings, runtimeDirectory, "claude", null);

    internal static ProcessStartInfo CliStartInfo(Settings settings, string runtimeDirectory, string command, string? dataDirectory)
    {
        if (!Directory.Exists(settings.ProjectFolder)) throw new ArgumentException("Hãy chọn một thư mục dự án còn tồn tại.");
        string executable = FindCli(command) ?? throw new InvalidOperationException($"Chưa tìm thấy {command}. Cài CLI rồi mở lại Din Desk.");
        var info = new ProcessStartInfo("powershell.exe") { UseShellExecute = false, WorkingDirectory = settings.ProjectFolder };
        info.ArgumentList.Add("-NoProfile");
        info.ArgumentList.Add("-NoExit");
        info.ArgumentList.Add("-ExecutionPolicy");
        info.ArgumentList.Add("Bypass");
        info.ArgumentList.Add("-File");
        info.ArgumentList.Add(Path.Combine(runtimeDirectory, command + "-muse.ps1"));
        if (dataDirectory != null)
        {
            if (command == "claude") info.ArgumentList.Add("-MuseDesk");
            info.ArgumentList.Add("-DeskDirectory");
            info.ArgumentList.Add(dataDirectory);
            info.ArgumentList.Add("-FullAccess");
        }
        else
        {
            info.Environment["MUSE_PROXY_URL"] = $"http://127.0.0.1:{settings.Port}";
            info.Environment["MUSE_PROXY_API_KEY"] = settings.ApiKey;
            info.Environment["MUSE_CONTEXT_TOKENS"] = settings.ContextTokens.ToString();
            info.Environment["MUSE_MODEL"] = settings.ModelId;
        }
        info.Environment["PATH"] = Path.GetDirectoryName(executable) + Path.PathSeparator + info.Environment["PATH"];
        // This console is interactive and explicitly opened by the user.
        return info;
    }

    public static void OpenLink(string url) => Process.Start(new ProcessStartInfo(url) { UseShellExecute = true })?.Dispose();
}
