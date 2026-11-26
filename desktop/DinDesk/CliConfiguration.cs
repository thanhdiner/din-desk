using System.Security.Cryptography;

namespace MuseDesk;

internal sealed class CliConfiguration(string dataDirectory, string userDirectory, string documentsDirectory,
    string? claudeDirectory = null, string? codexDirectory = null)
{
    private const string ArchiveKind = "cli-settings";
    private const string PowerShell5 = "powershell-5-profile";
    private const string PowerShell7 = "powershell-7-profile";
    private const string AgyResetState = "agy-reset-state";
    internal const string Extension = "dindesk-backup";

    internal static CliConfiguration ForCurrentUser(string dataDirectory) => new(dataDirectory,
        Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
        Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments),
        Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR"), Environment.GetEnvironmentVariable("CODEX_HOME"));

    internal Dictionary<string, string> Paths()
    {
        var paths = AgyCliSetup.Paths(userDirectory);
        AddProfiles(paths);
        paths["claude-settings"] = Path.Combine(ResolveHome(claudeDirectory, ".claude"), "settings.json");
        paths["codex-settings"] = Path.Combine(ResolveHome(codexDirectory, ".codex"), "config.toml");
        paths[AgyResetState] = Path.GetFullPath(AgyCliSetup.BaselinePath(dataDirectory));
        if (paths.Values.Distinct(StringComparer.OrdinalIgnoreCase).Count() != paths.Count)
            throw new InvalidOperationException("Hai CLI đang dùng chung đường dẫn cấu hình.");
        return paths;
    }

    internal void Export(string file)
    {
        var paths = Paths();
        file = Path.GetFullPath(file);
        if (!Path.GetExtension(file).Equals("." + Extension, StringComparison.OrdinalIgnoreCase)
            || paths.Values.Contains(file, StringComparer.OrdinalIgnoreCase))
            throw new ArgumentException("Chọn vị trí riêng để lưu bản sao cấu hình.");
        ConfigBackup.Export(file, ArchiveKind, ConfigBackup.Capture(paths), overwrite: true);
    }

    internal int Restore(string file)
    {
        var paths = Paths();
        Dictionary<string, byte[]?> snapshot;
        try { snapshot = ConfigBackup.Import(file, ArchiveKind, paths.Keys); }
        catch (CryptographicException)
        {
            throw new InvalidOperationException("Không mở được bản sao. Hãy dùng file đã sao lưu trên tài khoản Windows này.");
        }
        return ConfigBackup.Apply(paths, ConfigBackup.Capture(paths), snapshot);
    }

    internal (int Changes, bool IncompleteAgy) Reset()
    {
        var agyPaths = AgyCliSetup.Paths(userDirectory);
        var paths = new Dictionary<string, string>(agyPaths);
        AddProfiles(paths);
        var current = ConfigBackup.Capture(paths);
        var desired = AgyCliSetup.BeforeSetup(dataDirectory, agyPaths, current, out bool incompleteAgy) ?? new();
        desired[PowerShell5] = CliSetup.WithoutManagedAliases(current[PowerShell5]);
        desired[PowerShell7] = CliSetup.WithoutManagedAliases(current[PowerShell7]);
        string baseline = AgyCliSetup.BaselinePath(dataDirectory);
        byte[]? originalBaseline = ConfigBackup.ReadIfPresent(baseline);
        if (originalBaseline != null)
        {
            paths[AgyResetState] = baseline;
            current[AgyResetState] = originalBaseline;
            desired[AgyResetState] = null;
        }
        int changes = ConfigBackup.Apply(paths, current, desired);
        return (changes - (originalBaseline != null ? 1 : 0), incompleteAgy);
    }

    private void AddProfiles(Dictionary<string, string> paths)
    {
        if (string.IsNullOrWhiteSpace(documentsDirectory)) throw new InvalidOperationException("Không tìm thấy thư mục Documents.");
        paths[PowerShell5] = Path.GetFullPath(CliSetup.ProfilePath(documentsDirectory, "WindowsPowerShell"));
        paths[PowerShell7] = Path.GetFullPath(CliSetup.ProfilePath(documentsDirectory, "PowerShell"));
    }

    private string ResolveHome(string? overrideDirectory, string directory) => Path.GetFullPath(
        string.IsNullOrWhiteSpace(overrideDirectory) ? Path.Combine(userDirectory, directory) : overrideDirectory);
}
