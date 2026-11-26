using System.Net;
using System.Net.Http.Headers;
using System.Net.Sockets;

namespace MuseDesk;

internal static class ProxyKeyTests
{
    internal static async Task RunAsync(string root, List<string> checks)
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        int port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();
        var settings = new Settings { Cookie = "hatch_sess=proxy-expiry-fixture", Port = port };
        var store = new SettingsStore(root);
        DateTimeOffset deadline = DateTimeOffset.FromUnixTimeSeconds(DateTimeOffset.UtcNow.ToUnixTimeSeconds() + 7);
        settings.ApiKeyExpiresAt = deadline;
        store.Save(settings);
        var loaded = store.Load();
        Require(loaded.ApiKey == settings.ApiKey && loaded.ApiKeyExpiresAt == deadline, "Proxy expiry or initial key changed after saving");

        using var server = new ServerController(root);
        await server.StartAsync(loaded);
        using var http = new HttpClient(new HttpClientHandler { UseProxy = false }) { Timeout = TimeSpan.FromSeconds(5) };
        string url = $"http://127.0.0.1:{port}";
        using (var health = await http.GetAsync(url + "/health"))
            Require(health.IsSuccessStatusCode, "Health route unexpectedly requires a key");
        foreach (string path in new[] { "/v1/models", "/v1/conversations", "/v1/chat/completions", "/v1/responses" })
        {
            var method = path.EndsWith("completions", StringComparison.Ordinal) || path.EndsWith("responses", StringComparison.Ordinal) ? HttpMethod.Post : HttpMethod.Get;
            using var request = new HttpRequestMessage(method, url + path);
            using var denied = await http.SendAsync(request);
            Require(denied.StatusCode == HttpStatusCode.Unauthorized, "Desktop proxy exposed an unauthenticated API route");
        }
        using (var accepted = await ModelsAsync(http, url, loaded.ApiKey))
            Require(accepted.IsSuccessStatusCode, "Initial proxy key was rejected before expiration");
        TimeSpan remaining = deadline - DateTimeOffset.UtcNow;
        if (remaining > TimeSpan.Zero) await Task.Delay(remaining + TimeSpan.FromMilliseconds(80));
        using (var expired = await ModelsAsync(http, url, loaded.ApiKey))
            Require(expired.StatusCode == HttpStatusCode.Unauthorized && (await expired.Content.ReadAsStringAsync()).Contains("proxy_key_expired"), "Running proxy accepted an expired key");
        server.Stop();

        var rotated = loaded.Copy();
        rotated.RotateApiKey(DateTimeOffset.UtcNow);
        Require(rotated.ApiKey != loaded.ApiKey && rotated.ApiKeyExpiresAt == null, "Key rotation retained expired credentials");
        await ProxyPreferences.ApplyAsync(server, loaded, rotated, store.Save);
        await server.StartAsync(store.Load());
        using (var denied = await ModelsAsync(http, url, loaded.ApiKey))
            Require(denied.StatusCode == HttpStatusCode.Unauthorized, "Proxy accepted pre-rotation key");
        using (var accepted = await ModelsAsync(http, url, rotated.ApiKey))
            Require(accepted.IsSuccessStatusCode, "Proxy rejected the persisted replacement key");
        checks.Add("Desktop routes require a key, active expiration returns 401 and manual rotation revokes the previous key");

        var fake = new RestartFixture(rotated);
        var replacement = rotated.Copy();
        replacement.RotateApiKey(DateTimeOffset.UtcNow);
        await ExpectFailure(() => ProxyPreferences.ApplyAsync(fake, rotated, replacement, _ => throw new IOException()));
        Require(fake.Running && fake.Active == rotated, "Failed write did not restore the previous live credentials");
        fake.FailStart = true;
        Settings? saved = null;
        await ExpectFailure(() => ProxyPreferences.ApplyAsync(fake, rotated, replacement, value => saved = value));
        Require(!fake.Running && saved == replacement, "Restart failure did not retain the persisted replacement key");
        checks.Add("Key changes recover a running proxy after write failure and preserve the replacement after restart failure");
    }

    private static async Task<HttpResponseMessage> ModelsAsync(HttpClient http, string url, string key)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, url + "/v1/models");
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
        return await http.SendAsync(request);
    }

    private static void Require(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    private static async Task ExpectFailure(Func<Task> action)
    {
        try { await action(); }
        catch (InvalidOperationException) { return; }
        throw new InvalidOperationException("Proxy update unexpectedly succeeded");
    }

    private sealed class RestartFixture(Settings initial) : IAccountProxy
    {
        public bool Running { get; private set; } = true;
        public Settings Active { get; private set; } = initial;
        public bool FailStart { get; set; }
        public Task StartAsync(Settings settings)
        {
            if (FailStart) throw new InvalidOperationException();
            Active = settings;
            Running = true;
            return Task.CompletedTask;
        }
        public Task<int> CheckMuseAsync() => Task.FromResult(0);
        public void Stop() => Running = false;
    }
}
