using System.Text;

namespace MuseDesk;

internal static class CliConfigurationTests
{
    internal static void Run(string root, List<string> checks)
    {
        string fixture = Path.Combine(root, "cli-config-fixture");
        string user = Path.Combine(fixture, "user");
        string documents = Path.Combine(fixture, "documents");
        string data = Path.Combine(fixture, "data");
        string backup = Path.Combine(fixture, "manual.dindesk-backup");
        string invalid = Path.Combine(fixture, "invalid.dindesk-backup");
        var config = new CliConfiguration(data, user, documents);
        var paths = config.Paths();
        string profile5 = paths["powershell-5-profile"];
        string profile7 = paths["powershell-7-profile"];
        string claude = paths["claude-settings"];
        string codex = paths["codex-settings"];
        string agy = paths[AgyCliSetup.SettingsFile];
        string statusline = paths[AgyCliSetup.StatuslineFile];
        const string originalProfile = "# profile hiện tại\r\nfunction cy {}\r\n";
        const string originalAgy = "{\"fixture\":\"original-agy\"}";
        const string originalStatusline = "# original statusline\r\n";
        const string claudeConfig = "{\"env\":{\"FIXTURE_KEY\":\"private-fixture-key\"}}";

        try
        {
            Seed(profile5, originalProfile, Encoding.Unicode);
            Seed(profile7, originalProfile);
            Seed(claude, claudeConfig);
            Seed(agy, originalAgy);
            Seed(statusline, originalStatusline);
            CliSetup.Install(data, documents, "claude");
            CliSetup.Install(data, documents, "codex");
            AgyCliSetup.Install(user, data);
            AgyCliSetup.Install(user, data);
            var snapshot = ConfigBackup.Capture(paths);
            config.Export(backup);
            Require(!Encoding.UTF8.GetString(File.ReadAllBytes(backup)).Contains("private-fixture-key"), "Export leaked CLI configuration contents");

            Seed(claude, "{}");
            Seed(codex, "model = 'extra after backup'");
            File.Delete(agy);
            File.WriteAllText(invalid, "invalid fixture");
            ExpectFailure(() => config.Restore(invalid));
            Require(File.ReadAllText(claude) == "{}", "Invalid CLI backup changed the current config");
            var reopened = new CliConfiguration(data, user, documents);
            Require(reopened.Restore(backup) == 3, "Restoring CLI configuration changed an unexpected number of files");
            foreach (var (name, content) in ConfigBackup.Capture(paths))
                Require(content == null ? snapshot[name] == null : snapshot[name] != null && content.AsSpan().SequenceEqual(snapshot[name]), "CLI restore did not reproduce the exported file state");

            File.AppendAllText(profile5, "$env:USER_EDIT = 'keep'\r\n", Encoding.Unicode);
            Seed(codex, "model = 'user-config'");
            var reset = reopened.Reset();
            Require(reset is (4, false), "CLI reset failed to recover PowerShell and agy configuration");
            Require(File.ReadAllText(profile5) == originalProfile + "$env:USER_EDIT = 'keep'\r\n" && File.ReadAllText(profile7) == originalProfile,
                "CLI reset altered unrelated profile content");
            Require(File.ReadAllText(agy) == originalAgy && File.ReadAllText(statusline) == originalStatusline,
                "Repeated Setup overwrote the original agy configuration used for reset");
            Require(File.ReadAllText(claude) == claudeConfig && File.ReadAllText(codex) == "model = 'user-config'", "Reset changed the direct Claude/Codex settings");
            Require(reopened.Reset() is (0, false), "Repeating CLI reset was not idempotent");

            File.SetAttributes(codex, FileAttributes.ReadOnly);
            try
            {
                bool writeBlocked = false;
                try { reopened.Restore(backup); }
                catch (UnauthorizedAccessException) { writeBlocked = true; }
                Require(writeBlocked && File.ReadAllText(agy) == originalAgy && File.ReadAllText(statusline) == originalStatusline,
                    "A denied restore did not roll back earlier files");
            }
            finally { File.SetAttributes(codex, FileAttributes.Normal); }
            reopened.Restore(backup);
            Require(reopened.Reset() is (4, false) && File.ReadAllText(statusline) == originalStatusline,
                "Restoring an exported configuration lost its agy reset state");

            AgyCliSetup.Install(user);
            Require(reopened.Reset() is (1, true) && File.ReadAllText(agy) == originalAgy, "Legacy agy reset did not use the available settings backup");
            checks.Add("CLI config backup encrypts the selected file and restores profiles, client settings, statusline and absent files");
            checks.Add("CLI reset preserves other PowerShell edits, survives restore, rolls back failed changes and reports partial legacy recovery");
        }
        finally
        {
            foreach (string file in paths.Values.Concat(new[] { backup, invalid, agy + ".bak", AgyCliSetup.BaselinePath(data) })
                .Concat(RuntimeFiles.CliFiles.Select(name => Path.Combine(data, "cli", name))))
                if (File.Exists(file)) File.Delete(file);
            foreach (string profile in new[] { profile5, profile7 })
            {
                string directory = Path.GetDirectoryName(profile)!;
                if (!Directory.Exists(directory)) continue;
                foreach (string file in Directory.EnumerateFiles(directory, "Microsoft.PowerShell_profile.ps1.dindesk-*.bak")) File.Delete(file);
                Directory.Delete(directory);
            }
            foreach (string directory in new[] { Path.Combine(data, "cli"), data, Path.GetDirectoryName(agy)!, Path.Combine(user, ".gemini"),
                Path.GetDirectoryName(statusline)!, Path.GetDirectoryName(claude)!, Path.GetDirectoryName(codex)!, user, documents, fixture })
                if (Directory.Exists(directory) && !Directory.EnumerateFileSystemEntries(directory).Any()) Directory.Delete(directory);
        }
    }

    private static void Seed(string path, string content, Encoding? encoding = null)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, content, encoding ?? new UTF8Encoding(false));
    }

    private static void ExpectFailure(Func<int> action)
    {
        try { _ = action(); }
        catch (InvalidOperationException) { return; }
        throw new InvalidOperationException("Invalid CLI backup was accepted");
    }

    private static void Require(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }
}
