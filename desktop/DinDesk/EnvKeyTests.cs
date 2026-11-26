using System.Text;

namespace MuseDesk;

internal static class EnvKeyTests
{
    internal static void Run(string root, List<string> checks)
    {
        string dir = Path.Combine(root, "env-key-fixture");
        Directory.CreateDirectory(dir);
        string file = Path.Combine(dir, ".env");
        string missingPath = Path.Combine(dir, ".env.missing");
        const string variable = "CLIENT_PROXY_KEY";
        const string contents = "# Giữ nguyên chú thích\r\n"
            + "BANNER=\"first line\r\nCLIENT_PROXY_KEY=part of banner\r\nlast line\"\n"
            + "  export CLIENT_PROXY_KEY = 'old#key' \t# proxy key\r"
            + "MUSE_PROXY_API_KEY=another-variable\r\nKEEP=some value";

        try
        {
            var store = new SettingsStore(dir);
            var settings = store.Load();
            Require(settings.EnvKeyFilePath == null && settings.EnvKeyVariable == "MUSE_PROXY_API_KEY", "Missing .env target defaults were incorrect");
            settings.EnvKeyFilePath = file;
            settings.EnvKeyVariable = variable;
            store.Save(settings);
            string beforeRotation = settings.ApiKey;
            File.WriteAllText(file, contents, new UTF8Encoding(false));
            var rotated = settings.Copy();
            rotated.RotateApiKey(DateTimeOffset.UtcNow);
            store.Save(rotated);
            Require(File.ReadAllText(file) == contents, "Rotating the key also modified the external file");

            var reopened = new SettingsStore(dir).Load();
            Require(reopened.EnvKeyFilePath == file && reopened.EnvKeyVariable == variable && reopened.ApiKey != beforeRotation,
                "Saved .env target or rotated key did not survive reload");
            EnvKeyFile.Update(reopened);
            string expected = contents.Replace("'old#key'", $"'{reopened.ApiKey}'", StringComparison.Ordinal);
            Require(File.ReadAllBytes(file).AsSpan().SequenceEqual(Encoding.UTF8.GetBytes(expected)), "Updating .env changed bytes outside the requested value");
            EnvKeyFile.Update(reopened);
            Require(File.ReadAllText(file) == expected, "Repeating .env update changed the result");

            const string unicode = "CLIENT_PROXY_KEY=  # leave this comment\r\nKEEP=tiếng Việt\r\n";
            File.WriteAllText(file, unicode, Encoding.Unicode);
            EnvKeyFile.Update(reopened);
            string expectedUnicode = unicode.Replace("KEY=", $"KEY={reopened.ApiKey}", StringComparison.Ordinal);
            byte[] expectedBytes = [..Encoding.Unicode.GetPreamble(), ..Encoding.Unicode.GetBytes(expectedUnicode)];
            Require(File.ReadAllBytes(file).AsSpan().SequenceEqual(expectedBytes), "Updating an empty UTF-16 value changed its encoding or comment");

            VerifyRejected("KEEP=unchanged\r\n", "Không tìm thấy biến");
            VerifyRejected("CLIENT_PROXY_KEY='one'\nexport CLIENT_PROXY_KEY=two\r\n", "xuất hiện nhiều lần");
            VerifyRejected("CLIENT_PROXY_KEY=\"unfinished\nKEEP=unchanged\n", "thiếu dấu nháy");
            reopened.EnvKeyFilePath = missingPath;
            ExpectRejection(reopened, "Không tìm thấy file");
            Require(!File.Exists(missingPath), "An external file was created for a missing target");

            reopened.EnvKeyFilePath = file;
            string previousKey = "CLIENT_PROXY_KEY=before\r\n";
            File.WriteAllText(file, previousKey);
            reopened.ApiKeyExpiresAt = DateTimeOffset.UtcNow.AddMinutes(-1);
            ExpectRejection(reopened, "hết hạn");
            Require(File.ReadAllText(file) == previousKey, "An expired key modified the external file");
            reopened.ApiKeyExpiresAt = null;
            File.SetAttributes(file, FileAttributes.ReadOnly);
            try { ExpectRejection(reopened, "Không ghi được file"); }
            finally { File.SetAttributes(file, FileAttributes.Normal); }
            Require(File.ReadAllText(file) == previousKey, "A failed write changed the read-only file");
            Require(!Directory.EnumerateFiles(dir, ".dindesk-*").Any(), "Updating the .env file left a temporary key copy");
            checks.Add("Manual .env key sync persists the chosen target and preserves quotes, comments, encoding and unrelated values");
            checks.Add(".env key sync rejects missing, duplicate, expired, malformed and read-only targets without changing file contents");

            void VerifyRejected(string input, string message)
            {
                File.WriteAllText(file, input);
                ExpectRejection(reopened, message);
                Require(File.ReadAllText(file) == input, "Rejected .env sync modified the file");
            }
        }
        finally
        {
            File.Delete(file);
            File.Delete(missingPath);
            File.Delete(Path.Combine(dir, "settings.protected"));
            if (!Directory.EnumerateFileSystemEntries(dir).Any()) Directory.Delete(dir);
        }
    }

    private static void ExpectRejection(Settings settings, string expected)
    {
        try { EnvKeyFile.Update(settings); }
        catch (InvalidOperationException error) when (error.Message.Contains(expected, StringComparison.Ordinal)) { return; }
        throw new InvalidOperationException("Expected .env update to be rejected");
    }

    private static void Require(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }
}
