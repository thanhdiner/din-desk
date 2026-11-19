using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace MuseDesk;

internal static class RetiredProviderTests
{
    internal static void Run(string root, List<string> checks)
    {
        void Require(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
        }

        var muse = new MuseAccount { Name = "Retained Muse", Cookie = "hatch_sess=fixture-retained-muse" };
        var gemini = new MuseAccount {
            Name = "Retained Gemini", Provider = "gemini", Cookie = "__Secure-1PSID=fixture-retained-google",
            GeminiAccountIndex = 5, GeminiModelId = "56fdd199312815e2", GeminiThinkingId = "1",
            ProxyUrl = "http://127.0.0.1:8080"
        };
        var retired = new MuseAccount { Name = "Retired fixture", Provider = "deepseek", Cookie = "fixture-retired-cookie" };
        var settings = new Settings {
            Accounts = new() { muse, gemini, retired }, ActiveAccountId = retired.Id,
            Provider = "deepseek", Cookie = retired.Cookie, AutoStart = true,
            Port = 3017, ContextTokens = 500000, ProjectFolder = root
        };
        var store = new SettingsStore(root);
        string path = Path.Combine(root, "settings.protected");

        void WriteOld(Settings value)
        {
            var old = JsonSerializer.SerializeToNode(value)!.AsObject();
            old["DeepSeekToken"] = "fixture-retired-token";
            old["DeepSeekThinking"] = true;
            foreach (var account in old["Accounts"]!.AsArray()) account!["DeepSeekToken"] = "fixture-old-field";
            byte[] plain = Encoding.UTF8.GetBytes(old.ToJsonString());
            try { File.WriteAllBytes(path, WindowsSecrets.Protect(plain)); }
            finally { CryptographicOperations.ZeroMemory(plain); }
        }

        WriteOld(settings);
        var migrated = store.Load();
        Require(migrated.ActiveAccountId == null && !migrated.AutoStart && migrated.Cookie == ""
            && migrated.ProxyUrl == "" && migrated.ApiKey != settings.ApiKey,
            "Retired active account retained credentials, key or automatic connection");
        Require(migrated.Accounts.Count == 2 && migrated.Accounts[0].Cookie == muse.Cookie
            && migrated.Accounts[1].Cookie == gemini.Cookie && migrated.Accounts[1].ProxyUrl == gemini.ProxyUrl
            && migrated.Accounts[1].GeminiAccountIndex == 5 && migrated.Accounts[1].GeminiModelId == gemini.GeminiModelId
            && migrated.Accounts[1].GeminiThinkingId == gemini.GeminiThinkingId && migrated.Port == 3017
            && migrated.ContextTokens == 500000 && migrated.ProjectFolder == root,
            "Retired account cleanup changed retained accounts or preferences");
        byte[] savedPlain = WindowsSecrets.Unprotect(File.ReadAllBytes(path));
        try
        {
            string text = Encoding.UTF8.GetString(savedPlain);
            Require(!text.Contains("deepseek", StringComparison.OrdinalIgnoreCase)
                && !text.Contains("fixture-retired") && !text.Contains("fixture-old-field"),
                "Retired credentials remain in protected storage");
        }
        finally { CryptographicOperations.ZeroMemory(savedPlain); }
        Require(store.Load().ApiKey == migrated.ApiKey, "Settings migration repeats on subsequent load");
        checks.Add("Retired active provider removed from DPAPI storage; credentials/key cleared, auto-start off, Muse/Gemini accounts and preferences retained");

        settings.SelectAccount(gemini.Id);
        WriteOld(settings);
        var inactive = store.Load();
        Require(inactive.Accounts.Count == 2 && inactive.ActiveAccountId == gemini.Id && inactive.ApiKey == settings.ApiKey
            && inactive.Cookie == gemini.Cookie && inactive.AutoStart,
            "Removing inactive retired account disrupted active Gemini");
        var legacy = new Settings { Provider = "deepseek", Cookie = "fixture-retired-cookie", AutoStart = true };
        WriteOld(legacy);
        var empty = store.Load();
        Require(empty.Accounts.Count == 0 && empty.ActiveAccountId == null && empty.Cookie == "" && !empty.AutoStart,
            "Legacy retired cookie was migrated into a supported provider");
        checks.Add("Inactive provider removal preserves active Gemini and API key; retired legacy cookie cannot become Muse account");
    }
}
