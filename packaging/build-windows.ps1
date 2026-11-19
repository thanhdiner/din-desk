param([string]$Version = '0.1.0-beta.18')
$ErrorActionPreference = 'Stop'
$rootPath = Split-Path -Parent $PSScriptRoot
$cargoRoot = Join-Path $env:USERPROFILE '.cargo'
$env:CARGO_HOME = $cargoRoot
$env:RUSTUP_HOME = Join-Path $env:USERPROFILE '.rustup'
$cargoExe = Join-Path $cargoRoot 'bin\cargo.exe'
$targetPath = Join-Path $rootPath 'target\claude-check'
$rustTarget = 'x86_64-pc-windows-msvc'
$previousRustFlags = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS
# Ship without a separate Visual C++ Redistributable installation.
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = ($previousRustFlags+' -C target-feature=+crt-static').Trim()
$releasePath = Join-Path $rootPath "dist\DinDesk-$Version-win-x64"
$publishedPath = Join-Path $rootPath 'target\desktop-publish'
Push-Location -LiteralPath $rootPath
try {
    & $cargoExe build --release --locked --bin muse-api --target $rustTarget --target-dir $targetPath
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' }
    $serverBinary = Join-Path $targetPath "$rustTarget\release\muse-api.exe"
    & dotnet publish 'desktop/DinDesk/DinDesk.csproj' -c Release -r win-x64 --self-contained true -o $publishedPath "-p:ServerBinary=$serverBinary" "-p:Version=$Version"
    if ($LASTEXITCODE -ne 0) { throw 'Desktop publish failed.' }
    New-Item -ItemType Directory -Path $releasePath -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $publishedPath 'DinDesk.exe') -Destination $releasePath -Force
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'HUONG-DAN.txt') -Destination $releasePath -Force
    $metadata = & $cargoExe metadata --locked --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cannot collect dependency metadata.' }
    $notice = @('# Third-party software', '', 'Din Desk includes a Rust server and the Microsoft .NET Windows Desktop runtime.',
        'Rust dependency licenses reported by Cargo are listed below. Full dependency license files are in licenses/rust.',
        'Microsoft .NET license: https://github.com/dotnet/runtime/blob/main/LICENSE.TXT',
        'Microsoft third-party notices: https://github.com/dotnet/runtime/blob/main/THIRD-PARTY-NOTICES.TXT', '')
    foreach ($package in ($metadata.packages | Sort-Object name,version)) {
        $notice += "- $($package.name) $($package.version): $($package.license)"
        if ($package.name -eq 'muse-api') { continue }
        $packageRoot = Split-Path -Parent $package.manifest_path
        $licenseFiles = @(Get-ChildItem -LiteralPath $packageRoot -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)' })
        if ($licenseFiles.Count -gt 0) {
            $licenseTarget = Join-Path $releasePath "licenses\rust\$($package.name)-$($package.version)"
            New-Item -ItemType Directory -Path $licenseTarget -Force | Out-Null
            $licenseFiles | Copy-Item -Destination $licenseTarget
        }
    }
    $notice | Set-Content -LiteralPath (Join-Path $releasePath 'THIRD-PARTY-NOTICES.txt') -Encoding utf8
    # Include the licenses shipped with the exact restored .NET runtime packs.
    $assets = Get-Content -LiteralPath (Join-Path $rootPath 'desktop\DinDesk\obj\project.assets.json') -Raw | ConvertFrom-Json
    $runtimeDownloads = @($assets.project.frameworks.PSObject.Properties | ForEach-Object { $_.Value.downloadDependencies })
    foreach ($runtimeName in @('Microsoft.NETCore.App.Runtime.win-x64','Microsoft.WindowsDesktop.App.Runtime.win-x64')) {
        $runtime = $runtimeDownloads | Where-Object { $_.name -eq $runtimeName } | Select-Object -First 1
        if ($null -eq $runtime) { throw "Missing runtime dependency: $runtimeName" }
        $runtimeVersion = $runtime.version.Trim('[',']').Split(',')[0].Trim()
        $runtimeRoot = $assets.packageFolders.PSObject.Properties.Name | ForEach-Object {
            Join-Path $_ ($runtimeName.ToLowerInvariant()+'\'+$runtimeVersion)
        } | Where-Object { Test-Path -LiteralPath $_ -PathType Container } | Select-Object -First 1
        if ($null -eq $runtimeRoot) { throw "Missing restored runtime pack: $runtimeName $runtimeVersion" }
        $licenseTarget = Join-Path $releasePath ('licenses\dotnet\'+$runtimeName+'-'+$runtimeVersion)
        New-Item -ItemType Directory -Path $licenseTarget -Force | Out-Null
        $runtimeLicenses = @(Get-ChildItem -LiteralPath $runtimeRoot -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|THIRD-PARTY-NOTICES)' })
        if ($runtimeLicenses.Count -eq 0) { throw "Missing runtime license files: $runtimeName $runtimeVersion" }
        $runtimeLicenses | Copy-Item -Destination $licenseTarget
    }
    $report = Join-Path $rootPath "target\DinDesk-$Version-smoke.json"
    $test = Start-Process -FilePath (Join-Path $releasePath 'DinDesk.exe') -ArgumentList @('--self-test','--report',('"'+$report+'"')) -WindowStyle Hidden -Wait -PassThru
    if ($test.ExitCode -ne 0) { throw "Packaged app self-test failed. Inspect $report" }
    $zipPath = Join-Path $rootPath "dist\DinDesk-$Version-win-x64.zip"
    # Open source files with sharing; antivirus scans can briefly hold a new EXE.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipStream = [IO.File]::Open($zipPath, [IO.FileMode]::Create, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $archive = [IO.Compression.ZipArchive]::new($zipStream, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in (Get-ChildItem -LiteralPath $releasePath -File -Recurse)) {
            $relativePath = ([IO.Path]::GetFileName($releasePath) + '/' + $file.FullName.Substring($releasePath.Length + 1)).Replace('\','/')
            $entry = $archive.CreateEntry($relativePath, [IO.Compression.CompressionLevel]::Optimal)
            $inputFile = [IO.File]::Open($file.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
            $outputFile = $entry.Open()
            try { $inputFile.CopyTo($outputFile) } finally { $outputFile.Dispose(); $inputFile.Dispose() }
        }
    } finally { $archive.Dispose(); $zipStream.Dispose() }
    $hash = Get-FileHash -LiteralPath $zipPath -Algorithm SHA256
    "$($hash.Hash.ToLowerInvariant())  $([IO.Path]::GetFileName($zipPath))" | Set-Content -LiteralPath ($zipPath+'.sha256') -Encoding ascii
    [pscustomobject]@{ Archive=$zipPath;SizeMiB=[math]::Round((Get-Item -LiteralPath $zipPath).Length/1MB,1);SHA256=$hash.Hash;SmokeReport=$report } | ConvertTo-Json -Compress
} finally {
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = $previousRustFlags
    Pop-Location
}
