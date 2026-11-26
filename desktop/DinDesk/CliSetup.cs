using System.Text;
using System.Text.RegularExpressions;

namespace MuseDesk;

internal static class CliSetup
{
    private sealed record ProfileChange(string Path, byte[]? Original, byte[] Updated);

    internal static string ProfilePath(string documentsDirectory, string shell) =>
        Path.Combine(documentsDirectory, shell, "Microsoft.PowerShell_profile.ps1");

    internal static int Install(string dataDirectory, string documentsDirectory, string command)
    {
        string alias = command switch { "claude" => "cy", "codex" => "cx", _ => throw new ArgumentException("CLI không hợp lệ.") };
        if (string.IsNullOrWhiteSpace(documentsDirectory)) throw new InvalidOperationException("Không tìm thấy thư mục Documents.");
        dataDirectory = Path.GetFullPath(dataDirectory);

        using var profileLock = new Mutex(false, "Local\\DinDesk-PowerShellProfiles-" + Environment.UserName);
        try { if (!profileLock.WaitOne(TimeSpan.FromSeconds(5))) throw new InvalidOperationException("Profile PowerShell đang được cập nhật. Thử lại sau."); }
        catch (AbandonedMutexException) { }
        try
        {
            RuntimeFiles.UpdateCli(dataDirectory);
            var changes = new List<ProfileChange>();
            foreach (string shell in new[] { "WindowsPowerShell", "PowerShell" })
            {
                string path = ProfilePath(documentsDirectory, shell);
                byte[]? original = ConfigBackup.ReadIfPresent(path);
                byte[] updated = UpdateProfile(original ?? [], alias, command, dataDirectory);
                if (original != null && original.AsSpan().SequenceEqual(updated)) continue;
                changes.Add(new(path, original, updated));
            }

            foreach (var change in changes)
            {
                if (change.Original == null) continue;
                string backup = change.Path + $".dindesk-{DateTime.UtcNow:yyyyMMdd-HHmmss}-{Guid.NewGuid():N}.bak";
                using var file = new FileStream(backup, FileMode.CreateNew);
                file.Write(change.Original);
            }
            var applied = new Stack<ProfileChange>();
            try
            {
                foreach (var change in changes)
                {
                    if (change.Original != null && !File.ReadAllBytes(change.Path).AsSpan().SequenceEqual(change.Original))
                        throw new InvalidOperationException("Profile PowerShell vừa được sửa. Bấm Setup lại để dùng bản mới nhất.");
                    ConfigBackup.WriteAtomic(change.Path, change.Updated, overwrite: change.Original != null);
                    applied.Push(change);
                }
            }
            catch
            {
                foreach (var change in applied)
                {
                    if (!File.Exists(change.Path) || !File.ReadAllBytes(change.Path).AsSpan().SequenceEqual(change.Updated)) continue;
                    if (change.Original == null) File.Delete(change.Path);
                    else ConfigBackup.WriteAtomic(change.Path, change.Original, overwrite: true);
                }
                throw;
            }
            return changes.Count(change => change.Original != null);
        }
        finally { profileLock.ReleaseMutex(); }
    }

    private static byte[] UpdateProfile(byte[] bytes, string alias, string command, string dataDirectory)
    {
        var (encoding, existing) = ReadProfile(bytes);
        string newline = existing.Contains('\n') && !existing.Contains("\r\n") ? "\n" : "\r\n";
        string directory = dataDirectory.All(char.IsAscii)
            ? "'" + dataDirectory.Replace("'", "''", StringComparison.Ordinal) + "'"
            : "([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('" + Convert.ToBase64String(Encoding.UTF8.GetBytes(dataDirectory)) + "')))";
        string start = $"# >>> Din Desk {alias} >>>";
        string end = $"# <<< Din Desk {alias} <<<";
        string managed = string.Join(newline, start,
            $"if (Test-Path -LiteralPath Alias:{alias}) {{ Remove-Item -LiteralPath Alias:{alias} -Force }}",
            $"function global:{alias} {{",
            $"    $deskDirectory = {directory}",
            $"    & (Join-Path $deskDirectory 'cli/{command}-muse.ps1'){(command == "claude" ? " -MuseDesk" : "")} -DeskDirectory $deskDirectory -FullAccess @args",
            "}", end, "");
        string pattern = ManagedPattern(alias);
        var matches = Regex.Matches(existing, pattern);
        if (matches.Count == 1 && matches[0].Value == managed) return bytes;
        string previous = Regex.Replace(existing, pattern, "");
        if (previous.Length > 0 && !previous.EndsWith('\n')) previous += newline;
        return [..encoding.GetPreamble(), ..encoding.GetBytes(previous + managed)];
    }

    internal static byte[]? WithoutManagedAliases(byte[]? bytes)
    {
        if (bytes == null) return null;
        var (encoding, text) = ReadProfile(bytes);
        string original = Regex.Replace(Regex.Replace(text, ManagedPattern("cy"), ""), ManagedPattern("cx"), "");
        return original == text ? bytes : [..encoding.GetPreamble(), ..encoding.GetBytes(original)];
    }

    private static string ManagedPattern(string alias) =>
        $"(?m)^# >>> Din Desk {alias} >>>\\r?\\n[\\s\\S]*?^# <<< Din Desk {alias} <<<[^\\S\\r\\n]*(?:\\r?\\n|$)";

    private static (Encoding Encoding, string Text) ReadProfile(byte[] bytes)
    {
        // The managed block uses ASCII, so a profile without a BOM can retain its original bytes.
        Encoding encoding = new Encoding[] { Encoding.UTF32, new UTF32Encoding(true, true), Encoding.UTF8, Encoding.Unicode, Encoding.BigEndianUnicode }
            .FirstOrDefault(candidate => bytes.AsSpan().StartsWith(candidate.Preamble)) ?? Encoding.Latin1;
        return (encoding, encoding.GetString(bytes.AsSpan(encoding.Preamble.Length)));
    }
}
