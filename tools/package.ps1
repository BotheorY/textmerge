# SPDX-License-Identifier: MIT
# Package only the runtime deliverable, documentation, and redistribution notices.
$ErrorActionPreference = 'Stop'
. 'F:\rust\Enter-Rust.ps1'
$projectDirectory = Split-Path -Parent $PSScriptRoot
$binaryDirectory = 'C:\DEV\TextMerge\bin'
$packageDirectory = Join-Path $binaryDirectory 'portable\TextMerge'
$licenseDirectory = Join-Path $packageDirectory 'ThirdPartyLicenses'
New-Item -ItemType Directory -Force -Path $packageDirectory,$licenseDirectory | Out-Null
Copy-Item -LiteralPath (Join-Path $binaryDirectory 'TextMerge.exe') -Destination $packageDirectory -Force
foreach ($name in @('README.md','LICENSE','CHANGELOG.md')) {Copy-Item -LiteralPath (Join-Path $projectDirectory $name) -Destination $packageDirectory -Force}
foreach ($name in @('README.md','LICENSE','CHANGELOG.md')) {Copy-Item -LiteralPath (Join-Path $projectDirectory $name) -Destination $binaryDirectory -Force}
$assetDirectory = Join-Path $packageDirectory 'assets'
New-Item -ItemType Directory -Force -Path $assetDirectory | Out-Null
Copy-Item -LiteralPath (Join-Path $projectDirectory 'assets\UNICODE-LICENSE.txt') -Destination $assetDirectory -Force
Push-Location $projectDirectory
try {
    $metadata = & cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-gnu | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) {throw 'Cannot read locked dependency metadata'}
    $notices = [System.Collections.Generic.List[string]]::new()
    $notices.Add('# TextMerge third-party notices')
    $notices.Add('')
    $notices.Add('Dependency source packages and license texts are retained in F:\rust\cargo\registry on the development PC. The following build dependency graph is fixed by Cargo.lock; some packages are build-time tools only.')
    $notices.Add('')
    foreach ($package in $metadata.packages | Sort-Object name,version) {
        if ($package.name -eq 'textmerge') {continue}
        $directory = Join-Path $licenseDirectory ($package.name + '-' + $package.version)
        New-Item -ItemType Directory -Force -Path $directory | Out-Null
        $sourceDirectory = Split-Path -Parent $package.manifest_path
        foreach ($file in Get-ChildItem -LiteralPath $sourceDirectory -File | Where-Object {$_.Name -match '^(LICENSE|LICENCE|COPYING|UNLICENSE|COPYRIGHT)'}) {
            Copy-Item -LiteralPath $file.FullName -Destination $directory -Force
        }
        if ($package.license_file) {Copy-Item -LiteralPath (Join-Path $sourceDirectory $package.license_file) -Destination $directory -Force}
        $notices.Add('- **' + $package.name + ' ' + $package.version + '**: ' + $package.license)
    }
    $unicodeDirectory = Join-Path $licenseDirectory 'Unicode-17.0.0'
    New-Item -ItemType Directory -Force -Path $unicodeDirectory | Out-Null
    Copy-Item -LiteralPath (Join-Path $projectDirectory 'assets\UNICODE-LICENSE.txt') -Destination $unicodeDirectory -Force
    $rustDirectory = Join-Path $licenseDirectory 'Rust-standard-library'
    New-Item -ItemType Directory -Force -Path $rustDirectory | Out-Null
    $rustDocs = 'F:\rust\rustup\toolchains\stable-x86_64-pc-windows-gnu\share\doc\rust'
    Copy-Item -LiteralPath (Join-Path $rustDocs 'licenses') -Destination $rustDirectory -Recurse -Force
    Copy-Item -LiteralPath (Join-Path $rustDocs 'COPYRIGHT-library.html') -Destination $rustDirectory -Force
    $runtimeDirectory = Join-Path $licenseDirectory 'MinGW-and-GCC-runtime'
    New-Item -ItemType Directory -Force -Path $runtimeDirectory | Out-Null
    Copy-Item -LiteralPath 'F:\rust\w64devkit\COPYING.MinGW-w64-runtime.txt' -Destination $runtimeDirectory -Force
    foreach ($name in @('GCC-exception-3.1.txt','GPL-3.0-or-later.txt')) {Copy-Item -LiteralPath (Join-Path $rustDocs ('licenses\' + $name)) -Destination $runtimeDirectory -Force}
    $notices.Add('- Unicode 17.0.0 case-folding data: Unicode License v3.')
    $notices.Add('- Rust standard library: MIT OR Apache-2.0, with bundled runtime notices.')
    $notices.Add('- GCC runtime: GPL-3.0-or-later WITH GCC-exception-3.1; MinGW runtime: included component notices.')
    [System.IO.File]::WriteAllLines((Join-Path $packageDirectory 'THIRD-PARTY-NOTICES.md'),$notices,[System.Text.UTF8Encoding]::new($false))
    # ZIP timestamps cannot represent dates before 1980, as found in some crates.
    foreach ($file in Get-ChildItem -LiteralPath $packageDirectory -Recurse -File) {
        if ($file.LastWriteTime.Year -lt 1980) {$file.LastWriteTime=[datetime]'1980-01-01'}
    }
    Compress-Archive -LiteralPath $packageDirectory -DestinationPath (Join-Path $binaryDirectory 'TextMerge-portable.zip') -Force
    # Source distributions contain the working source, not repository history,
    # local agent configuration, credentials directories, or build products.
    $sourceArchivePath = Join-Path $binaryDirectory 'TextMerge-source-MIT.zip'
    $archiveStream = [System.IO.File]::Create($sourceArchivePath)
    $archive = [System.IO.Compression.ZipArchive]::new($archiveStream,[System.IO.Compression.ZipArchiveMode]::Create)
    function Add-SourceDirectory([string]$directory) {
        foreach ($item in Get-ChildItem -LiteralPath $directory -Force) {
            if ($item.Name -in @('.git','.codex','.agents','.aws','target')) {continue}
            if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {continue}
            if ($item.PSIsContainer) {Add-SourceDirectory $item.FullName; continue}
            $entryName = $item.FullName.Substring($projectDirectory.Length + 1).Replace('\','/')
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive,$item.FullName,$entryName,[System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    }
    try {Add-SourceDirectory $projectDirectory} finally {$archive.Dispose();$archiveStream.Dispose()}
} finally {Pop-Location}
