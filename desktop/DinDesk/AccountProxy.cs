using System.Net;

namespace MuseDesk;

internal static class AccountProxy
{
    internal static Uri? Parse(string value)
    {
        if (string.IsNullOrWhiteSpace(value)) return null;
        if (value.Length > 4096 || value.Any(char.IsControl) || !Uri.TryCreate(value, UriKind.Absolute, out var uri)
            || uri.Scheme is not ("http" or "https") || string.IsNullOrEmpty(uri.Host) || uri.Port is < 1 or > 65535
            || uri.AbsolutePath != "/" || uri.Query.Length > 0 || uri.Fragment.Length > 0 || uri.UserInfo.StartsWith(':'))
            throw new ArgumentException("Proxy: nhập http(s)://[user:password@]host:port; không kèm đường dẫn.");
        return uri;
    }

    internal static HttpClientHandler Handler(string proxyUrl)
    {
        var uri = Parse(proxyUrl);
        var handler = new HttpClientHandler { AllowAutoRedirect = false, UseCookies = false, UseProxy = uri != null };
        if (uri != null)
        {
            var address = new UriBuilder(uri) { UserName = "", Password = "" }.Uri;
            var proxy = new WebProxy(address) { BypassProxyOnLocal = false };
            if (uri.UserInfo.Length > 0)
            {
                var parts = uri.UserInfo.Split(':', 2);
                proxy.Credentials = new NetworkCredential(Uri.UnescapeDataString(parts[0]), parts.Length > 1 ? Uri.UnescapeDataString(parts[1]) : "");
            }
            handler.Proxy = proxy;
        }
        return handler;
    }

    internal static string Display(string value)
    {
        var uri = Parse(value);
        return uri == null ? "Trực tiếp" : $"{uri.Scheme}://{uri.Host}:{uri.Port}";
    }
}
