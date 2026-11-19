using System.Globalization;
using System.Net;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal interface AccountQuota
{
    string Summary { get; }
    string Detail { get; }
}

// Stateless dispatcher: no timers, shared cookies, chat bootstrap or active-account changes.
internal sealed class AccountQuotaClient(Func<string, HttpMessageHandler>? handlerFactory = null)
{
    private readonly Func<string, HttpMessageHandler> createHandler = handlerFactory ?? AccountProxy.Handler;

    public async Task<AccountQuota> FetchAccountAsync(MuseAccount account, CancellationToken token = default)
    {
        SettingsStore.ValidateProviderCookie(account.Cookie, account.Provider);
        if (account.Provider == "gemini")
        {
            using var client = new GeminiQuotaClient(createHandler(account.ProxyUrl));
            return await client.FetchAsync(account.Cookie, token, account.GeminiAccountIndex).ConfigureAwait(false);
        }
        if (account.Provider == "muse")
        {
            using var client = new MuseQuotaClient(createHandler(account.ProxyUrl));
            return await client.FetchAsync(account.Cookie, token).ConfigureAwait(false);
        }
        throw new ArgumentException("Nguồn tài khoản không hợp lệ.");
    }
}

internal sealed class GeminiQuotaClient : IDisposable
{
    // Read-only UsagePageQuery used by gemini.google.com/usage. Protocol reference:
    // https://github.com/steipete/CodexBar/pull/4535 (fraction units, not percentages).
    private const string QuerySignature = "2/dWbCvkVB0X16r9NPnxifeOAAqdCqMf0xeWvau9yPy7g=";
    private const string Variables = "{\"g1Request\":{\"entrypoints\":[\"GEMINI_USAGE_SETTINGS_UPGRADE_BANNER\"]},\"workspaceRequest\":{\"recommendationsForSurfaceRequests\":[]}}";
    private const int MaximumResponseBytes = 256 * 1024;
    private readonly HttpClient http;

    public GeminiQuotaClient(HttpMessageHandler? handler = null) =>
        http = new HttpClient(handler ?? AccountProxy.Handler("")) { Timeout = TimeSpan.FromSeconds(20) };

    public async Task<GeminiQuota> FetchAsync(string cookie, CancellationToken token = default, int accountIndex = 0)
    {
        SettingsStore.ValidateProviderCookie(cookie, "gemini");
        SettingsStore.ValidateGeminiSelection(accountIndex,"","");
        var url = $"https://gemini.google.com/{(accountIndex==0?"":$"u/{accountIndex}/")}_/BardChatUi/graphql/schemas/GEMINI_WEB_GRAPHQL/executeQuery"
            + "?query_signature=" + Uri.EscapeDataString(QuerySignature) + "&variables=" + Uri.EscapeDataString(Variables);
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Add("Cookie", cookie);
        request.Headers.Accept.ParseAdd("application/json");
        request.Headers.Referrer = new Uri(accountIndex==0?"https://gemini.google.com/usage":$"https://gemini.google.com/u/{accountIndex}/usage");
        request.Headers.Add("X-Goog-AuthUser", accountIndex.ToString(CultureInfo.InvariantCulture));
        request.Headers.UserAgent.ParseAdd("Mozilla/5.0");
        request.Headers.CacheControl = new() { NoCache = true };
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(20));
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, deadline.Token).ConfigureAwait(false);
        if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden || (int)response.StatusCode is >= 300 and < 400)
            throw SignedOut();
        if (response.StatusCode == HttpStatusCode.TooManyRequests)
            throw new InvalidOperationException("Gemini đang giới hạn lượt kiểm tra. Đợi một lúc rồi bấm lại.");
        if (!response.IsSuccessStatusCode)
            throw new InvalidOperationException("Gemini chưa trả được quota. Thử lại sau.");
        if (response.Content.Headers.ContentLength > MaximumResponseBytes) throw Unavailable();
        using var source = await response.Content.ReadAsStreamAsync(deadline.Token).ConfigureAwait(false);
        using var body = new MemoryStream();
        var buffer = new byte[8192];
        int count;
        while ((count = await source.ReadAsync(buffer, deadline.Token).ConfigureAwait(false)) != 0)
        {
            if (body.Length + count > MaximumResponseBytes) throw Unavailable();
            body.Write(buffer, 0, count);
        }
        return Parse(Encoding.UTF8.GetString(body.GetBuffer(), 0, (int)body.Length));
    }

    internal static GeminiQuota Parse(string body)
    {
        string json = body.TrimStart();
        if (json.StartsWith('<')) throw SignedOut();
        if (json.StartsWith(")]}'", StringComparison.Ordinal)) json = json[4..].TrimStart();
        try
        {
            using var doc = JsonDocument.Parse(json, new JsonDocumentOptions { MaxDepth = 48 });
            if (doc.RootElement.ValueKind != JsonValueKind.Array) throw Unavailable();
            GeminiQuota? quota = null;
            foreach (var result in doc.RootElement.EnumerateArray())
            {
                if (!Property(result, "data", out var data) || !Property(data, "viewer", out var viewer)) continue;
                if (viewer.ValueKind == JsonValueKind.Null) throw SignedOut();
                if (!Property(viewer, "rpcs", out var rpcs) || !Property(rpcs, "getUsageInfo", out var usage)
                    || !Property(usage, "pools", out var pools) || pools.ValueKind != JsonValueKind.Array) throw Unavailable();
                GeminiQuotaWindow? fiveHours = null, weekly = null;
                foreach (var pool in pools.EnumerateArray())
                {
                    string? type = Text(pool, "poolType");
                    if (type is not ("GXU_FIVE_HOURLY" or "GXU_WEEKLY")) continue;
                    if (!Property(pool, "percentageUsed", out var percent) || percent.ValueKind != JsonValueKind.Number
                        || !percent.TryGetDouble(out double fraction) || !double.IsFinite(fraction) || fraction < 0) throw Unavailable();
                    DateTimeOffset? reset = null;
                    if (Property(pool, "gxu", out var gxu) && Text(gxu, "windowRefreshesAt") is { } raw
                        && DateTimeOffset.TryParse(raw, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var timestamp)) reset = timestamp;
                    var window = new GeminiQuotaWindow(Math.Min(100, fraction * 100), reset);
                    if (type == "GXU_FIVE_HOURLY") { if (fiveHours != null) throw Unavailable(); fiveHours = window; }
                    else { if (weekly != null) throw Unavailable(); weekly = window; }
                }
                if (quota != null) throw Unavailable();
                if (fiveHours == null && weekly == null)
                    throw new InvalidOperationException("Tài khoản này chưa được Google cung cấp dữ liệu quota Gemini Web. Xem tại gemini.google.com/usage.");
                string? tier = Text(usage, "subscriptionTier");
                quota = new GeminiQuota(tier == "TIER1" ? "Google AI Pro" : null, fiveHours, weekly, DateTimeOffset.Now);
            }
            return quota ?? throw Unavailable();
        }
        catch (JsonException) { throw Unavailable(); }
    }

    private static bool Property(JsonElement node, string key, out JsonElement value)
    {
        value = default;
        return node.ValueKind == JsonValueKind.Object && node.TryGetProperty(key, out value);
    }
    private static string? Text(JsonElement node, string key) =>
        Property(node, key, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
    private static InvalidOperationException SignedOut() => new("Cookie Gemini hết hạn hoặc chưa đủ quyền. Lấy lại header Cookie tại gemini.google.com/usage rồi cập nhật tài khoản.");
    private static InvalidOperationException Unavailable() => new("Không đọc được quota Gemini Web. API có thể đã thay đổi; xem gemini.google.com/usage hoặc cập nhật app.");
    public void Dispose() => http.Dispose();
}

internal sealed record GeminiQuotaWindow(double PercentUsed, DateTimeOffset? ResetsAt)
{
    public string PercentText => Math.Round(PercentUsed, 0, MidpointRounding.AwayFromZero).ToString("0", CultureInfo.CurrentCulture) + "%";
    public string Summary => $"{PercentText} đã dùng";
    public string Detail(string label) => $"{label}: {Summary}"
        + (ResetsAt is { } reset ? $" · Reset {reset.ToLocalTime():dd/MM/yyyy HH:mm} (giờ máy)" : " · Google chưa báo giờ reset");
}

internal sealed record GeminiQuota(string? Plan, GeminiQuotaWindow? FiveHours, GeminiQuotaWindow? Weekly, DateTimeOffset CheckedAt) : AccountQuota
{
    public string Summary => string.Join(" · ", new[] { FiveHours is { } five ? $"5h: {five.PercentText}" : null,
        Weekly is { } week ? $"Tuần: {week.PercentText}" : null }.Where(text => text != null));
    public string Detail => string.Join(Environment.NewLine, new[] {
        Plan == null ? "Gemini Web" : $"Gemini Web · {Plan}",
        FiveHours?.Detail("5 giờ") ?? "5 giờ: Google chưa cung cấp dữ liệu",
        Weekly?.Detail("Hàng tuần") ?? "Hàng tuần: Google chưa cung cấp dữ liệu",
        $"Kiểm tra lúc {CheckedAt:dd/MM HH:mm:ss} · Bấm để cập nhật."
    });
}
