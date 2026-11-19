using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal static class GeminiQuotaTests
{
    internal const string Response = """
        )]}'
        [{"data":{"viewer":{"rpcs":{"getUsageInfo":{"subscriptionTier":"TIER1","pools":[
          {"poolType":"GXU_FIVE_HOURLY","percentageUsed":0.71,"gxu":{"windowRefreshesAt":"2026-11-13T05:43:53.558200Z"}},
          {"poolType":"GXU_WEEKLY","percentageUsed":0.04,"gxu":{"windowRefreshesAt":"2026-11-19T04:43:53.558273Z"}},
          {"poolType":"OVERAGE_CREDITS","creditsRemaining":100}
        ]}}}},"errors":[{"message":"unrelated upsell error"}]}]
        """;

    private sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) => send(request, token);
    }
    private static HttpResponseMessage Ok(string body = Response) => new(HttpStatusCode.OK) { Content = new StringContent(body, Encoding.UTF8, "application/json") };

    public static async Task RunAsync(string root, List<string> checks)
    {
        void Require(bool value, string message) { if (!value) throw new InvalidOperationException(message); }
        var quota = GeminiQuotaClient.Parse(Response);
        Require(quota.FiveHours?.PercentUsed == 71 && quota.Weekly?.PercentUsed == 4 && quota.Plan == "Google AI Pro"
            && quota.FiveHours.ResetsAt == DateTimeOffset.Parse("2026-11-13T05:43:53.558200Z")
            && quota.Detail.Contains("Reset") && !quota.Detail.Contains("creditsRemaining"), "Gemini units, windows, plan or reset changed");
        var partial = GeminiQuotaClient.Parse("""[{"data":{"viewer":{"rpcs":{"getUsageInfo":{"subscriptionTier":"NEW_TIER","pools":[{"poolType":"GXU_WEEKLY","percentageUsed":0}]}}}}}]""");
        Require(partial.FiveHours == null && partial.Weekly?.PercentUsed == 0 && partial.Plan == null
            && partial.Detail.Contains("5 giờ: Google chưa cung cấp dữ liệu"), "Missing Gemini pool became zero/unlimited or unknown tier was guessed");
        var exhausted = GeminiQuotaClient.Parse(Response.Replace("0.71", "1"));
        Require(exhausted.FiveHours?.PercentUsed == 100, "Gemini 100% was mistaken for 1%");
        foreach (string malformed in new[] {
            "<html>private-login-response</html>", "[{\"data\":{\"viewer\":null}}]", "[{\"errors\":[{\"message\":\"private-response\"}],\"data\":null}]",
            Response.Replace("0.71", "null"), Response.Replace("0.71", "-1"), Response.Replace("0.71", "\"0.71\""),
            Response.Replace("GXU_FIVE_HOURLY", "GXU_WEEKLY"), "[{\"data\":{\"viewer\":{\"rpcs\":{\"getUsageInfo\":{\"pools\":[]}}}}}]", "[not-json]"
        })
        {
            bool failed = false;
            try { GeminiQuotaClient.Parse(malformed); }
            catch (InvalidOperationException e) { failed = !e.Message.Contains("private-"); }
            Require(failed, "Malformed/signed-out Gemini quota was accepted or leaked upstream data");
        }
        checks.Add("Gemini Web 5-hour/weekly fraction units, resets, partial pools, unknown tiers and strict redacted error parsing");

        int calls = 0;
        var proxyUrls = new List<string>();
        var seenCookies = new List<string>();
        var dispatcher = new AccountQuotaClient(proxy => {
            proxyUrls.Add(proxy);
            return new Handler(async (request, token) => {
                calls++;
                var uri = request.RequestUri!;
                string cookie = request.Headers.GetValues("Cookie").Single(); seenCookies.Add(cookie);
                Require(!request.Headers.Contains("Authorization") && !request.Headers.Contains("x-api-key"), "Local API key was sent upstream");
                if (uri.Host == "gemini.google.com")
                {
                    int googleIndex=cookie.Contains("second-private")?5:0;
                    Require(request.Method == HttpMethod.Get && request.Content == null
                        && uri.AbsolutePath == (googleIndex==0?"":"/u/5")+"/_/BardChatUi/graphql/schemas/GEMINI_WEB_GRAPHQL/executeQuery"
                        && request.Headers.Referrer?.AbsoluteUri == (googleIndex==0?"https://gemini.google.com/usage":"https://gemini.google.com/u/5/usage")
                        && !cookie.Contains("hatch_sess") && !uri.Query.Contains("private"), "Gemini quota generated chat or mixed credential destinations");
                    var query = uri.Query.TrimStart('?').Split('&').Select(part => part.Split('=', 2))
                        .ToDictionary(part => part[0], part => Uri.UnescapeDataString(part[1]));
                    Require(query["query_signature"] == "2/dWbCvkVB0X16r9NPnxifeOAAqdCqMf0xeWvau9yPy7g=", "Persisted signature lost percent encoding");
                    Require(!query.ContainsKey("authuser") && request.Headers.GetValues("X-Goog-AuthUser").Single()==googleIndex.ToString(),"Gemini quota account path/header mismatch or redirect-inducing authuser query");
                    using var vars = JsonDocument.Parse(query["variables"]);
                    Require(vars.RootElement.GetProperty("g1Request").GetProperty("entrypoints")[0].GetString() == "GEMINI_USAGE_SETTINGS_UPGRADE_BANNER", "Wrong Gemini usage query variables");
                    return Ok();
                }
                Require(uri.Host == "muse.ai" && request.Method == HttpMethod.Post && cookie.StartsWith("hatch_sess=")
                    && await request.Content!.ReadAsStringAsync(token) == "[{\"includeAgreement\":true}]", "Muse quota dispatch regressed");
                return new(HttpStatusCode.OK) { Content = new StringContent("0:{\"a\":\"$@1\"}\n1:{\"success\":true,\"subscription\":{}}", Encoding.UTF8, "text/x-component") };
            });
        });
        var settings = new Settings { Cookie = "hatch_sess=muse-private" }; settings.NormalizeAccounts();
        settings.Accounts.Add(new MuseAccount { Name = "Gemini one", Provider = "gemini", Cookie = "__Secure-1PSID=first-private", ProxyUrl = "http://127.0.0.1:8091" });
        settings.Accounts.Add(new MuseAccount { Name = "Gemini two", Provider = "gemini", Cookie = "__Secure-1PSID=second-private", ProxyUrl = "http://127.0.0.1:8092",GeminiAccountIndex=5 });
        string before = JsonSerializer.Serialize(settings);
        Require(calls == 0 && proxyUrls.Count == 0, "Gemini quota fetched on construction");
        foreach (var account in settings.Accounts) await dispatcher.FetchAccountAsync(account);
        Require(calls == 3 && before == JsonSerializer.Serialize(settings)
            && seenCookies.SequenceEqual(settings.Accounts.Select(a => a.Cookie))
            && proxyUrls.SequenceEqual(settings.Accounts.Select(a => a.ProxyUrl)), "Quota mixed accounts/proxies, switched active account, mutated secrets or retried");
        checks.Add("Gemini/Muse quota click-only dispatch, exact read-only request, per-account cookie/proxy isolation and unchanged active session");

        foreach (var status in new[] { HttpStatusCode.Unauthorized, HttpStatusCode.Forbidden, HttpStatusCode.Redirect, HttpStatusCode.TooManyRequests, HttpStatusCode.InternalServerError })
        {
            int failedCalls = 0;
            using var client = new GeminiQuotaClient(new Handler((_, _) => { failedCalls++; return Task.FromResult(new HttpResponseMessage(status) { Content = new StringContent("private-response") }); }));
            bool failed = false;
            try { await client.FetchAsync("__Secure-1PSID=private-cookie"); }
            catch (InvalidOperationException e) { failed = !e.Message.Contains("private-"); }
            Require(failed && failedCalls == 1, "Gemini quota failure retried or leaked response");
        }
        foreach (bool streaming in new[] { false, true })
        {
            using var client = new GeminiQuotaClient(new Handler((_, _) => {
                HttpContent content = new StringContent(new string('x', 262145));
                if (streaming) content = new StreamContent(new UnseekableLargeStream());
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = content });
            }));
            bool failed = false;
            try { await client.FetchAsync("__Secure-1PSID=private-cookie"); } catch (InvalidOperationException) { failed = true; }
            Require(failed, "Unbounded Gemini quota body accepted");
        }
        using (var client = new GeminiQuotaClient(new Handler(async (_, token) => { await Task.Delay(Timeout.Infinite, token); return Ok(); })))
        {
            using var cancel = new CancellationTokenSource(20);
            bool cancelled = false;
            try { await client.FetchAsync("__Secure-1PSID=private-cookie", cancel.Token); } catch (OperationCanceledException) { cancelled = true; }
            Require(cancelled, "Gemini quota ignored cancellation");
        }
        checks.Add("Gemini quota no redirects/retries, redacted auth/rate/HTTP failures, bounded/chunked response and cancellation");

        var listener = new TcpListener(IPAddress.Loopback, 0); listener.Start();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(6));
        var observed = Task.Run(async () => {
            using var peer = await listener.AcceptTcpClientAsync(deadline.Token);
            var stream = peer.GetStream(); var bytes = new List<byte>(); var one = new byte[1];
            while (bytes.Count < 16384 && await stream.ReadAsync(one, deadline.Token) == 1) {
                bytes.Add(one[0]); if (bytes.Count >= 4 && Encoding.ASCII.GetString(bytes.ToArray()).EndsWith("\r\n\r\n")) break;
            }
            await stream.WriteAsync("HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"u8.ToArray(), deadline.Token);
            return Encoding.ASCII.GetString(bytes.ToArray());
        });
        try
        {
            bool failed = false;
            try { await new AccountQuotaClient().FetchAccountAsync(new MuseAccount { Provider = "gemini", Cookie = "__Secure-1PSID=private-connect-cookie", ProxyUrl = $"http://127.0.0.1:{((IPEndPoint)listener.LocalEndpoint).Port}" }, deadline.Token); }
            catch (HttpRequestException) { failed = true; }
            string connect = await observed;
            Require(failed && connect.StartsWith("CONNECT gemini.google.com:443 ") && !connect.Contains("private-connect-cookie"), "Gemini quota bypassed proxy or exposed cookie before TLS");
        }
        finally { deadline.Cancel(); listener.Stop(); }
        checks.Add("Gemini quota real HTTP CONNECT uses the selected proxy and stops on 407 without leaking Google cookies or falling back direct");
        await CheckUiAsync(root, checks);
    }

    private static Task CheckUiAsync(string root, List<string> checks)
    {
        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() => {
            try
            {
                string directory = Path.Combine(root, "gemini-quota-ui");
                var settings = new Settings { Cookie = "hatch_sess=ui-muse-fixture" }; settings.NormalizeAccounts();
                settings.Accounts.Add(new MuseAccount { Name = "Gemini UI fixture", Provider = "gemini", Cookie = "__Secure-1PSID=ui-gemini-fixture" });
                var store = new SettingsStore(directory); store.Save(settings);
                int calls = 0;
                var reply = new TaskCompletionSource<HttpResponseMessage>(TaskCreationOptions.RunContinuationsAsynchronously);
                var client = new AccountQuotaClient(_ => new Handler(async (request, token) => {
                    calls++;
                    if (request.RequestUri?.Host != "gemini.google.com") throw new InvalidOperationException("Gemini UI called Muse quota");
                    return await reply.Task.WaitAsync(token);
                }));
                using var form = new MainForm(directory, client);
                form.Shown += async (_, _) => {
                    try
                    {
                        var grid = form.Controls.Find("Accounts", true).OfType<DataGridView>().Single();
                        var detail = form.Controls.Find("QuotaDetail", true).OfType<TextBox>().Single();
                        if (calls != 0 || grid.Rows[1].Cells["CheckQuota"].Value?.ToString() != "Kiểm tra quota")
                            throw new InvalidOperationException("Gemini quota button missing or fetched on app open");
                        grid.CurrentCell = grid.Rows[1].Cells[0];
                        if (calls != 0 || !detail.Text.StartsWith("Gemini UI fixture"))
                            throw new InvalidOperationException("Selecting account fetched quota or left previous account details");
                        var click = typeof(DataGridView).GetMethod("OnCellContentClick", System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!;
                        var args = new DataGridViewCellEventArgs(grid.Columns["CheckQuota"].Index, 1);
                        click.Invoke(grid, new object[] { args });
                        click.Invoke(grid, new object[] { args });
                        if (calls != 1 || !detail.Text.Contains("Đang lấy quota từ Gemini")) throw new InvalidOperationException("Gemini click did not start or double click duplicated request");
                        reply.SetResult(Ok());
                        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
                        while (!detail.Text.Contains("5 giờ: 71%")) await Task.Delay(10, deadline.Token);
                        var saved = store.Load();
                        if (saved.ActiveAccountId != settings.ActiveAccountId || saved.Provider != "muse" || calls != 1
                            || !detail.Text.Contains("Hàng tuần: 4%") || grid.Rows[0].Cells["Quota"].Value?.ToString() != "Chưa kiểm tra")
                            throw new InvalidOperationException("Gemini quota UI mixed results or switched active account");
                        checks.Add("Gemini account row button handles click/double click, pending and 5-hour/weekly result without fetching on selection or switching active Muse");
                        completed.TrySetResult();
                    }
                    catch (Exception error) { completed.TrySetException(error); }
                    finally { form.Dispose(); }
                };
                Application.Run(form);
                if (File.Exists(Path.Combine(directory, "settings.protected"))) File.Delete(Path.Combine(directory, "settings.protected"));
                if (Directory.Exists(directory) && !Directory.EnumerateFileSystemEntries(directory).Any()) Directory.Delete(directory);
            }
            catch (Exception error) { completed.TrySetException(error); }
        });
        thread.SetApartmentState(ApartmentState.STA); thread.IsBackground = true; thread.Start();
        return completed.Task.WaitAsync(TimeSpan.FromSeconds(12));
    }

    private sealed class UnseekableLargeStream : Stream
    {
        private int remaining = 262145;
        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override int Read(byte[] buffer, int offset, int count) { int size = Math.Min(count, remaining); Array.Fill(buffer, (byte)'x', offset, size); remaining -= size; return size; }
        public override void Flush() { }
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    }
}
