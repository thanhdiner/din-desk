using System.Net;
using System.Net.Sockets;
using System.Text;

namespace MuseDesk;

internal static class AccountProxyTests
{
    public static async Task RunAsync(string root, List<string> checks)
    {
        void Require(bool value, string error) { if (!value) throw new InvalidOperationException(error); }
        foreach (var invalid in new[] { "socks5://host:1080", "http://host/path", "http://host?key=private", "http://host/#x", "http://:pass@host", "http://host\r\n" })
        {
            bool rejected = false;
            try { AccountProxy.Parse(invalid); } catch (ArgumentException e) { rejected = !e.Message.Contains("private"); }
            Require(rejected, "Invalid proxy accepted or leaked secret");
        }
        using(var direct = AccountProxy.Handler("")) Require(!direct.UseProxy, "Direct connection inherited system proxy");
        const string url = "http://fixture-user:fixture%40private-password@127.0.0.1:12345";
        using(var handler = AccountProxy.Handler(url))
        {
            var address = new Uri("https://muse.ai/");
            var target = handler.Proxy!.GetProxy(address)!;
            var credential = handler.Proxy.Credentials!.GetCredential(target, "Basic")!;
            Require(handler.UseProxy && !handler.Proxy.IsBypassed(address) && target.Port == 12345
                && target.UserInfo == "" && credential.UserName == "fixture-user" && credential.Password == "fixture@private-password", "Proxy routing/auth decoding failed");
        }
        var settings = new Settings {Cookie="hatch_sess=proxy-fixture-cookie"}; settings.NormalizeAccounts();
        settings.Accounts[0].ProxyUrl = url; settings.NormalizeAccounts();
        var store = new SettingsStore(root); store.Save(settings);
        var loaded = store.Load();
        Require(loaded.ProxyUrl == url && loaded.Accounts[0].ProxyUrl == url, "Account proxy lost in encrypted settings");
        var ciphertext = Encoding.UTF8.GetString(File.ReadAllBytes(Path.Combine(root,"settings.protected")));
        Require(!ciphertext.Contains("private-password") && !AccountProxy.Display(url).Contains("fixture-user"), "Proxy secret exposed");
        var second = new MuseAccount {Name="Proxy Two",Cookie="hatch_sess=proxy-fixture-two",ProxyUrl="https://other.example.invalid:443"};
        loaded.Accounts.Add(second); loaded.SelectAccount(second.Id);
        Require(loaded.ProxyUrl == second.ProxyUrl && loaded.Accounts[0].ProxyUrl == url, "Account switch mixed proxy settings");
        loaded.SelectAccount(loaded.Accounts[0].Id); Require(loaded.ProxyUrl == url, "Switch back lost proxy");
        checks.Add("Per-account proxy validation, auth decoding, explicit direct mode, DPAPI storage and switch isolation");

        // Real HttpClient HTTPS CONNECT from quota checking must reach this
        // account's selected proxy; no real Muse endpoint or cookie is contacted.
        for (int i=0; i<2; i++)
        {
            var listener = new TcpListener(IPAddress.Loopback, 0); listener.Start();
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(6));
            var port = ((IPEndPoint)listener.LocalEndpoint).Port;
            var observed = Task.Run(async () => {
                using var peer = await listener.AcceptTcpClientAsync(deadline.Token);
                var stream = peer.GetStream(); var bytes = new List<byte>(); var one = new byte[1];
                while(bytes.Count < 16384 && await stream.ReadAsync(one,deadline.Token) == 1) {
                    bytes.Add(one[0]); if(bytes.Count>=4 && Encoding.ASCII.GetString(bytes.ToArray()).EndsWith("\r\n\r\n")) break;
                }
                await stream.WriteAsync("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"u8.ToArray(),deadline.Token);
                return Encoding.ASCII.GetString(bytes.ToArray());
            });
            try {
                using var quota = new MuseQuotaClient();
                bool failed = false;
                try { await quota.FetchAccountAsync(new MuseAccount {Cookie=$"hatch_sess=isolated-cookie-{i}",ProxyUrl=$"http://127.0.0.1:{port}"},deadline.Token); }
                catch(HttpRequestException) {failed=true;}
                var connect = await observed;
                Require(failed && connect.StartsWith("CONNECT muse.ai:443 ") && !connect.Contains("isolated-cookie"), "Quota ignored account proxy or exposed cookie before TLS");
            } finally {deadline.Cancel();listener.Stop();}
        }
        checks.Add("Quota real HTTP CONNECT follows each selected account proxy and never sends cookies before TLS or falls back direct");

        // The actual bundled Rust child must receive this account's proxy env.
        var proxyListener = new TcpListener(IPAddress.Loopback,0); proxyListener.Start();
        using var deadlineServer = new CancellationTokenSource(TimeSpan.FromSeconds(8));
        var connection = Task.Run(async () => {
            using var peer = await proxyListener.AcceptTcpClientAsync(deadlineServer.Token);
            var stream = peer.GetStream(); var data = new List<byte>(); var one = new byte[1];
            while(data.Count<16384 && await stream.ReadAsync(one,deadlineServer.Token)==1) {
                data.Add(one[0]); if(data.Count>=4 && Encoding.ASCII.GetString(data.ToArray()).EndsWith("\r\n\r\n"))break;
            }
            await stream.WriteAsync("HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"u8.ToArray(),deadlineServer.Token);
            return Encoding.ASCII.GetString(data.ToArray());
        });
        var portProbe = new TcpListener(IPAddress.Loopback,0);portProbe.Start();
        int proxyApiPort=((IPEndPoint)portProbe.LocalEndpoint).Port;portProbe.Stop();
        settings.Port=proxyApiPort;
        settings.Accounts[0].ProxyUrl=$"http://proxy-user:private-test-password@127.0.0.1:{((IPEndPoint)proxyListener.LocalEndpoint).Port}";
        settings.NormalizeAccounts();
        using var server = new ServerController(root);
        try {
            await server.StartAsync(settings);
            bool rejected=false;
            try {await server.CheckMuseAsync();}catch(InvalidOperationException){rejected=true;}
            var request=await connection;
            Require(rejected && request.StartsWith("CONNECT muse.ai:443 ") && !request.Contains("hatch_sess"),"Embedded server ignored account proxy");
            Require(!server.Sanitize($"proxy={settings.ProxyUrl}").Contains("private-test-password"),"Diagnostic leaked proxy password");
        } finally {server.Stop();deadlineServer.Cancel();proxyListener.Stop();}
        checks.Add("Bundled Rust child receives account proxy, fails closed on 407 and diagnostics redact proxy credentials");
    }
}
