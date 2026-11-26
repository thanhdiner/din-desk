using System.Security.AccessControl;
using System.Security.Cryptography;
using System.Text.Json;

namespace MuseDesk;

internal static class ConfigBackup
{
    private sealed record Archive(int Version, string Kind, Dictionary<string, byte[]?> Files);

    internal static Dictionary<string, byte[]?> Capture(IReadOnlyDictionary<string, string> paths) =>
        paths.ToDictionary(entry => entry.Key, entry => ReadIfPresent(entry.Value), StringComparer.Ordinal);

    internal static void Export(string path, string kind, Dictionary<string, byte[]?> files, bool overwrite)
    {
        byte[] plain = JsonSerializer.SerializeToUtf8Bytes(new Archive(1, kind, files));
        try { WriteAtomic(path, WindowsSecrets.Protect(plain), overwrite); }
        finally { CryptographicOperations.ZeroMemory(plain); }
    }

    internal static Dictionary<string, byte[]?> Import(string path, string kind, IEnumerable<string> names)
    {
        byte[] plain = WindowsSecrets.Unprotect(File.ReadAllBytes(path));
        try
        {
            var archive = JsonSerializer.Deserialize<Archive>(plain);
            if (archive is not { Version: 1, Files: not null } || archive.Kind != kind || !names.ToHashSet(StringComparer.Ordinal).SetEquals(archive.Files.Keys))
                throw new InvalidOperationException("File sao lưu không đúng loại hoặc không đầy đủ.");
            return archive.Files;
        }
        catch (JsonException)
        {
            throw new InvalidOperationException("File sao lưu không hợp lệ.");
        }
        finally { CryptographicOperations.ZeroMemory(plain); }
    }

    internal static int Apply(IReadOnlyDictionary<string, string> paths, Dictionary<string, byte[]?> original, Dictionary<string, byte[]?> desired)
    {
        var applied = new Stack<string>();
        try
        {
            foreach (var (name, next) in desired)
            {
                byte[]? previous = original[name];
                if (Same(previous, next)) continue;
                string path = paths[name];
                if (!Same(ReadIfPresent(path), previous)) throw new InvalidOperationException("Cấu hình vừa được sửa. Hãy kiểm tra và thử lại.");
                SetFile(path, next, overwrite: previous != null);
                applied.Push(name);
            }
            return applied.Count;
        }
        catch
        {
            bool recovered = true;
            foreach (string name in applied)
            {
                try
                {
                    string path = paths[name];
                    if (Same(ReadIfPresent(path), desired[name])) SetFile(path, original[name], overwrite: desired[name] != null);
                    else recovered = false;
                }
                catch (Exception error) when (error is IOException or UnauthorizedAccessException) { recovered = false; }
            }
            if (!recovered) throw new InvalidOperationException("Không khôi phục được toàn bộ file đã thay đổi. Dùng bản sao lưu để khôi phục lại.");
            throw;
        }
    }

    internal static byte[]? ReadIfPresent(string path)
    {
        try { return File.ReadAllBytes(path); }
        catch (Exception error) when (error is FileNotFoundException or DirectoryNotFoundException) { return null; }
    }

    private static bool Same(byte[]? first, byte[]? second) => first == null ? second == null : second != null && first.AsSpan().SequenceEqual(second);

    private static void SetFile(string path, byte[]? content, bool overwrite)
    {
        if (content == null) File.Delete(path);
        else WriteAtomic(path, content, overwrite);
    }

    internal static void WriteAtomic(string path, byte[] content, bool overwrite)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        string temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            using (var output = overwrite && File.Exists(path)
                ? new FileInfo(temporary).Create(FileMode.CreateNew, FileSystemRights.Write,
                    FileShare.None, 4096, FileOptions.None, new FileInfo(path).GetAccessControl(AccessControlSections.Access))
                : new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                output.Write(content);
            }
            File.Move(temporary, path, overwrite);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }
}
