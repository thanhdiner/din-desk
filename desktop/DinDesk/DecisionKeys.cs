using System.Security.Cryptography;
using System.Text.Json;

namespace MuseDesk;

internal static class DecisionKeys
{
    // This optional store is separate from Muse/Gemini Web account credentials.
    // Select one key; never rotate projects automatically to work around quotas.
    internal static string Load(string dataDirectory)
    {
        string path = Path.Combine(dataDirectory, "decision", "gemini-existing-keys.protected");
        if (!File.Exists(path)) return "";
        byte[] clear = ProtectedData.Unprotect(File.ReadAllBytes(path), null, DataProtectionScope.CurrentUser);
        try
        {
            using var document = JsonDocument.Parse(clear);
            var items = document.RootElement;
            if (items.ValueKind != JsonValueKind.Array || items.GetArrayLength() == 0)
                throw new InvalidOperationException("Kho key Decision không có key đã lưu.");
            byte[] key = ProtectedData.Unprotect(Convert.FromBase64String(items[0].GetProperty("keyProtected").GetString()!), null, DataProtectionScope.CurrentUser);
            try { return System.Text.Encoding.UTF8.GetString(key); }
            finally { CryptographicOperations.ZeroMemory(key); }
        }
        finally { CryptographicOperations.ZeroMemory(clear); }
    }
}
