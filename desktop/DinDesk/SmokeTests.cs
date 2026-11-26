using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal static class SmokeTests
{
    // Runs only on explicit --self-test. Uses isolated data and never saves a
    // credential into the release directory. Optional live test is opt-in.
    public static int Run(string[] args)
    {
        string root = Path.Combine(Path.GetTempPath(), "DinDesk-test-" + Guid.NewGuid().ToString("N"));
        string? report = args.SkipWhile(a => a != "--report").Skip(1).FirstOrDefault();
        var checks = new List<string>();
        string? failure = null;
        try { RunAsync(root, checks, args.Contains("--live"), args.Contains("--with-claude")).GetAwaiter().GetResult(); }
        catch (Exception error) { failure = error is ArgumentException or InvalidOperationException ? error.Message : error.GetType().Name; }
        var result = new { version = Program.Version, success = failure == null, checks, failure };
        if (report != null) File.WriteAllText(report, JsonSerializer.Serialize(result, new JsonSerializerOptions { WriteIndented = true }));
        // Exact test-owned files only, no recursive computed deletion.
        foreach (string path in new[] { Path.Combine(root, "settings.protected"), Path.Combine(root, "claude.ps1"), Path.Combine(root, "gemini-launcher.json"), Path.Combine(root, "launcher-capture.json"), Path.Combine(root, "runtime", Program.Version, "muse-api.exe") }
            .Concat(RuntimeFiles.CliFiles.SelectMany(name => new[] { Path.Combine(root, "runtime", Program.Version, name), Path.Combine(root, "cli", name) })))
            if (File.Exists(path)) File.Delete(path);
        foreach (string path in new[] { Path.Combine(root, "runtime", Program.Version), Path.Combine(root, "runtime"), Path.Combine(root, "cli"), root })
            if (Directory.Exists(path) && !Directory.EnumerateFileSystemEntries(path).Any()) Directory.Delete(path);
        return failure == null ? 0 : 1;
    }

    private static async Task RunAsync(string root, List<string> checks, bool live, bool withClaude)
    {
        Directory.CreateDirectory(root);
        DecisionKeyTests.Run(root, checks);
        await AccountTests.RunAsync(root,checks);
        await ProviderTests.RunAsync(root,checks);
        await AccountProxyTests.RunAsync(root,checks);
        await QuotaTests.RunAsync(root,checks);
        await GeminiQuotaTests.RunAsync(root,checks);
        await GeminiModelTests.RunAsync(root,checks);
        RetiredProviderTests.Run(root,checks);
        await ProxyKeyTests.RunAsync(root, checks);
        EnvKeyTests.Run(root, checks);
        CliConfigurationTests.Run(root, checks);
        var listener = new TcpListener(IPAddress.Loopback, 0); listener.Start();
        int port = ((IPEndPoint)listener.LocalEndpoint).Port; listener.Stop();
        var settings = new Settings { Cookie = "datr=fixture; ps_l=0; ps_n=1; hatch_sess=fixture-private-cookie", Port = port };
        var store = new SettingsStore(root);
        store.Save(settings);
        var loaded = store.Load();
        if (loaded.Cookie != settings.Cookie || loaded.ApiKey != settings.ApiKey || loaded.NodeId != settings.NodeId) throw new InvalidOperationException("Encrypted settings roundtrip failed");
        string cipher = Encoding.UTF8.GetString(File.ReadAllBytes(Path.Combine(root, "settings.protected")));
        if (cipher.Contains("fixture-private-cookie") || cipher.Contains(settings.ApiKey)) throw new InvalidOperationException("Plain credentials found in saved settings");
        checks.Add("DPAPI config roundtrip; no plaintext cookie/API key");
        using var server = new ServerController(root);
        await server.StartAsync(loaded);
        if (!server.Running) throw new InvalidOperationException("Embedded Rust server did not start");
        checks.Add("Embedded server extraction, startup and authenticated readiness");
        string sanitized = server.Sanitize($"Cookie: {settings.Cookie} Authorization: Bearer {settings.ApiKey} auth_token=private-token-value");
        if (sanitized.Contains("fixture-private-cookie") || sanitized.Contains(settings.ApiKey) || sanitized.Contains("private-token-value")) throw new InvalidOperationException("Diagnostic redaction failed");
        if (!server.Sanitize("HTTP 502 elapsed_ms=1209 ps_l=0 ps_n=1").Contains("HTTP 502 elapsed_ms=1209"))
            throw new InvalidOperationException("Preference cookies corrupted diagnostic HTTP status or timing");
        checks.Add("Diagnostic secret redaction");
        using (var http = new HttpClient(new HttpClientHandler { UseProxy = false }))
        {
            using var body = new StringContent("{\"model\":\"muse\",\"messages\":[{\"role\":\"user\",\"content\":\"check\"}]}", Encoding.UTF8, "application/json");
            using var denied = await http.PostAsync($"http://127.0.0.1:{port}/v1/messages/count_tokens", body);
            if (denied.StatusCode != HttpStatusCode.Unauthorized) throw new InvalidOperationException("Unauthenticated request was accepted");
        }
        checks.Add("Proxy rejects unauthenticated Anthropic request");
        // Run the actual bundled launcher under Windows PowerShell with a
        // harmless Claude shim. Inspect its live MCP file before cleanup.
        settings.ProjectFolder = root;
        string capture = Path.Combine(root, "launcher-capture.json");
        File.WriteAllText(Path.Combine(root, "claude.ps1"), """
            $configIndex = [Array]::IndexOf($args, '--mcp-config')
            if ($configIndex -lt 0) { throw 'Missing MCP config' }
            $configPath = $args[$configIndex + 1]
            $snapshot = @{ arguments = @($args); cwd = (Get-Location).Path; configPath = $configPath; config = (Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json) }
            [IO.File]::WriteAllText($env:MUSE_LAUNCHER_TEST_CAPTURE, ($snapshot | ConvertTo-Json -Depth 8))
            exit 0
            """);
        var shimInfo = RuntimeFiles.ClaudeStartInfo(settings, server.RuntimeDirectory);
        shimInfo.ArgumentList.Remove("-NoExit");
        shimInfo.ArgumentList.Add("--resume");
        shimInfo.ArgumentList.Add("fixture-conversation");
        shimInfo.Environment["PATH"] = root + Path.PathSeparator + shimInfo.Environment["PATH"];
        shimInfo.Environment["MUSE_LAUNCHER_TEST_CAPTURE"] = capture;
        shimInfo.CreateNoWindow = true;
        shimInfo.RedirectStandardOutput = shimInfo.RedirectStandardError = true;
        using (var shim = System.Diagnostics.Process.Start(shimInfo) ?? throw new InvalidOperationException("Launcher shim did not start"))
        {
            var output = shim.StandardOutput.ReadToEndAsync();
            var errors = shim.StandardError.ReadToEndAsync();
            using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(35));
            try { await shim.WaitForExitAsync(limit.Token); }
            catch (OperationCanceledException) { shim.Kill(entireProcessTree: true); throw new InvalidOperationException("Launcher shim timed out"); }
            _ = await output; _ = await errors;
            if (shim.ExitCode != 0 || !File.Exists(capture)) throw new InvalidOperationException("Bundled web search launcher failed");
        }
        using (var snapshot = JsonDocument.Parse(File.ReadAllText(capture)))
        {
            var value = snapshot.RootElement;
            var arguments = value.GetProperty("arguments").EnumerateArray().Select(item => item.GetString()).ToArray();
            int deniedIndex = Array.IndexOf(arguments, "--disallowedTools");
            if (deniedIndex < 0 || arguments[deniedIndex + 1] != "WebSearch" ||
                !arguments.Contains("--resume") || !arguments.Contains("fixture-conversation") ||
                value.GetProperty("cwd").GetString() != root)
                throw new InvalidOperationException("Launcher lost tool filtering, working directory or caller arguments");
            var search = value.GetProperty("config").GetProperty("mcpServers").GetProperty("muse-web-search");
            if (search.GetProperty("type").GetString() != "http" ||
                search.GetProperty("url").GetString() != "https://mcp.exa.ai/mcp?tools=web_search_exa" ||
                File.Exists(value.GetProperty("configPath").GetString()))
                throw new InvalidOperationException("Launcher MCP configuration/cleanup failed");
        }
        File.Delete(Path.Combine(root, "claude.ps1"));
        File.Delete(capture);
        checks.Add("Bundled launcher uses web search MCP, excludes native WebSearch, preserves args/cwd and cleans temporary config");
        await CliSetupTests.RunAsync(root, settings, checks);
        if (withClaude)
        {
            settings.ProjectFolder = root;
            var info = RuntimeFiles.ClaudeStartInfo(settings, server.RuntimeDirectory);
            info.ArgumentList.Remove("-NoExit");
            info.ArgumentList.Add("--version");
            info.CreateNoWindow = true;
            info.RedirectStandardOutput = info.RedirectStandardError = true;
            using var child = System.Diagnostics.Process.Start(info) ?? throw new InvalidOperationException("Claude launcher did not start");
            var stdout = child.StandardOutput.ReadToEndAsync();
            var stderr = child.StandardError.ReadToEndAsync();
            using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(35));
            try { await child.WaitForExitAsync(timeout.Token); }
            catch (OperationCanceledException) { child.Kill(entireProcessTree: true); throw new InvalidOperationException("Claude launcher timed out"); }
            string output = await stdout;
            _ = await stderr;
            if (child.ExitCode != 0 || !output.Contains("Claude Code")) throw new InvalidOperationException("Claude launcher validation failed");
            checks.Add("Bundled launcher starts installed Claude Code with isolated proxy configuration");
        }
        using (var other = new ServerController(Path.Combine(root, "other")))
        {
            bool refused = false;
            try { await other.StartAsync(settings); } catch (InvalidOperationException) { refused = true; }
            if (!refused || !server.Running) throw new InvalidOperationException("Port conflict did not preserve existing listener");
        }
        checks.Add("Port conflict preserves existing server");
        server.Stop();
        listener = new TcpListener(IPAddress.Loopback, port); listener.Start(); listener.Stop();
        checks.Add("Stop releases server and listener");
        if (live)
        {
            settings.Cookie = Environment.GetEnvironmentVariable("MUSE_DESK_TEST_COOKIE") ?? throw new InvalidOperationException("Live test requires MUSE_DESK_TEST_COOKIE");
            settings.NodeId = Environment.GetEnvironmentVariable("MUSE_DESK_TEST_NODE_ID") ?? settings.NodeId;
            await server.StartAsync(settings);
            await server.CheckMuseAsync();
            server.Stop();
            checks.Add("Live cookie bootstrap, Muse VM connection and conversations API");
        }
        if (!File.Exists(Path.Combine(root, "runtime", Program.Version, "claude-muse.ps1"))) throw new InvalidOperationException("Claude launcher missing");
        checks.Add("Claude launcher included; config and source workspace not bundled");
        string agyProfile = Path.Combine(root, "agy-profile");
        var agyFirst = AgyCliSetup.Install(agyProfile);
        var agySecond = AgyCliSetup.Install(agyProfile);
        if (!File.Exists(agyFirst.StatuslinePath) || !File.Exists(agyFirst.SettingsPath) || agySecond.BackupPath == null || !File.Exists(agySecond.BackupPath))
            throw new InvalidOperationException("AgyCliSetup did not write expected files or backup");
        using (var agyDoc = JsonDocument.Parse(File.ReadAllText(agyFirst.SettingsPath)))
        {
            var sl = agyDoc.RootElement.GetProperty("statusLine");
            string cmd = sl.GetProperty("command").GetString() ?? "";
            if (!sl.GetProperty("stack_with_default").GetBoolean() || !cmd.Contains(agyProfile.Replace('\\', '/'), StringComparison.Ordinal))
                throw new InvalidOperationException("AgyCliSetup statusLine configuration mismatch");
            var trusted = agyDoc.RootElement.GetProperty("trustedWorkspaces").EnumerateArray().Select(e => e.GetString()).ToArray();
            if (!trusted.Contains(@"F:\All Project\muse-api-rust"))
                throw new InvalidOperationException("AgyCliSetup missing trusted workspace");
        }
        File.Delete(agySecond.BackupPath);
        File.Delete(agyFirst.SettingsPath);
        File.Delete(agyFirst.StatuslinePath);
        Directory.Delete(Path.Combine(agyProfile, ".gemini", "antigravity-cli"));
        Directory.Delete(Path.Combine(agyProfile, ".gemini"));
        Directory.Delete(Path.Combine(agyProfile, ".antigravity"));
        Directory.Delete(agyProfile);
        checks.Add("AgyCliSetup installs portable agy CLI settings and statusline");
    }
}
