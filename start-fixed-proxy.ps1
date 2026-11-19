# Run with PowerShell. Credentials stay in this console/child process only.
param([string]$ServerBinary = 'target\claude-check\release\muse-api.exe', [switch]$VerifyContextUpload)
$ErrorActionPreference = 'Stop'
$Host.UI.RawUI.WindowTitle = 'Muse proxy - ban sua khoi phuc context'
$rootPath = $PSScriptRoot
$serverPath = [IO.Path]::GetFullPath((Join-Path $rootPath $ServerBinary))
if (-not (Test-Path -LiteralPath $serverPath)) { throw 'Build the release server first.' }
$env:MUSE_PROXY_API_KEY = [Environment]::GetEnvironmentVariable('MUSE_PROXY_API_KEY','User')
if ([string]::IsNullOrWhiteSpace($env:MUSE_PROXY_API_KEY)) { throw 'Missing MUSE_PROXY_API_KEY in user settings.' }
Write-Host 'Muse proxy - ban sua khoi phuc context'
Write-Host 'Dan cookie Muse vao day. Cookie khong hien tren man hinh va khong luu vao file.'
$cookieInput = Read-Host 'Cookie Muse' -AsSecureString
$cookiePointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($cookieInput)
try { $env:MUSE_COOKIE = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($cookiePointer).Trim() }
finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($cookiePointer); $cookieInput.Dispose() }
try {
if ($env:MUSE_COOKIE -notmatch '(^|;\s*)hatch_sess=') { throw 'Cookie phai co hatch_sess.' }
$nodePath = Join-Path $rootPath '.local\fixed-proxy-node-id.txt'
if (-not (Test-Path -LiteralPath $nodePath)) { ('muse-fix-'+[Guid]::NewGuid().ToString('N')) | Set-Content -LiteralPath $nodePath }
$env:MUSE_NODE_ID = (Get-Content -LiteralPath $nodePath -Raw).Trim()
$env:RUST_LOG = 'muse_api=info'
$env:BIND_ADDR = '127.0.0.1:18565'
$env:MUSE_CONVERSATIONS_PATH = Join-Path $rootPath '.local\fixed-proxy-conversations.json'
if ($VerifyContextUpload) {
    $env:CARGO_HOME = Join-Path $env:USERPROFILE '.cargo'
    $env:RUSTUP_HOME = Join-Path $env:USERPROFILE '.rustup'
    $savedConversationsPath = $env:MUSE_CONVERSATIONS_PATH
    try {
        $env:MUSE_CONVERSATIONS_PATH = Join-Path $rootPath '.local\ack-probe-conversations.json'
        & "$env:USERPROFILE\.cargo\bin\cargo.exe" test --lib --target-dir (Join-Path $rootPath 'target\claude-check') muse_live_staged_coding_context_preserves_markers -- --ignored --nocapture
        if ($LASTEXITCODE -ne 0) { throw 'Kiem tra context voi Muse chua dat; giu server cu.' }
    } finally { $env:MUSE_CONVERSATIONS_PATH = $savedConversationsPath }
}
$testProcess = $null
try {
    $testProcess = Start-Process -FilePath $serverPath -WorkingDirectory $rootPath -WindowStyle Hidden -PassThru
    $ready = $false
    for ($attempt=0; $attempt -lt 40; $attempt++) {
        if ($testProcess.HasExited) { throw 'Khong bat duoc proxy thu nghiem.' }
        try { $null = Invoke-RestMethod 'http://127.0.0.1:18565/health' -TimeoutSec 1; $ready=$true; break }
        catch { Start-Sleep -Milliseconds 150 }
    }
    if (-not $ready) { throw 'Proxy thu nghiem chua san sang.' }
    $null = Invoke-RestMethod 'http://127.0.0.1:18565/v1/conversations' -TimeoutSec 40
    Write-Host 'Cookie hop le, da ket noi Muse.'
} finally {
    if ($testProcess -and -not $testProcess.HasExited) { Stop-Process -Id $testProcess.Id; $testProcess.WaitForExit() }
}
# Only replace this project's previous server after account validation succeeds.
$listener = Get-NetTCPConnection -State Listen -LocalPort 3000 -ErrorAction SilentlyContinue
if ($listener) {
    $runningServer = Get-Process -Id ($listener.OwningProcess | Select-Object -First 1)
    $expectedServer = [IO.Path]::GetFullPath((Join-Path $rootPath 'target\claude-check\debug\muse-api.exe'))
    $releaseServer = Join-Path $rootPath 'target\claude-check\release\muse-api.exe'
    if ($runningServer.Path -notin @($expectedServer,$releaseServer,$serverPath)) { throw 'Cong 3000 dang do ung dung khac quan ly; khong tu dung ung dung do.' }
    Stop-Process -Id $runningServer.Id
    $runningServer.WaitForExit()
}
$env:BIND_ADDR = '127.0.0.1:3000'
$serverProcess = Start-Process -FilePath $serverPath -WorkingDirectory $rootPath -WindowStyle Hidden -RedirectStandardOutput (Join-Path $rootPath '.local\muse-server-fixed.log') -RedirectStandardError (Join-Path $rootPath '.local\muse-server-fixed-error.log') -PassThru
Write-Host ('Da chay ban sua tren cong 3000, PID '+$serverProcess.Id+'. Co the dong cua so nay.')
} finally {
    Remove-Item Env:MUSE_COOKIE -ErrorAction SilentlyContinue
    Remove-Item Env:MUSE_PROXY_API_KEY -ErrorAction SilentlyContinue
}
