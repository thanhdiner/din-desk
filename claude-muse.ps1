# Route this Claude Code process through Muse; restore the terminal afterwards.
# Start the Rust server with MUSE_COOKIE, MUSE_NODE_ID and MUSE_PROXY_API_KEY first.
param([switch]$MuseDesk, [string]$DeskDirectory, [switch]$FullAccess)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'dindesk-connection.ps1')

$claudeCommand = Get-Command claude -CommandType Application,ExternalScript -ErrorAction Stop | Select-Object -First 1
$contextTokens = 256000
if (-not $MuseDesk -and $env:MUSE_CONTEXT_TOKENS) {
    if (-not [int]::TryParse($env:MUSE_CONTEXT_TOKENS, [ref]$contextTokens) -or $contextTokens -lt 64000 -or $contextTokens -gt 1000000) {
        throw 'MUSE_CONTEXT_TOKENS must be an integer from 64000 to 1000000.'
    }
}
$proxyUrl = if ($env:MUSE_PROXY_URL) { $env:MUSE_PROXY_URL.TrimEnd('/') } else { 'http://127.0.0.1:3000' }
$proxyKey = $env:MUSE_PROXY_API_KEY
$model = if ($env:MUSE_MODEL) { $env:MUSE_MODEL } else { 'muse' }
if ($MuseDesk) {
    $connection = Get-DinDeskConnection $DeskDirectory
    Test-DinDeskConnection $connection
    $proxyUrl = $connection.Url
    $proxyKey = $connection.Key
    $model = $connection.Model
    $contextTokens = $connection.ContextTokens
} elseif ([string]::IsNullOrWhiteSpace($proxyKey)) {
    $proxyKey = [Environment]::GetEnvironmentVariable('MUSE_PROXY_API_KEY', 'User')
}
if ($model -notin @('muse','gemini')) { throw 'MUSE_MODEL must be muse or gemini.' }
if ([string]::IsNullOrWhiteSpace($proxyKey)) {
    throw 'Set MUSE_PROXY_API_KEY to the same key as the Rust server before running this launcher.'
}

# Auth/protocol preflight does not send a chat message or create a Muse session.
$checkBody = @{ model = $model; messages = @(@{ role = 'user'; content = 'connection check' }) } | ConvertTo-Json -Depth 5 -Compress
if (-not $MuseDesk) { try {
    $null = Invoke-RestMethod -Method Post -Uri "$proxyUrl/v1/messages/count_tokens" `
        -Headers @{ Authorization = "Bearer $proxyKey" } `
        -ContentType 'application/json' -Body $checkBody -TimeoutSec 10 -MaximumRedirection 0
} catch {
    $preflightError = $_
    $recovered = $false
    # Existing terminals still hold the old cy function with a Windows user
    # key. Recover only a 401 from this exact app-owned loopback endpoint.
    if ([int]$_.Exception.Response.StatusCode -eq 401) {
        try {
            $target = [uri]$proxyUrl
            if ($target.Scheme -eq 'http' -and $target.IsLoopback -and $target.AbsolutePath -eq '/' -and -not $target.UserInfo) {
                $connection = Get-DinDeskConnection $DeskDirectory
                if ($target.Port -eq $connection.Port) {
                    Test-DinDeskConnection $connection
                    $model = $connection.Model
                    $proxyUrl = $connection.Url
                    $proxyKey = $connection.Key
                    $contextTokens = $connection.ContextTokens
                    $recovered = $true
                    Write-Host 'Da dung cau hinh ket noi cua Muse Desk (khoa Windows cu khong con khop).'
                }
            }
        } catch { }
    }
    if (-not $recovered) {
        throw "Muse proxy preflight failed at $proxyUrl. Neu dung app, mo Muse Desk va ket noi tai khoan, sau do chay cy. $($preflightError.Exception.Message)"
    }
} }

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
if ($MuseDesk) {
    $overrides.NO_PROXY = (@($env:NO_PROXY, '127.0.0.1', 'localhost') | Where-Object { $_ }) -join ','
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
    $options = @('--model', $model, '--disallowedTools', 'WebSearch', '--mcp-config', $searchConfigPath)
    if ($FullAccess -and '--dangerously-skip-permissions' -notin $args) { $options += '--dangerously-skip-permissions' }
    & $claudeCommand.Source @options @args
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
