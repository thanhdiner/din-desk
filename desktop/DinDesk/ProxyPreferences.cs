namespace MuseDesk;

internal static class ProxyPreferences
{
    internal static async Task<bool> ApplyAsync(IAccountProxy proxy, Settings previous, Settings candidate, Action<Settings> save)
    {
        SettingsStore.Validate(candidate, requireCookie: false);
        bool restart = proxy.Running && (candidate.Port != previous.Port || candidate.ApiKey != previous.ApiKey
            || candidate.ApiKeyExpiresAt != previous.ApiKeyExpiresAt);
        if (restart) proxy.Stop();
        try { save(candidate); }
        catch
        {
            if (restart && !previous.IsApiKeyExpired(DateTimeOffset.UtcNow))
            {
                try { await proxy.StartAsync(previous); await proxy.CheckMuseAsync(); }
                catch { proxy.Stop(); }
            }
            throw new InvalidOperationException("Không lưu được cài đặt proxy. Kiểm tra quyền ghi thư mục và thử lại.");
        }
        if (!restart || candidate.IsApiKeyExpired(DateTimeOffset.UtcNow)) return false;
        try
        {
            await proxy.StartAsync(candidate);
            await proxy.CheckMuseAsync();
            return true;
        }
        catch
        {
            proxy.Stop();
            throw new InvalidOperationException("Đã lưu cài đặt proxy. Bấm Kết nối lại để chạy proxy với cấu hình vừa lưu.");
        }
    }
}
