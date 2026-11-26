namespace MuseDesk;

internal interface IAccountProxy
{
    bool Running { get; }
    Task StartAsync(Settings settings);
    Task<int> CheckMuseAsync();
    void Stop();
}

internal static class AccountSwitch
{
    // Persist the new selection only after authentication succeeds. Restarting
    // the owned server discards its socket and transcript caches atomically.
    public static async Task ApplyAsync(IAccountProxy proxy, Settings previous, Settings candidate, Action<Settings> save)
    {
        SettingsStore.Validate(candidate, requireCookie:true);
        candidate.RequireValidApiKey();
        bool restore = proxy.Running;
        proxy.Stop();
        try
        {
            await proxy.StartAsync(candidate);
            await proxy.CheckMuseAsync();
            save(candidate);
        }
        catch
        {
            proxy.Stop();
            if (restore)
            {
                try { await proxy.StartAsync(previous); await proxy.CheckMuseAsync(); }
                catch { proxy.Stop(); throw new InvalidOperationException("Không kết nối được tài khoản mới và chưa khôi phục được kết nối cũ. Tài khoản đang chọn vẫn được giữ nguyên; kiểm tra mạng/cookie rồi kết nối lại."); }
            }
            throw new InvalidOperationException("Không kết nối được tài khoản đã chọn. Kiểm tra cookie/mạng. Tài khoản trước đó được giữ nguyên.");
        }
    }
}
