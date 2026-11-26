param([string]$DeskDirectory, [switch]$FullAccess)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'dindesk-connection.ps1')
$codexCommand = Get-Command codex -CommandType Application,ExternalScript -ErrorAction Stop | Select-Object -First 1
$connection = Get-DinDeskConnection $DeskDirectory
Test-DinDeskConnection $connection

$model = $connection.Model
$context = $connection.ContextTokens
$catalog = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'codex-model-catalog.json') -Raw | ConvertFrom-Json
$catalog.models[0].slug = $model
$catalog.models[0].display_name = $model
$catalog.models[0].context_window = $context
$catalog.models[0].max_context_window = $context
$catalogPath = Join-Path ([IO.Path]::GetTempPath()) ('DinDesk-codex-model-' + [Guid]::NewGuid().ToString('N') + '.json')
$savedKey = $env:DINDESK_PROXY_API_KEY
$savedNoProxy = $env:NO_PROXY
try {
    [IO.File]::WriteAllText($catalogPath, ($catalog | ConvertTo-Json -Depth 7 -Compress), (New-Object Text.UTF8Encoding($false)))
    $env:DINDESK_PROXY_API_KEY = $connection.Key
    $env:NO_PROXY = (@($savedNoProxy, '127.0.0.1', 'localhost') | Where-Object { $_ }) -join ','
    $options = @(
        '-c', 'model_provider=dindesk', '-c', "model=$model", '-c', "model_context_window=$context",
        '-c', "model_catalog_json=$catalogPath", '-c', 'model_providers.dindesk.name=DinDesk',
        '-c', "model_providers.dindesk.base_url=$($connection.Url)/v1",
        '-c', 'model_providers.dindesk.env_key=DINDESK_PROXY_API_KEY',
        '-c', 'model_providers.dindesk.wire_api=responses',
        '-c', 'model_providers.dindesk.requires_openai_auth=false',
        '-c', 'model_providers.dindesk.supports_websockets=false',
        '-c', 'model_providers.dindesk.capabilities.remote_compaction=unsupported',
        '-c', 'web_search=disabled'
    )
    if ($FullAccess -and '--dangerously-bypass-approvals-and-sandbox' -notin $args -and '--yolo' -notin $args) {
        $options += '--dangerously-bypass-approvals-and-sandbox'
    }
    & $codexCommand.Source @options @args
    if ($LASTEXITCODE -ne 0) { throw "Codex exited with code $LASTEXITCODE" }
} finally {
    [Environment]::SetEnvironmentVariable('DINDESK_PROXY_API_KEY', $savedKey, 'Process')
    [Environment]::SetEnvironmentVariable('NO_PROXY', $savedNoProxy, 'Process')
    if (Test-Path -LiteralPath $catalogPath) { Remove-Item -LiteralPath $catalogPath -Force }
}
