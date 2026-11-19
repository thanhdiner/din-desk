using System.Diagnostics;
using System.Net;
using System.Net.Http.Headers;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace MuseDesk;

internal sealed class ServerController(string dataDirectory) : IDisposable, IAccountProxy
{
    private Process? process;
    private ProcessJob? job;
    private Settings? active;
    private readonly object logLock = new();
    private readonly Queue<string> recent = new();
    private readonly HttpClient http = new(new HttpClientHandler { UseProxy = false }) { Timeout = TimeSpan.FromSeconds(45) };
    public bool Running => process is { HasExited: false };
    public string RuntimeDirectory { get; private set; } = "";
    public event Action? StateChanged;

    public async Task StartAsync(Settings settings)
    {
        SettingsStore.Validate(settings, requireCookie: true);
        if (Running) throw new InvalidOperationException("Proxy đang chạy. Dừng proxy trước khi thay cấu hình.");
        Stop();
        // Do not adopt or stop an unrelated listener, including the developer's server.
        try
        {
            var probe = new TcpListener(IPAddress.Loopback, settings.Port);
            try { probe.Start(); } finally { probe.Stop(); }
        }
        catch (SocketException) { throw new InvalidOperationException($"Cổng {settings.Port} đang được dùng. Chọn cổng khác trong Cài đặt nâng cao."); }

        Directory.CreateDirectory(dataDirectory);
        RuntimeDirectory = RuntimeFiles.Extract(dataDirectory);
        active = settings;
        var info = new ProcessStartInfo(Path.Combine(RuntimeDirectory, "muse-api.exe"))
        {
            UseShellExecute = false, CreateNoWindow = true, WorkingDirectory = dataDirectory,
            RedirectStandardOutput = true, RedirectStandardError = true
        };
        // Explicit settings for the owned child. Never put credentials on its command line.
        info.Environment["MUSE_BACKEND"] = settings.Provider;
        info.Environment["MUSE_COOKIE"] = settings.Provider == "muse" ? settings.Cookie : "";
        info.Environment["GEMINI_COOKIE"] = settings.Provider == "gemini" ? settings.Cookie : "";
        info.Environment["GEMINI_ACCOUNT_INDEX"] = settings.Provider == "gemini" ? settings.GeminiAccountIndex.ToString(System.Globalization.CultureInfo.InvariantCulture) : "0";
        info.Environment["GEMINI_MODEL_ID"] = settings.Provider == "gemini" ? settings.GeminiModelId : "";
        info.Environment["GEMINI_THINKING_ID"] = settings.Provider == "gemini" ? settings.GeminiThinkingId : "";
        info.Environment["MUSE_NODE_ID"] = settings.Provider == "muse" ? settings.NodeId : "";
        info.Environment["MUSE_UPSTREAM_PROXY"] = settings.ProxyUrl;
        info.Environment["MUSE_PROXY_API_KEY"] = settings.ApiKey;
        info.Environment["DECISION_API_KEY"] = DecisionKeys.Load(dataDirectory);
        info.Environment["BIND_ADDR"] = $"127.0.0.1:{settings.Port}";
        info.Environment["MUSE_CONVERSATIONS_PATH"] = ConversationPath(dataDirectory, settings);
        info.Environment["RUST_LOG"] = "muse_api=info";
        info.Environment["RUST_BACKTRACE"] = "0";
        info.Environment["NO_COLOR"] = "1";
        job = new ProcessJob();
        process = new Process { StartInfo = info, EnableRaisingEvents = true };
        process.OutputDataReceived += (_, e) => { if (e.Data != null) AddLog(e.Data); };
        process.ErrorDataReceived += (_, e) => { if (e.Data != null) AddLog(e.Data); };
        process.Exited += (_, _) => StateChanged?.Invoke();
        try
        {
            process.Start();
            job.Assign(process);
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();
            for (int attempt = 0; attempt < 75; attempt++)
            {
                if (!Running) throw new InvalidOperationException("Proxy không khởi động được. Cổng có thể vừa bị ứng dụng khác sử dụng.");
                try
                {
                    using var body = new StringContent(JsonSerializer.Serialize(new {model=settings.ModelId,messages=new[]{new{role="user",content="ready"}}}), Encoding.UTF8, "application/json");
                    using var request = AuthRequest(HttpMethod.Post, "/v1/messages/count_tokens");
                    request.Content = body;
                    using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(1));
                    using var reply = await http.SendAsync(request, cts.Token);
                    if (reply.IsSuccessStatusCode && Running)
                    {
                        AddLog("Proxy đã sẵn sàng trên localhost.");
                        StateChanged?.Invoke();
                        return;
                    }
                }
                catch (HttpRequestException) { }
                catch (OperationCanceledException) { }
                await Task.Delay(150);
            }
            throw new InvalidOperationException("Proxy khởi động quá lâu. Thử chọn một cổng khác.");
        }
        catch { Stop(); throw; }
    }

    public async Task<int> CheckMuseAsync()
    {
        if (!Running) throw new InvalidOperationException("Bật proxy trước khi kiểm tra Muse.");
        bool gemini=active!.Provider=="gemini";
        using var request = AuthRequest(HttpMethod.Get, gemini ? "/v1/provider/check" : "/v1/conversations");
        using var response = await http.SendAsync(request);
        if (!response.IsSuccessStatusCode)
        {
            throw new InvalidOperationException(gemini ? "Chưa xác thực được Gemini. Kiểm tra proxy và cập nhật __Secure-1PSID/__Secure-1PSIDTS từ gemini.google.com." : "Chưa kết nối được tài khoản Muse. Cookie có thể đã hết hạn; mở Muse, lấy cookie mới rồi Lưu & kết nối lại.");
        }
        using var document = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
        int count = gemini ? 0 : document.RootElement.GetProperty("data").GetArrayLength();
        if(gemini && (!document.RootElement.GetProperty("authenticated").GetBoolean() || document.RootElement.GetProperty("provider").GetString()!="gemini")) throw new InvalidOperationException("Phản hồi xác thực không khớp Gemini.");
        if(gemini && (!document.RootElement.TryGetProperty("account_index",out var index) || index.GetInt32()!=active.GeminiAccountIndex))
            throw new InvalidOperationException("Phản hồi Gemini không khớp số tài khoản Google đã chọn.");
        AddLog(gemini ? "Đã xác thực Gemini Web." : "Đã xác thực tài khoản và kết nối VM Muse.");
        return count;
    }

    internal static string ConversationPath(string directory, Settings settings)
    {
        if (settings.ActiveAccountId == null) return Path.Combine(directory,"conversations.json");
        if (!Guid.TryParseExact(settings.ActiveAccountId,"N",out _)) throw new ArgumentException("ID tài khoản không hợp lệ.");
        return Path.Combine(directory,(settings.Provider=="gemini"?$"gemini-conversations-u{settings.GeminiAccountIndex}-":"conversations-")+settings.ActiveAccountId+".json");
    }

    private HttpRequestMessage AuthRequest(HttpMethod method, string path)
    {
        var request = new HttpRequestMessage(method, $"http://127.0.0.1:{active!.Port}{path}");
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", active.ApiKey);
        return request;
    }

    public void Stop()
    {
        if (process != null)
        {
            try { if (!process.HasExited) { process.Kill(entireProcessTree: true); process.WaitForExit(3000); } }
            catch (InvalidOperationException) { }
            finally { process.Dispose(); process = null; }
        }
        job?.Dispose(); job = null;
        StateChanged?.Invoke();
    }

    internal string Sanitize(string value)
    {
        value = Regex.Replace(value, @"(?i)https?://[^\s""<>]*@[^\s""<>]*", "[proxy đã ẩn]");
        value = Regex.Replace(value, "\\x1B\\[[0-9;]*[a-zA-Z]", "");
        if (active != null)
        {
            foreach (string secret in active.Cookie.Split(';').Select(p => p.Trim()).Append(active.Cookie).Append(active.ApiKey).Append(active.ProxyUrl).Where(s => s.Length > 3))
            {
                value = value.Replace(secret, "[đã ẩn]", StringComparison.Ordinal);
                int equals = secret.IndexOf('=');
                // Short preference cookie values such as 0/1 are not secrets by
                // themselves; replacing them globally destroys HTTP codes and timestamps.
                if (equals >= 0 && secret.Length - equals - 1 >= 4) value = value.Replace(secret[(equals + 1)..], "[đã ẩn]", StringComparison.Ordinal);
            }
        }
        value = Regex.Replace(value, @"(?i)(auth_token|access_token|hatch_sess|__Secure-1PSIDTS|__Secure-1PSID|SAPISID|SNlM0e|cookie|authorization|api_key)(\s*[=:]\s*)[^\s,;]+", "$1=$2[đã ẩn]");
        return value.Length > 1800 ? value[..1800] : value;
    }

    private void AddLog(string value)
    {
        lock (logLock)
        {
            recent.Enqueue(Sanitize(value));
            while (recent.Count > 120) recent.Dequeue();
        }
    }

    public string Diagnostics()
    {
        lock (logLock) return $"Din Desk {Program.Version}\r\nWindows: {Environment.OSVersion.Version}\r\nProxy: {(Running ? "đang chạy" : "đã dừng")}\r\n\r\n" + string.Join("\r\n", recent);
    }

    public void Dispose() { Stop(); http.Dispose(); }
}
