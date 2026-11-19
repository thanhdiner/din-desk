using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal static class DecisionKeyTests
{
    internal static void Run(string root, List<string> checks)
    {
        string directory = Path.Combine(root, "decision");
        string path = Path.Combine(directory, "gemini-existing-keys.protected");
        if (DecisionKeys.Load(root) != "") throw new InvalidOperationException("Missing Decision store must stay optional");
        Directory.CreateDirectory(directory);
        try
        {
            var rows = new[] { "fixture-first-key", "fixture-second-key" }.Select(key => new {
                keyProtected = Convert.ToBase64String(ProtectedData.Protect(Encoding.UTF8.GetBytes(key), null, DataProtectionScope.CurrentUser))
            });
            byte[] bytes = JsonSerializer.SerializeToUtf8Bytes(rows);
            File.WriteAllBytes(path, ProtectedData.Protect(bytes, null, DataProtectionScope.CurrentUser));
            CryptographicOperations.ZeroMemory(bytes);
            if (DecisionKeys.Load(root) != "fixture-first-key") throw new InvalidOperationException("Decision must use the selected first key without rotating projects");
            string stored = Encoding.UTF8.GetString(File.ReadAllBytes(path));
            if (stored.Contains("fixture-first-key") || stored.Contains("fixture-second-key")) throw new InvalidOperationException("Decision key stored in plaintext");
            checks.Add("Optional Decision credential store decrypts with DPAPI, remains separate from accounts and does not rotate keys");
        }
        finally
        {
            if (File.Exists(path)) File.Delete(path);
            if (Directory.Exists(directory) && !Directory.EnumerateFileSystemEntries(directory).Any()) Directory.Delete(directory);
        }
    }
}
