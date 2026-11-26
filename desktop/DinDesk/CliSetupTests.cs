using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal static class CliSetupTests
{
    internal static async Task RunAsync(string root, Settings settings, List<string> checks)
    {
        string documents = Path.Combine(root, "CLI Documents");
        string winProfile = Path.Combine(documents, "WindowsPowerShell", "Microsoft.PowerShell_profile.ps1");
        string psProfile = Path.Combine(documents, "PowerShell", "Microsoft.PowerShell_profile.ps1");
        string shimDir = Path.Combine(root, "CLI Project");
        Directory.CreateDirectory(shimDir);
        string capture = Path.Combine(shimDir, "capture.json");
        string claudeResult = Path.Combine(shimDir, "claude.json");
        string codexResult = Path.Combine(shimDir, "codex.json");
        string probe = Path.Combine(shimDir, "probe.ps1");
        var fixtureFiles = new List<string> { probe, capture, claudeResult, codexResult };
        try
        {
            foreach (string profile in new[] { winProfile, psProfile })
            {
                Directory.CreateDirectory(Path.GetDirectoryName(profile)!);
                File.WriteAllText(profile, "# Original profile café\r\n$env:DINDESK_PROFILE_FIXTURE = 'kept'\r\nfunction cy { throw 'old cy' }\r\nSet-Alias cx Get-Date\r\n", Encoding.Unicode);
            }
            byte[] original = File.ReadAllBytes(winProfile);
            Require(CliSetup.Install(root, documents, "claude") == 2, "Claude setup did not back up both profiles");
            Require(CliSetup.Install(root, documents, "codex") == 2, "Codex setup did not back up both profiles");
            Require(CliSetup.Install(root, documents, "claude") == 0 && CliSetup.Install(root, documents, "codex") == 0, "Repeating CLI setup rewrote the profiles");
            foreach (string profile in new[] { winProfile, psProfile })
            {
                string text = File.ReadAllText(profile);
                Require(text.Contains("Original profile café") && text.Split("# >>> Din Desk cy >>>").Length == 2 && text.Split("# >>> Din Desk cx >>>").Length == 2, "CLI setup lost original profile content or duplicated a command");
                Require(Directory.EnumerateFiles(Path.GetDirectoryName(profile)!, "*.dindesk-*.bak").Any(path => File.ReadAllBytes(path).AsSpan().SequenceEqual(original)), "CLI backup did not preserve the original profile bytes");
                Require(!text.Contains(settings.ApiKey), "CLI profile persisted the API key");
            }

            foreach (string cli in new[] { "claude", "codex" })
            {
                string shim = Path.Combine(shimDir, cli + ".ps1");
                fixtureFiles.Add(shim);
                File.WriteAllText(shim, """
                    $catalog = $null; $catalogPath = $null
                    foreach ($argument in $args) {
                        if ($argument -like 'model_catalog_json=*') {
                            $catalogPath = $argument.Substring('model_catalog_json='.Length)
                            $catalog = Get-Content -LiteralPath $catalogPath -Raw | ConvertFrom-Json
                        }
                    }
                    $key = if ($catalog) { $env:DINDESK_PROXY_API_KEY } else { $env:ANTHROPIC_AUTH_TOKEN }
                    $snapshot = @{ arguments=@($args); cwd=(Get-Location).Path; keyInherited=($key.Length -eq 64); keyInArgs=($args -contains $key); catalog=$catalog; catalogPath=$catalogPath; claudeContext=$env:CLAUDE_CODE_MAX_CONTEXT_TOKENS }
                    [IO.File]::WriteAllText($env:DINDESK_TEST_CAPTURE, ($snapshot | ConvertTo-Json -Depth 12))
                    exit 0
                    """);
            }
            File.WriteAllText(probe, """
                param([string]$FixtureProfile, [string]$Capture, [string]$ClaudeResult, [string]$CodexResult)
                $ErrorActionPreference = 'Stop'
                . $FixtureProfile
                if ($env:DINDESK_PROFILE_FIXTURE -ne 'kept') { throw 'Original profile was not loaded' }
                $env:DINDESK_TEST_CAPTURE = $Capture
                $env:DINDESK_PROXY_API_KEY = 'previous-codex-key'
                $env:ANTHROPIC_AUTH_TOKEN = 'previous-claude-key'
                cy --resume 'Claude fixture session'
                Move-Item -LiteralPath $Capture -Destination $ClaudeResult -Force
                cx exec 'Codex fixture prompt'
                Move-Item -LiteralPath $Capture -Destination $CodexResult -Force
                if ($env:DINDESK_PROXY_API_KEY -ne 'previous-codex-key' -or $env:ANTHROPIC_AUTH_TOKEN -ne 'previous-claude-key') { throw 'Launcher changed the parent shell environment' }
                """);

            await VerifyShellAsync("powershell.exe", winProfile);
            bool hasPwsh = RuntimeFiles.FindCli("pwsh") != null;
            if (hasPwsh) await VerifyShellAsync("pwsh.exe", psProfile);
            checks.Add(hasPwsh ? "CLI setup and launcher execution on Windows PowerShell 5.1 and PowerShell 7" : "CLI setup for both profiles and launcher execution on Windows PowerShell 5.1");
        }
        finally
        {
            foreach (string file in fixtureFiles) File.Delete(file);
            Directory.Delete(shimDir);
            foreach (string profile in new[] { winProfile, psProfile })
            {
                string directory = Path.GetDirectoryName(profile)!;
                if (!Directory.Exists(directory)) continue;
                File.Delete(profile);
                foreach (string backup in Directory.EnumerateFiles(directory, "*.dindesk-*.bak")) File.Delete(backup);
                Directory.Delete(directory);
            }
            if (Directory.Exists(documents)) Directory.Delete(documents);
        }

        async Task VerifyShellAsync(string shell, string profile)
        {
            var info = new ProcessStartInfo(shell) { WorkingDirectory = shimDir, CreateNoWindow = true, UseShellExecute = false,
                RedirectStandardOutput = true, RedirectStandardError = true };
            foreach (string argument in new[] { "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", probe, profile, capture, claudeResult, codexResult }) info.ArgumentList.Add(argument);
            info.Environment["PATH"] = shimDir + Path.PathSeparator + info.Environment["PATH"];
            using var process = Process.Start(info) ?? throw new InvalidOperationException("Could not start CLI profile test");
            var stdout = process.StandardOutput.ReadToEndAsync();
            var stderr = process.StandardError.ReadToEndAsync();
            using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(35));
            try { await process.WaitForExitAsync(timeout.Token); }
            catch (OperationCanceledException) { process.Kill(entireProcessTree: true); throw new InvalidOperationException($"{shell} CLI test timed out"); }
            string errors = await stderr; _ = await stdout;
            Require(process.ExitCode == 0, $"{shell} CLI test failed: {errors.Trim()}");
            using var claude = JsonDocument.Parse(File.ReadAllText(claudeResult));
            VerifyCapture(claude.RootElement, "--dangerously-skip-permissions", "Claude fixture session");
            Require(claude.RootElement.GetProperty("claudeContext").GetString() == settings.ContextTokens.ToString(), "Claude launcher ignored app context");
            using var codex = JsonDocument.Parse(File.ReadAllText(codexResult));
            VerifyCapture(codex.RootElement, "--dangerously-bypass-approvals-and-sandbox", "Codex fixture prompt");
            var catalog = codex.RootElement.GetProperty("catalog").GetProperty("models")[0];
            Require(catalog.GetProperty("slug").GetString() == settings.ModelId && catalog.GetProperty("context_window").GetInt32() == settings.ContextTokens, "Codex launcher ignored app model or context");
            Require(!File.Exists(codex.RootElement.GetProperty("catalogPath").GetString()), "Codex launcher left its temporary catalog on disk");
        }

        void VerifyCapture(JsonElement snapshot, string permission, string argument)
        {
            var args = snapshot.GetProperty("arguments").EnumerateArray().Select(item => item.GetString()).ToArray();
            Require(args.Contains(permission) && args.Contains(argument) && snapshot.GetProperty("cwd").GetString() == shimDir, "CLI launcher lost permissions, args or working directory");
            Require(snapshot.GetProperty("keyInherited").GetBoolean() && !snapshot.GetProperty("keyInArgs").GetBoolean(), "CLI launcher did not pass the key only through the process environment");
        }
    }

    private static void Require(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }
}
