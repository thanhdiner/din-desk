using System.Diagnostics;
using System.Reflection;
using System.Security.Cryptography;

namespace MuseDesk;

internal static class RuntimeFiles
{
    public static string Extract(string dataDirectory)
    {
        string directory = Path.Combine(dataDirectory, "runtime", Program.Version);
        Directory.CreateDirectory(directory);
        foreach (var (resource, name) in new[] { ("MuseDesk.server.exe", "muse-api.exe"), ("MuseDesk.claude-muse.ps1", "claude-muse.ps1") })
        {
            using Stream stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(resource)
                ?? throw new InvalidOperationException("Bản phát hành thiếu server. Hãy tải lại gói Din Desk.");
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
        return directory;
    }

    public static string? FindClaude()
    {
        string profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var roots = (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator).ToList();
        roots.AddRange(new[] { Path.Combine(profile, ".local", "bin"), Path.Combine(profile, ".claude", "local"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "npm"),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "nodejs") });
        foreach (string directory in roots.Where(p => !string.IsNullOrWhiteSpace(p)).Distinct(StringComparer.OrdinalIgnoreCase))
        {
            foreach (string name in new[] { "claude.exe", "claude.cmd", "claude.ps1" })
            {
                string file = Path.Combine(directory.Trim('"'), name);
                if (File.Exists(file)) return file;
            }
        }
        return null;
    }

    public static void LaunchClaude(Settings settings, string runtimeDirectory)
    {
        Process.Start(ClaudeStartInfo(settings, runtimeDirectory))?.Dispose();
    }

    internal static ProcessStartInfo ClaudeStartInfo(Settings settings, string runtimeDirectory)
    {
        if (!Directory.Exists(settings.ProjectFolder)) throw new ArgumentException("Hãy chọn một thư mục dự án còn tồn tại.");
        string claude = FindClaude() ?? throw new InvalidOperationException("Chưa tìm thấy Claude Code. Bấm “Cài Claude Code” rồi mở lại Din Desk.");
        var info = new ProcessStartInfo("powershell.exe") { UseShellExecute = false, WorkingDirectory = settings.ProjectFolder };
        info.ArgumentList.Add("-NoProfile");
        info.ArgumentList.Add("-NoExit");
        info.ArgumentList.Add("-ExecutionPolicy");
        info.ArgumentList.Add("Bypass");
        info.ArgumentList.Add("-File");
        info.ArgumentList.Add(Path.Combine(runtimeDirectory, "claude-muse.ps1"));
        info.Environment["MUSE_PROXY_URL"] = $"http://127.0.0.1:{settings.Port}";
        info.Environment["MUSE_PROXY_API_KEY"] = settings.ApiKey;
        info.Environment["MUSE_CONTEXT_TOKENS"] = settings.ContextTokens.ToString();
        info.Environment["MUSE_MODEL"] = settings.ModelId;
        info.Environment["PATH"] = Path.GetDirectoryName(claude) + Path.PathSeparator + info.Environment["PATH"];
        // This console is interactive and explicitly opened by the user.
        return info;
    }

    public static void OpenLink(string url) => Process.Start(new ProcessStartInfo(url) { UseShellExecute = true })?.Dispose();
}
