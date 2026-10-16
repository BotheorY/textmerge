# SPDX-License-Identifier: MIT
param([switch]$IndexBenchmark)
$ErrorActionPreference = 'Stop'
. 'F:\rust\Enter-Rust.ps1'
$projectDirectory = Split-Path -Parent $PSScriptRoot
$validationDirectory = 'C:\DEV\TextMerge\bin\validation'
New-Item -ItemType Directory -Force -Path $validationDirectory | Out-Null
$started = Get-Date
$prefix = if ($IndexBenchmark) {'index'} else {'stress'}
$test = if ($IndexBenchmark) {'million_line_index_stress'} else {'multi_gigabyte_single_line_stress'}
$stdout = Join-Path $validationDirectory ($prefix + '-stdout.txt')
$stderr = Join-Path $validationDirectory ($prefix + '-stderr.txt')
$process = Start-Process -FilePath 'F:\rust\cargo\bin\cargo.exe' -ArgumentList @('test','--release','--test','merging',$test,'--','--ignored','--nocapture') -WorkingDirectory $projectDirectory -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WindowStyle Hidden -PassThru
$peakWorkingSet = 0L
$peakPrivateBytes = 0L
while (!$process.HasExited) {
    foreach ($worker in Get-Process -Name 'merging-*' -ErrorAction SilentlyContinue) {
        if ($worker.StartTime -ge $started) {
            $peakWorkingSet = [Math]::Max($peakWorkingSet,$worker.PeakWorkingSet64)
            $peakPrivateBytes = [Math]::Max($peakPrivateBytes,$worker.PrivateMemorySize64)
        }
    }
    Start-Sleep -Milliseconds 500
    $process.Refresh()
}
$process.WaitForExit()
$report = [pscustomobject]@{
    ExitCode = $process.ExitCode
    InputBytes = $(if ($IndexBenchmark) {30000000L} else {2L * (2049L * 1024 * 1024 + 1)})
    LargestInputLineBytes = $(if ($IndexBenchmark) {14L} else {2049L * 1024 * 1024})
    PeakWorkingSetBytes = $peakWorkingSet
    PeakSampledPrivateBytes = $peakPrivateBytes
    WallSecondsIncludingBuild = [Math]::Round(((Get-Date)-$started).TotalSeconds,3)
}
$report | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $validationDirectory ($prefix + '-report.json'))
Get-Content -LiteralPath $stdout
Get-Content -LiteralPath $stderr
$report | ConvertTo-Json
if ($process.ExitCode -ne 0) {throw 'Multi-gigabyte stress test failed'}
