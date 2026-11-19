using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal sealed class Settings
{
    public List<MuseAccount> Accounts { get; set; } = new();
    public string? ActiveAccountId { get; set; }
    public string Cookie { get; set; } = "";
    public string ProxyUrl { get; set; } = "";
    public string Provider { get; set; } = "muse";
    public int GeminiAccountIndex { get; set; }
    public string GeminiModelId { get; set; } = "";
    public string GeminiThinkingId { get; set; } = "";
    public string ModelId => Provider switch {"gemini"=>"gemini",_=>"muse"};
    public string NodeId { get; set; } = "musedesk-" + Guid.NewGuid().ToString("N");
    public string ApiKey { get; set; } = Convert.ToHexString(RandomNumberGenerator.GetBytes(32));
    public int Port { get; set; } = 3001;
    public int ContextTokens { get; set; } = 256000;
    public string ProjectFolder { get; set; } = Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments);
    public bool AutoStart { get; set; }

    public Settings Copy() => JsonSerializer.Deserialize<Settings>(JsonSerializer.Serialize(this))!;
    public void NormalizeAccounts()
    {
        if (Accounts.Count == 0 && !string.IsNullOrWhiteSpace(Cookie))
        {
            var migrated = new MuseAccount { Name = "Tài khoản hiện tại", Cookie = Cookie, NodeId = NodeId, ProxyUrl = ProxyUrl, Provider = Provider };
            Accounts.Add(migrated); ActiveAccountId = migrated.Id;
        }
        if (ActiveAccountId != null)
        {
            var account = Accounts.SingleOrDefault(a => a.Id == ActiveAccountId)
                ?? throw new ArgumentException("Tài khoản đang chọn không còn tồn tại.");
            Cookie = account.Cookie; NodeId = account.NodeId; ProxyUrl = account.ProxyUrl; Provider = account.Provider;
            GeminiAccountIndex = account.GeminiAccountIndex; GeminiModelId = account.GeminiModelId; GeminiThinkingId = account.GeminiThinkingId;
        }
        else { Cookie = ""; ProxyUrl = ""; Provider = "muse"; GeminiAccountIndex=0;GeminiModelId=GeminiThinkingId=""; }
    }
    internal bool RemoveRetiredAccounts()
    {
        bool activeRetired = Provider == "deepseek"
            || Accounts.Any(a => a.Id == ActiveAccountId && a.Provider == "deepseek");
        int removed = Accounts.RemoveAll(a => a.Provider == "deepseek");
        if (activeRetired)
        {
            ActiveAccountId = null;
            Cookie = ProxyUrl = "";
            Provider = "muse";
            ApiKey = Convert.ToHexString(RandomNumberGenerator.GetBytes(32));
            AutoStart = false;
        }
        return removed > 0 || activeRetired;
    }
    public void SelectAccount(string id)
    {
        if (!Accounts.Any(a => a.Id == id)) throw new ArgumentException("Không tìm thấy tài khoản.");
        if(ActiveAccountId!=id) ApiKey=Convert.ToHexString(RandomNumberGenerator.GetBytes(32));
        ActiveAccountId = id; NormalizeAccounts();
    }
}

internal sealed class MuseAccount
{
    public string Id { get; set; } = Guid.NewGuid().ToString("N");
    public string Name { get; set; } = "";
    public string Cookie { get; set; } = "";
    public string ProxyUrl { get; set; } = "";
    public string Provider { get; set; } = "muse";
    public int GeminiAccountIndex { get; set; }
    public string GeminiModelId { get; set; } = "";
    public string GeminiThinkingId { get; set; } = "";
    public string GeminiModelName { get; set; } = "";
    public string GeminiThinkingName { get; set; } = "";
    public string ProviderLabel => Provider switch {"gemini"=>"Gemini",_=>"Muse"};
    public string NodeId { get; set; } = "musedesk-" + Guid.NewGuid().ToString("N");
    public override string ToString() => Name;
}

internal sealed class SettingsStore(string directory)
{
    public string DirectoryPath { get; } = Path.GetFullPath(directory);
    private string FilePath => Path.Combine(DirectoryPath, "settings.protected");
    public static string DefaultDirectory => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "DinDesk");
    private static string LegacyDirectory => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "MuseDesk");

    public Settings Load()
    {
        if (!File.Exists(FilePath) && string.Equals(DirectoryPath, DefaultDirectory, StringComparison.OrdinalIgnoreCase))
        {
            string legacyFile = Path.Combine(LegacyDirectory, "settings.protected");
            if (File.Exists(legacyFile))
            {
                Directory.CreateDirectory(DirectoryPath);
                File.Copy(legacyFile, FilePath, overwrite: false);
            }
        }
        if (!File.Exists(FilePath)) return new Settings();
        byte[] plain = WindowsSecrets.Unprotect(File.ReadAllBytes(FilePath));
        try
        {
            var settings = JsonSerializer.Deserialize<Settings>(plain) ?? throw new InvalidDataException();
            // Remove retired accounts before legacy cookie migration can relabel them.
            bool retired = settings.RemoveRetiredAccounts();
            settings.NormalizeAccounts();
            Validate(settings, requireCookie: false);
            // Re-serialize into DPAPI storage to discard retired credential fields too.
            using var original = JsonDocument.Parse(plain);
            bool oldFields = original.RootElement.TryGetProperty("DeepSeekToken", out _)
                || original.RootElement.TryGetProperty("DeepSeekThinking", out _)
                || (original.RootElement.TryGetProperty("Accounts", out var accounts)
                    && accounts.EnumerateArray().Any(a => a.TryGetProperty("DeepSeekToken", out _)
                        || a.TryGetProperty("DeepSeekThinking", out _)));
            if (retired || oldFields) Save(settings);
            return settings;
        }
        finally { CryptographicOperations.ZeroMemory(plain); }
    }

    public void Save(Settings settings)
    {
        settings.NormalizeAccounts();
        Validate(settings, requireCookie: false);
        Directory.CreateDirectory(DirectoryPath);
        byte[] plain = JsonSerializer.SerializeToUtf8Bytes(settings);
        string temporary = FilePath + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.WriteAllBytes(temporary, WindowsSecrets.Protect(plain));
            File.Move(temporary, FilePath, overwrite: true);
        }
        finally
        {
            CryptographicOperations.ZeroMemory(plain);
            if (File.Exists(temporary)) File.Delete(temporary);
        }
    }

    public static void Validate(Settings settings, bool requireCookie)
    {
        if (settings.Accounts.Count > 100) throw new ArgumentException("Chỉ lưu tối đa 100 tài khoản.");
        if (settings.Accounts.Select(a => a.Id).Distinct().Count() != settings.Accounts.Count) throw new ArgumentException("ID tài khoản bị trùng.");
        if (settings.Accounts.Select(a => a.Name).Distinct(StringComparer.OrdinalIgnoreCase).Count() != settings.Accounts.Count) throw new ArgumentException("Tên tài khoản đã tồn tại.");
        foreach (var account in settings.Accounts)
        {
            if (!Guid.TryParseExact(account.Id,"N",out _) || string.IsNullOrWhiteSpace(account.Name) || account.Name.Length > 80 || string.IsNullOrWhiteSpace(account.NodeId)) throw new ArgumentException("Thông tin tài khoản không hợp lệ.");
            ValidateProviderCookie(account.Cookie,account.Provider);
            AccountProxy.Parse(account.ProxyUrl);
            ValidateGeminiSelection(account.GeminiAccountIndex,account.GeminiModelId,account.GeminiThinkingId);
        }
        if (settings.Port is < 1024 or > 65535) throw new ArgumentException("Cổng phải nằm trong khoảng 1024–65535.");
        if (settings.ContextTokens is < 64000 or > 1000000) throw new ArgumentException("Context phải nằm trong khoảng 64k–1M.");
        if (string.IsNullOrWhiteSpace(settings.NodeId) || string.IsNullOrWhiteSpace(settings.ApiKey)) throw new ArgumentException("Cấu hình nội bộ bị thiếu. Hãy mở lại ứng dụng.");
        if (settings.Provider is not ("muse" or "gemini")) throw new ArgumentException("Nguồn tài khoản không hợp lệ.");
        if (requireCookie) ValidateProviderCookie(settings.Cookie,settings.Provider);
        if (settings.Cookie.Any(c => c is '\r' or '\n')) throw new ArgumentException("Cookie phải nằm trên một dòng. Chỉ sao chép giá trị của header Cookie.");
    }
    internal static void ValidateCookie(string cookie)
    {
        if (cookie.Any(c => c is '\r' or '\n')) throw new ArgumentException("Cookie phải nằm trên một dòng.");
        if (!cookie.Split(';').Any(part => part.Trim().StartsWith("hatch_sess=", StringComparison.Ordinal) && part.Trim().Length > 11))
            throw new ArgumentException("Cookie cần có hatch_sess. Xem tab Hướng dẫn để lấy cookie.");
    }

    internal static void ValidateGeminiSelection(int index,string model,string thinking)
    {
        if(index is <0 or >99 || model.Length>128 || thinking.Length>160 ||
            (model+thinking).Any(c=>!char.IsAsciiLetterOrDigit(c) && !"-_:".Contains(c)) ||
            (model.Length==0 && thinking.Length>0)) throw new ArgumentException("Lựa chọn tài khoản/model/tư duy Gemini không hợp lệ.");
    }

    internal static void ValidateProviderCookie(string cookie,string provider)
    {
        if(provider=="muse") {ValidateCookie(cookie);return;}
        if(provider!="gemini") throw new ArgumentException("Nguồn tài khoản không hợp lệ.");
        if(cookie.Length>65536 || cookie.Any(char.IsControl) || !cookie.Split(';').Any(part=>part.Trim().StartsWith("__Secure-1PSID=",StringComparison.Ordinal) && part.Trim().Length>"__Secure-1PSID=".Length))
            throw new ArgumentException("Cookie Gemini cần __Secure-1PSID; kèm __Secure-1PSIDTS nếu có. Sao chép header Cookie trên một dòng.");
    }
}

/// DPAPI CurrentUser: only the same Windows user can decrypt the saved config.
internal static class WindowsSecrets
{
    [StructLayout(LayoutKind.Sequential)]
    private struct Blob { public int Length; public IntPtr Data; }
    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptProtectData(ref Blob input, string description, IntPtr entropy, IntPtr reserved, IntPtr prompt, uint flags, out Blob output);
    [DllImport("crypt32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptUnprotectData(ref Blob input, IntPtr description, IntPtr entropy, IntPtr reserved, IntPtr prompt, uint flags, out Blob output);
    [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr memory);

    public static byte[] Protect(byte[] input) => Transform(input, true);
    public static byte[] Unprotect(byte[] input) => Transform(input, false);
    private static byte[] Transform(byte[] bytes, bool protect)
    {
        var input = new Blob { Length = bytes.Length, Data = Marshal.AllocHGlobal(bytes.Length) };
        Blob output = default;
        try
        {
            Marshal.Copy(bytes, 0, input.Data, bytes.Length);
            bool ok = protect ? CryptProtectData(ref input, "Din Desk", IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, 1, out output)
                : CryptUnprotectData(ref input, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, 1, out output);
            if (!ok) throw new CryptographicException("Không đọc được cấu hình mã hóa trên tài khoản Windows này.");
            byte[] result = new byte[output.Length];
            Marshal.Copy(output.Data, result, 0, output.Length);
            return result;
        }
        finally
        {
            Marshal.Copy(new byte[bytes.Length], 0, input.Data, bytes.Length);
            Marshal.FreeHGlobal(input.Data);
            if (output.Data != IntPtr.Zero) LocalFree(output.Data);
        }
    }
}
