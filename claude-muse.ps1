# Route this Claude Code process through Muse; restore the terminal afterwards.
# Start the Rust server with MUSE_COOKIE, MUSE_NODE_ID and MUSE_PROXY_API_KEY first.
param([switch]$MuseDesk)
$ErrorActionPreference = 'Stop'

function Get-MuseDeskConnection {
    $directory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'MuseDesk'
    $settingsPath = Join-Path $directory 'settings.protected'
    if (-not (Test-Path -LiteralPath $settingsPath)) {
        throw 'Mo Muse Desk, them tai khoan va ket noi truoc khi chay cy.'
    }
    Add-Type -AssemblyName System.Security
    $plain = $null
    try {
        $plain = [Security.Cryptography.ProtectedData]::Unprotect(
            [IO.File]::ReadAllBytes($settingsPath), $null,
            [Security.Cryptography.DataProtectionScope]::CurrentUser)
        $settings = [Text.Encoding]::UTF8.GetString($plain) | ConvertFrom-Json
        $port = [int]$settings.Port
        $key = [string]$settings.ApiKey
        if ($port -lt 1024 -or $port -gt 65535 -or [string]::IsNullOrWhiteSpace($key) -or $key -match '[\r\n]') {
            throw 'Invalid Muse Desk connection settings.'
        }
        # Only send the app's key to its own local Rust child, never to a
        # different program that happens to occupy the configured port.
        $listener = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue |
            Where-Object LocalAddress -eq '127.0.0.1' | Select-Object -First 1
        if (-not $listener) { throw 'Muse Desk proxy is not running.' }
        $owner = Get-Process -Id $listener.OwningProcess -ErrorAction Stop
        $runtimeRoot = [IO.Path]::GetFullPath((Join-Path $directory 'runtime')) + [IO.Path]::DirectorySeparatorChar
        if ([string]::IsNullOrEmpty($owner.Path) -or
            -not $owner.Path.StartsWith($runtimeRoot, [StringComparison]::OrdinalIgnoreCase) -or
            [IO.Path]::GetFileName($owner.Path) -ne 'muse-api.exe') {
            throw 'The configured port is not owned by Muse Desk.'
        }
        $model = switch ($settings.Provider) { 'gemini' { 'gemini' } default { 'muse' } }
        return [pscustomobject]@{ Url = "http://127.0.0.1:$port"; Key = $key; Port = $port; Model = $model }
    } catch {
        throw 'Chua ket noi duoc proxy cua Muse Desk. Mo app, chon tai khoan va bam Ket noi roi chay lai cy.'
    } finally {
        if ($null -ne $plain) { [Array]::Clear($plain, 0, $plain.Length) }
    }
}

$claudeCommand = Get-Command claude -ErrorAction Stop
$contextTokens = 256000
if ($env:MUSE_CONTEXT_TOKENS) {
    if (-not [int]::TryParse($env:MUSE_CONTEXT_TOKENS, [ref]$contextTokens) -or $contextTokens -lt 64000 -or $contextTokens -gt 1000000) {
        throw 'MUSE_CONTEXT_TOKENS must be an integer from 64000 to 1000000.'
    }
}
$proxyUrl = if ($env:MUSE_PROXY_URL) { $env:MUSE_PROXY_URL.TrimEnd('/') } else { 'http://127.0.0.1:3000' }
$proxyKey = $env:MUSE_PROXY_API_KEY
$model = if ($env:MUSE_MODEL) { $env:MUSE_MODEL } else { 'muse' }
if ($model -notin @('muse','gemini')) { throw 'MUSE_MODEL must be muse or gemini.' }
if ($MuseDesk) {
    $connection = Get-MuseDeskConnection
    $proxyUrl = $connection.Url
    $proxyKey = $connection.Key
    $model = $connection.Model
} elseif ([string]::IsNullOrWhiteSpace($proxyKey)) {
    $proxyKey = [Environment]::GetEnvironmentVariable('MUSE_PROXY_API_KEY', 'User')
}
if ([string]::IsNullOrWhiteSpace($proxyKey)) {
    throw 'Set MUSE_PROXY_API_KEY to the same key as the Rust server before running this launcher.'
}

# Auth/protocol preflight does not send a chat message or create a Muse session.
$checkBody = @{ model = $model; messages = @(@{ role = 'user'; content = 'connection check' }) } | ConvertTo-Json -Depth 5 -Compress
try {
    $null = Invoke-RestMethod -Method Post -Uri "$proxyUrl/v1/messages/count_tokens" `
        -Headers @{ Authorization = "Bearer $proxyKey" } `
        -ContentType 'application/json' -Body $checkBody -TimeoutSec 10
} catch {
    $preflightError = $_
    $recovered = $false
    # Existing terminals still hold the old cy function with a Windows user
    # key. Recover only a 401 from this exact app-owned loopback endpoint.
    if (-not $MuseDesk -and [int]$_.Exception.Response.StatusCode -eq 401) {
        try {
            $target = [uri]$proxyUrl
            if ($target.Scheme -eq 'http' -and $target.IsLoopback -and $target.AbsolutePath -eq '/' -and -not $target.UserInfo) {
                $connection = Get-MuseDeskConnection
                if ($target.Port -eq $connection.Port) {
                    $model = $connection.Model
                    $checkBody = @{ model = $model; messages = @(@{ role = 'user'; content = 'connection check' }) } | ConvertTo-Json -Depth 5 -Compress
                    $null = Invoke-RestMethod -Method Post -Uri "$($connection.Url)/v1/messages/count_tokens" `
                        -Headers @{ Authorization = "Bearer $($connection.Key)" } `
                        -ContentType 'application/json' -Body $checkBody -TimeoutSec 10
                    $proxyUrl = $connection.Url
                    $proxyKey = $connection.Key
                    $recovered = $true
                    Write-Host 'Da dung cau hinh ket noi cua Muse Desk (khoa Windows cu khong con khop).'
                }
            }
        } catch { }
    }
    if (-not $recovered) {
        throw "Muse proxy preflight failed at $proxyUrl. Neu dung app, mo Muse Desk va ket noi tai khoan, sau do chay cy. $($preflightError.Exception.Message)"
    }
}

$overrides = @{
    ANTHROPIC_BASE_URL = $proxyUrl
    ANTHROPIC_AUTH_TOKEN = $proxyKey
    ANTHROPIC_API_KEY = $null
    CLAUDE_CODE_OAUTH_TOKEN = $null
    # Fallback identity only. The proxy prefers Claude's native conversation ID
    # from its header/metadata, allowing --resume across launcher restarts.
    ANTHROPIC_CUSTOM_HEADERS = "x-muse-client-id: $([Guid]::NewGuid().ToString())"
    CLAUDE_CODE_USE_BEDROCK = $null
    CLAUDE_CODE_USE_VERTEX = $null
    CLAUDE_CODE_USE_FOUNDRY = $null
    ANTHROPIC_MODEL = $model
    ANTHROPIC_DEFAULT_SONNET_MODEL = $model
    ANTHROPIC_DEFAULT_OPUS_MODEL = $model
    ANTHROPIC_DEFAULT_HAIKU_MODEL = $model
    ANTHROPIC_SMALL_FAST_MODEL = $model
    CLAUDE_CODE_SUBAGENT_MODEL = $model
    # Client budget; the 4 MiB JSON ceiling is an independent byte limit.
    CLAUDE_CODE_MAX_CONTEXT_TOKENS = [string]$contextTokens
    # Avoid reserving the unknown model's default 32k output on every request.
    CLAUDE_CODE_MAX_OUTPUT_TOKENS = '8192'
    ENABLE_TOOL_SEARCH = 'false'
    MAX_THINKING_TOKENS = '0'
    CLAUDE_CODE_DISABLE_THINKING = '1'
    DISABLE_PROMPT_CACHING = '1'
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'
}
$saved = @{}
$searchConfigPath = $null
try {
    foreach ($name in $overrides.Keys) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        if ($null -eq $overrides[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $overrides[$name], 'Process')
        }
    }
    # Anthropic's built-in WebSearch sends a server-tool request that Muse
    # cannot execute. Use Exa's read-only client MCP tool for this session.
    # A file preserves JSON quoting through Windows PowerShell 5 / claude.cmd.
    # No API key or persistent/global Claude configuration is written here.
    $searchConfig = @{
        mcpServers = @{
            'muse-web-search' = @{
                type = 'http'
                url = 'https://mcp.exa.ai/mcp?tools=web_search_exa'
            }
        }
    } | ConvertTo-Json -Depth 6 -Compress
    $searchConfigPath = Join-Path ([IO.Path]::GetTempPath()) ("MuseDesk-web-search-" + [Guid]::NewGuid().ToString('N') + '.json')
    [IO.File]::WriteAllText($searchConfigPath, $searchConfig, (New-Object Text.UTF8Encoding($false)))
    Write-Host "Claude Code -> $proxyUrl -> $model | Web search -> Exa MCP"
    & $claudeCommand.Source --model $model --disallowedTools WebSearch --mcp-config $searchConfigPath @args
    if ($LASTEXITCODE -ne 0) { throw "Claude Code exited with code $LASTEXITCODE" }
} finally {
    if ($searchConfigPath -and (Test-Path -LiteralPath $searchConfigPath)) {
        Remove-Item -LiteralPath $searchConfigPath -Force
    }
    foreach ($name in $saved.Keys) {
        if ($null -eq $saved[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process')
        }
    }
}
