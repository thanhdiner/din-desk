using System.Net;
using System.Text;

namespace MuseDesk;

internal static class QuotaTests
{
    private sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) => send(request, token);
    }

    public static async Task RunAsync(string root, List<string> checks)
    {
        void Require(bool value, string error) { if (!value) throw new InvalidOperationException(error); }
        const string result = "0:{\"a\":\"$@a\"}\na:{\"success\":true,\"subscription\":{\"tier\":{\"name\":\"Muse Free\"},\"usage\":{\"state\":\"METERED\",\"percentUsed\":50,\"resetsAt\":1795040808},\"topupBalance\":120000000,\"topupRowLabel\":\"Additional tokens\",\"topupRowValueLabel\":\"0% used (3B tokens left)\"}}\n";
        var quota = MuseQuotaClient.Parse(result);
        Require(quota.PercentUsed == 50 && quota.ResetsAt == DateTimeOffset.FromUnixTimeSeconds(1795040808)
            && quota.Detail.Contains("3B tokens left") && !quota.Detail.Contains("120000000"), "Quota display changed upstream units or lost reset/usage");
        foreach (string invalid in new[] { "<html>login</html>", "0:{\"a\":\"$@1\"}\n1:{\"success\":false,\"error\":\"private-response\"}",
            "0:{\"a\":\"$@2\"}\n1:{\"success\":true,\"subscription\":{}}", "0:{}" }) {
            bool rejected = false;
            try { MuseQuotaClient.Parse(invalid); } catch (InvalidOperationException e) { rejected = !e.Message.Contains("private-response"); }
            Require(rejected, "Malformed/failed quota response was accepted or leaked raw response");
        }
        var missing = MuseQuotaClient.Parse("0:{\"a\":\"$@1\"}\n1:{\"success\":true,\"subscription\":{\"tier\":{\"name\":\"Unknown\"}}}");
        Require(missing.PercentUsed == null && !missing.Summary.Contains("0%"), "Missing quota was displayed as unused quota");
        checks.Add("Quota RSC result reference, reset timestamp, upstream top-up labels and unavailable-data handling");

        int calls = 0;
        var cookies = new List<string>();
        using var client = new MuseQuotaClient(new Handler(async (request, token) => {
            calls++;
            cookies.Add(request.Headers.GetValues("Cookie").Single());
            Require(request.Method == HttpMethod.Post && request.RequestUri?.AbsoluteUri == "https://muse.ai/"
                && request.Headers.GetValues("Next-Action").Single() == "40e0249eeae158685b6a8d0c3b203f53dd4f2c7d63"
                && request.Headers.GetValues("Sec-Fetch-Site").Single() == "same-origin"
                && await request.Content!.ReadAsStringAsync(token) == "[{\"includeAgreement\":true}]", "Quota check used a non-read-only action or wrong request");
            return new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(result, Encoding.UTF8, "text/x-component") };
        }));
        Require(calls == 0, "Constructing quota client fetched automatically");
        var settings = new Settings { Cookie = "hatch_sess=first-private-cookie" }; settings.NormalizeAccounts();
        settings.Accounts.Add(new MuseAccount { Name = "Second", Cookie = "hatch_sess=second-private-cookie" });
        string before = System.Text.Json.JsonSerializer.Serialize(settings);
        await client.FetchAsync(settings.Accounts[0].Cookie);
        await client.FetchAsync(settings.Accounts[1].Cookie);
        Require(calls == 2 && cookies.SequenceEqual(settings.Accounts.Select(a => a.Cookie))
            && before == System.Text.Json.JsonSerializer.Serialize(settings), "Quota check mixed account credentials, auto-retried, or changed active account");
        foreach (var status in new[] { HttpStatusCode.Forbidden, HttpStatusCode.TooManyRequests, HttpStatusCode.InternalServerError, HttpStatusCode.Redirect }) {
            using var failed = new MuseQuotaClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(status) { Content = new StringContent("private-response") })));
            bool rejected = false;
            try { await failed.FetchAsync(settings.Cookie); } catch (InvalidOperationException e) { rejected = !e.Message.Contains("private-response"); }
            Require(rejected, "Quota HTTP failure was accepted or exposed response");
        }
        using (var large = new MuseQuotaClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) {
            Content = new StringContent(new string('x', 262145), Encoding.UTF8, "text/x-component") })))) {
            bool rejected = false;
            try { await large.FetchAsync(settings.Cookie); } catch (InvalidOperationException) { rejected = true; }
            Require(rejected, "Unbounded quota response accepted");
        }
        using (var pending = new MuseQuotaClient(new Handler(async (_, token) => { await Task.Delay(Timeout.Infinite, token); return new HttpResponseMessage(); }))) {
            using var cancel = new CancellationTokenSource(TimeSpan.FromMilliseconds(15));
            bool cancelled = false;
            try { await pending.FetchAsync(settings.Cookie, cancel.Token); } catch (OperationCanceledException) { cancelled = true; }
            Require(cancelled, "Cancelled quota check continued");
        }
        checks.Add("Quota is click-only, selected-account credentials isolated, no account switch, no retries, bounded response and cancellation");

        Exception? disposeError = null;
        var ui = new Thread(() => {
            try {
                using var form = new MainForm(Path.Combine(root, "quota-ui"));
                form.Dispose();
                form.Dispose(); // Close + Program's using scope must both be safe.
            } catch (Exception e) { disposeError = e; }
        });
        ui.SetApartmentState(ApartmentState.STA); ui.Start();
        if (!ui.Join(TimeSpan.FromSeconds(10)) || disposeError != null)
            throw new InvalidOperationException("Quota UI cleanup/double disposal failed");
        checks.Add("Account quota UI constructs offline and safely disposes twice without lingering timers or tray icon");
    }
}
