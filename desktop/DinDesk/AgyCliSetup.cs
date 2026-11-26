using System.Reflection;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal static class AgyCliSetup
{
    internal const string SettingsFile = "agy-settings";
    internal const string StatuslineFile = "agy-statusline";
    private const string SettingsResourceName = "MuseDesk.agy-settings.template.json";
    private const string StatuslineResourceName = "MuseDesk.agy-statusline.ps1";
    private const string BaselineKind = "agy-before-setup";

    internal sealed record InstallResult(string SettingsPath, string StatuslinePath, string? BackupPath);

    internal static Dictionary<string, string> Paths(string userProfileDir)
    {
        if (string.IsNullOrWhiteSpace(userProfileDir))
            throw new ArgumentException("Không xác định được thư mục người dùng.");
        string fullProfile = Path.GetFullPath(userProfileDir);
        return new()
        {
            [SettingsFile] = Path.Combine(fullProfile, ".gemini", "antigravity-cli", "settings.json"),
            [StatuslineFile] = Path.Combine(fullProfile, ".antigravity", "agy-statusline.ps1")
        };
    }

    public static InstallResult Install(string userProfileDir, string? dataDirectory = null)
    {
        var paths = Paths(userProfileDir);
        string settingsPath = paths[SettingsFile];
        var original = ConfigBackup.Capture(paths);
        var installed = Render(paths);
        bool alreadyInstalled = IsInstalled(original[SettingsFile], installed[SettingsFile]);
        if (dataDirectory != null && !alreadyInstalled && !File.Exists(BaselinePath(dataDirectory)))
            ConfigBackup.Export(BaselinePath(dataDirectory), BaselineKind, original, overwrite: false);
        string? backupPath = null;
        if (original[SettingsFile] is { } previous && (dataDirectory == null || !alreadyInstalled))
        {
            backupPath = settingsPath + ".bak";
            ConfigBackup.WriteAtomic(backupPath, previous, overwrite: true);
        }
        ConfigBackup.Apply(paths, original, installed);
        return new InstallResult(settingsPath, paths[StatuslineFile], backupPath);
    }

    internal static string BaselinePath(string dataDirectory) => Path.Combine(dataDirectory, "agy-before-setup.protected");

    internal static Dictionary<string, byte[]?>? BeforeSetup(string dataDirectory, IReadOnlyDictionary<string, string> paths,
        Dictionary<string, byte[]?> current, out bool incompleteLegacy)
    {
        incompleteLegacy = false;
        string baselinePath = BaselinePath(dataDirectory);
        if (File.Exists(baselinePath)) return ConfigBackup.Import(baselinePath, BaselineKind, paths.Keys);

        var installed = Render(paths);
        if (!IsInstalled(current[SettingsFile], installed[SettingsFile])) return null;
        incompleteLegacy = true;
        byte[]? previous = ConfigBackup.ReadIfPresent(paths[SettingsFile] + ".bak");
        if (previous == null || IsInstalled(previous, installed[SettingsFile])) return null;
        return new() { [SettingsFile] = previous };
    }

    private static Dictionary<string, byte[]?> Render(IReadOnlyDictionary<string, string> paths)
    {
        string template = ReadEmbeddedText(SettingsResourceName);
        string portableStatuslinePath = paths[StatuslineFile].Replace('\\', '/');
        string renderedSettings = template.Replace("{{STATUSLINE_PATH}}", portableStatuslinePath, StringComparison.Ordinal);
        using (JsonDocument.Parse(renderedSettings)) { }
        return new()
        {
            [SettingsFile] = Encoding.UTF8.GetBytes(renderedSettings),
            [StatuslineFile] = Encoding.UTF8.GetBytes(ReadEmbeddedText(StatuslineResourceName))
        };
    }

    private static bool IsInstalled(byte[]? current, byte[]? template) => current != null && template != null && current.AsSpan().SequenceEqual(template);

    private static string ReadEmbeddedText(string resourceName)
    {
        using Stream stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(resourceName)
            ?? throw new InvalidOperationException($"Thiếu tài nguyên nhúng: {resourceName}");
        using var reader = new StreamReader(stream, Encoding.UTF8, detectEncodingFromByteOrderMarks: true);
        return reader.ReadToEnd();
    }
}
