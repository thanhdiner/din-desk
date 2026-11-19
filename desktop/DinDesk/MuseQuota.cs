using System.Globalization;
using System.Net;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

// Read-only Muse web action, verified against the subscription screen. This is
// separate from chat/VM bootstrap and runs only after an explicit button click.
internal sealed class MuseQuotaClient : IDisposable
{
    private const string SubscriptionAction = "40e0249eeae158685b6a8d0c3b203f53dd4f2c7d63";
    private const int MaximumResponseBytes = 256 * 1024;
    private readonly HttpClient http;

    public MuseQuotaClient(HttpMessageHandler? handler = null)
    {
        http = new HttpClient(handler ?? AccountProxy.Handler("")) { Timeout = TimeSpan.FromSeconds(20) };
    }

    public async Task<MuseQuota> FetchAccountAsync(MuseAccount account, CancellationToken cancellationToken = default)
    {
        if(account.Provider!="muse")throw new ArgumentException("Quota hiện chỉ hỗ trợ tài khoản Muse.");
        using var client = new MuseQuotaClient(AccountProxy.Handler(account.ProxyUrl));
        return await client.FetchAsync(account.Cookie, cancellationToken).ConfigureAwait(false);
    }

    public async Task<MuseQuota> FetchAsync(string cookie, CancellationToken cancellationToken = default)
    {
        SettingsStore.ValidateCookie(cookie);
        using var request = new HttpRequestMessage(HttpMethod.Post, "https://muse.ai/");
        // Per-request credentials: checking another saved account never changes
        // the active proxy, its cookies, API key, connections or conversations.
        request.Headers.Add("Cookie", cookie);
        request.Headers.Add("Origin", "https://muse.ai");
        request.Headers.Referrer = new Uri("https://muse.ai/");
        request.Headers.Add("Sec-Fetch-Site", "same-origin");
        request.Headers.Add("Sec-Fetch-Mode", "cors");
        request.Headers.Add("Sec-Fetch-Dest", "empty");
        request.Headers.Add("Next-Action", SubscriptionAction);
        request.Headers.Add("Accept", "text/x-component");
        request.Headers.UserAgent.ParseAdd("Mozilla/5.0");
        request.Content = new StringContent("[{\"includeAgreement\":true}]", Encoding.UTF8, "text/plain");
        // ResponseHeadersRead needs an explicit deadline for the body as well.
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(20));
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, deadline.Token).ConfigureAwait(false);
        if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
            throw new InvalidOperationException("Không đọc được quota. Cookie có thể đã hết hạn; cập nhật cookie rồi thử lại.");
        if (response.StatusCode == HttpStatusCode.TooManyRequests)
            throw new InvalidOperationException("Muse đang giới hạn lượt kiểm tra. Đợi một lúc rồi bấm lại.");
        if (!response.IsSuccessStatusCode)
            throw new InvalidOperationException("Muse chưa trả được quota. Thử lại sau; nếu vẫn lỗi, API quota có thể đã thay đổi.");
        if (response.Content.Headers.ContentType?.MediaType != "text/x-component" ||
            response.Content.Headers.ContentLength > MaximumResponseBytes)
            throw Unavailable();
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

    internal static MuseQuota Parse(string body)
    {
        try
        {
            // Follow the action result reference; do not mistake another RSC
            // record, a login page or a failed action for a zero quota balance.
            var records = new Dictionary<string, string>();
            foreach (string line in body.Split('\n'))
            {
                int colon = line.IndexOf(':');
                if (colon > 0) records[line[..colon]] = line[(colon + 1)..];
            }
            using var root = JsonDocument.Parse(records["0"]);
            string reference = root.RootElement.GetProperty("a").GetString()!;
            if (!reference.StartsWith("$@", StringComparison.Ordinal)) throw Unavailable();
            using var result = JsonDocument.Parse(records[reference[2..]]);
            var value = result.RootElement;
            if (!value.TryGetProperty("success", out var success) || success.ValueKind != JsonValueKind.True ||
                !value.TryGetProperty("subscription", out var subscription) || subscription.ValueKind != JsonValueKind.Object)
                throw Unavailable();
            string? plan = subscription.TryGetProperty("tier", out var tier) ? Text(tier, "name") : null;
            double? percent = null;
            DateTimeOffset? resetsAt = null;
            string? state = null;
            if (subscription.TryGetProperty("usage", out var usage) && usage.ValueKind == JsonValueKind.Object)
            {
                state = Text(usage, "state");
                if (state == "METERED" && usage.TryGetProperty("percentUsed", out var used) &&
                    used.TryGetDouble(out double number) && double.IsFinite(number) && number >= 0 && number <= 100)
                    percent = number;
                if (usage.TryGetProperty("resetsAt", out var reset) && reset.TryGetInt64(out long seconds))
                    resetsAt = DateTimeOffset.FromUnixTimeSeconds(seconds);
            }
            return new MuseQuota(plan, state, percent, resetsAt, Text(subscription, "statusSubtitle"),
                Text(subscription, "topupRowLabel"), Text(subscription, "topupRowValueLabel"), DateTimeOffset.Now);
        }
        catch (Exception error) when (error is JsonException or KeyNotFoundException or InvalidOperationException or ArgumentException or NullReferenceException or FormatException)
        {
            // Never expose raw upstream bodies, cookies or request headers.
            throw Unavailable();
        }
    }

    private static string? Text(JsonElement element, string key) =>
        element.ValueKind == JsonValueKind.Object && element.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString() is { Length: > 0 } text ? text[..Math.Min(text.Length, 400)] : null : null;
    private static InvalidOperationException Unavailable() => new("Muse chưa cung cấp quota hợp lệ. Thử lại sau hoặc cập nhật app nếu API Muse đã thay đổi.");
    public void Dispose() => http.Dispose();
}

internal sealed record MuseQuota(string? Plan, string? State, double? PercentUsed, DateTimeOffset? ResetsAt,
    string? StatusSubtitle, string? AdditionalLabel, string? AdditionalValue, DateTimeOffset CheckedAt) : AccountQuota
{
    public string Summary => PercentUsed is { } used ? $"Đã dùng {used.ToString("0.#", CultureInfo.CurrentCulture)}%"
        : State == "UNLIMITED" ? "Không giới hạn (Muse báo)" : "Muse chưa báo % sử dụng";
    public string Detail => string.Join(Environment.NewLine, new[] {
        $"{Plan ?? "Gói chưa xác định"} · {Summary}",
        ResetsAt is { } reset ? $"Reset: {reset.ToLocalTime():dd/MM/yyyy HH:mm} (giờ máy)" : StatusSubtitle,
        AdditionalValue == null ? null : $"{AdditionalLabel ?? "Mua thêm"}: {AdditionalValue}",
        $"Kiểm tra lúc {CheckedAt:dd/MM HH:mm:ss} · Bấm để cập nhật."
    }.Where(line => !string.IsNullOrWhiteSpace(line)));
}
