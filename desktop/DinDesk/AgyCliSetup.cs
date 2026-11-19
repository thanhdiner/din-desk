using System.Reflection;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal static class AgyCliSetup
{
    private const string SettingsResourceName = "MuseDesk.agy-settings.template.json";
    private const string StatuslineResourceName = "MuseDesk.agy-statusline.ps1";

    internal sealed record InstallResult(string SettingsPath, string StatuslinePath, string? BackupPath);

    /// <summary>
    /// Installs the bundled Antigravity CLI (agy) settings and custom statusline script
    /// into the target Windows user profile directory.
    /// </summary>
    public static InstallResult Install(string userProfileDir)
    {
        if (string.IsNullOrWhiteSpace(userProfileDir))
            throw new ArgumentException("Không xác định được thư mục người dùng.");

        string fullProfile = Path.GetFullPath(userProfileDir);
        string antigravityDir = Path.Combine(fullProfile, ".antigravity");
        string cliConfigDir = Path.Combine(fullProfile, ".gemini", "antigravity-cli");
        Directory.CreateDirectory(antigravityDir);
        Directory.CreateDirectory(cliConfigDir);

        string statuslinePath = Path.Combine(antigravityDir, "agy-statusline.ps1");
        string settingsPath = Path.Combine(cliConfigDir, "settings.json");

        WriteEmbeddedText(StatuslineResourceName, statuslinePath, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));

        string? backupPath = null;
        if (File.Exists(settingsPath))
        {
            backupPath = settingsPath + ".bak";
            File.Copy(settingsPath, backupPath, overwrite: true);
        }

        string template = ReadEmbeddedText(SettingsResourceName);
        string portableStatuslinePath = statuslinePath.Replace('\\', '/');
        string renderedSettings = template.Replace("{{STATUSLINE_PATH}}", portableStatuslinePath, StringComparison.Ordinal);

        using (JsonDocument.Parse(renderedSettings)) { }
        File.WriteAllText(settingsPath, renderedSettings, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));

        return new InstallResult(settingsPath, statuslinePath, backupPath);
    }

    private static string ReadEmbeddedText(string resourceName)
    {
        using Stream stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(resourceName)
            ?? throw new InvalidOperationException($"Thiếu tài nguyên nhúng: {resourceName}");
        using var reader = new StreamReader(stream, Encoding.UTF8, detectEncodingFromByteOrderMarks: true);
        return reader.ReadToEnd();
    }

    private static void WriteEmbeddedText(string resourceName, string destinationPath, Encoding encoding)
    {
        string content = ReadEmbeddedText(resourceName);
        File.WriteAllText(destinationPath, content, encoding);
    }
}
