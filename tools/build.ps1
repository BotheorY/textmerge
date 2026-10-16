# SPDX-License-Identifier: MIT
param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
. 'F:\rust\Enter-Rust.ps1'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    & cargo fmt --check
    if ($LASTEXITCODE -ne 0) {throw 'Formatting check failed'}
    & cargo clippy --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) {throw 'Clippy failed'}
    if (!$SkipTests) {
        & cargo test --locked
        if ($LASTEXITCODE -ne 0) {throw 'Tests failed'}
    }
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) {throw 'Release build failed'}
    Copy-Item -LiteralPath 'C:\DEV\TextMerge\bin\build\release\textmerge.exe' -Destination 'C:\DEV\TextMerge\bin\TextMerge.exe' -Force
} finally {Pop-Location}
