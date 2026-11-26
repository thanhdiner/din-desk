function Get-DinDeskConnection([string]$Directory) {
    if (-not $Directory) { $Directory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'DinDesk' }
    $Directory = [IO.Path]::GetFullPath($Directory)
    $settingsPath = Join-Path $Directory 'settings.protected'
    if (-not (Test-Path -LiteralPath $settingsPath -PathType Leaf)) {
        throw 'Mo Din Desk, them tai khoan va ket noi proxy.'
    }

    $plain = $null
    try {
        if ($PSVersionTable.PSEdition -eq 'Desktop') { Add-Type -AssemblyName System.Security }
        else { Add-Type -AssemblyName System.Security.Cryptography.ProtectedData }
        $plain = [Security.Cryptography.ProtectedData]::Unprotect(
            [IO.File]::ReadAllBytes($settingsPath), $null,
            [Security.Cryptography.DataProtectionScope]::CurrentUser)
        $settings = [Text.Encoding]::UTF8.GetString($plain) | ConvertFrom-Json
    } catch {
        throw 'Khong doc duoc cau hinh ma hoa cua Din Desk.'
    } finally {
        if ($null -ne $plain) { [Array]::Clear($plain, 0, $plain.Length) }
    }

    $port = [int]$settings.Port
    $context = [int]$settings.ContextTokens
    $key = [string]$settings.ApiKey
    if ($port -lt 1024 -or $port -gt 65535 -or $context -lt 64000 -or $context -gt 1000000 -or
        [string]::IsNullOrWhiteSpace($key) -or $key -match '[\r\n]' -or $settings.Provider -notin @('muse','gemini')) {
        throw 'Cau hinh ket noi cua Din Desk khong hop le.'
    }
    if ($settings.ApiKeyExpiresAt -and [DateTimeOffset]$settings.ApiKeyExpiresAt -le [DateTimeOffset]::UtcNow) {
        throw 'API key da het han. Mo Din Desk, vao Cai dat va bam Doi API key.'
    }

    $listener = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue |
        Where-Object LocalAddress -eq '127.0.0.1' | Select-Object -First 1
    if (-not $listener) { throw 'Proxy Din Desk chua chay. Mo Din Desk va ket noi tai khoan.' }
    $owner = Get-Process -Id $listener.OwningProcess -ErrorAction SilentlyContinue
    $runtimeRoot = [IO.Path]::GetFullPath((Join-Path $Directory 'runtime')) + [IO.Path]::DirectorySeparatorChar
    if (-not $owner.Path -or -not $owner.Path.StartsWith($runtimeRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($owner.Path) -ne 'muse-api.exe') {
        throw 'Cong localhost khong thuoc proxy Din Desk. Kiem tra ket noi trong app.'
    }
    [pscustomobject]@{ Url = "http://127.0.0.1:$port"; Key = $key; Port = $port; Model = [string]$settings.Provider; ContextTokens = $context }
}

function Test-DinDeskConnection($Connection) {
    Add-Type -AssemblyName System.Net.Http
    $handler = New-Object System.Net.Http.HttpClientHandler
    $handler.UseProxy = $false
    $handler.AllowAutoRedirect = $false
    $http = New-Object System.Net.Http.HttpClient($handler)
    $http.Timeout = [TimeSpan]::FromSeconds(10)
    $request = New-Object System.Net.Http.HttpRequestMessage('POST', "$($Connection.Url)/v1/messages/count_tokens")
    $request.Headers.Authorization = New-Object System.Net.Http.Headers.AuthenticationHeaderValue('Bearer', $Connection.Key)
    $body = @{ model = $Connection.Model; messages = @(@{ role = 'user'; content = 'connection check' }) } | ConvertTo-Json -Depth 5 -Compress
    $request.Content = New-Object System.Net.Http.StringContent($body, [Text.Encoding]::UTF8, 'application/json')
    $reply = $null
    try {
        try { $reply = $http.SendAsync($request).GetAwaiter().GetResult() }
        catch { throw 'Khong goi duoc proxy localhost. Kiem tra ket noi trong Din Desk.' }
        if (-not $reply.IsSuccessStatusCode) {
            $status = [int]$reply.StatusCode
            $errorBody = $reply.Content.ReadAsStringAsync().GetAwaiter().GetResult()
            if ($status -eq 401 -and $errorBody -match 'proxy_key_expired') {
                throw 'API key da het han. Mo Din Desk, vao Cai dat va bam Doi API key.'
            }
            throw "Proxy Din Desk tu choi ket noi (HTTP $status). Kiem tra trang thai trong app."
        }
    } finally {
        if ($reply) { $reply.Dispose() }
        $request.Dispose()
        $http.Dispose()
    }
}
