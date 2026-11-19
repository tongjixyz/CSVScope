param([switch]$Release, [string[]]$Files = @(), [string]$BinaryPath = '', [string]$CachePath = '')
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$profile = if ($Release) { 'release' } else { 'debug' }
$binary = if ($BinaryPath) { (Resolve-Path -LiteralPath $BinaryPath).Path } else { Join-Path $workspace "target\$profile\csv-workbench.exe" }
if (-not (Test-Path -LiteralPath $binary)) { throw "请先运行 scripts/build.ps1。缺少 $binary" }
$browserArguments = $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS
$cacheSetting = $env:CSV_WORKBENCH_CACHE_DIR
$oldBenchFiles = $env:CSV_WORKBENCH_BENCH_FILES
$oldBenchOutput = $env:CSV_WORKBENCH_BENCH_OUTPUT
$oldTestStart = $env:CSV_WORKBENCH_TEST_START_MS
$app = $null
Push-Location $workspace
try {
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9223 --remote-debugging-address=127.0.0.1'
    $reportDirectory = Join-Path $workspace '.bench\desktop-smoke'
    $env:CSV_WORKBENCH_CACHE_DIR = Join-Path $workspace '.bench\desktop-smoke-cache'
    $arguments = @('scripts/ui-smoke.mjs', '--desktop')
    if ($Files.Count -gt 0) {
        $reportDirectory = Join-Path $workspace ('.bench\desktop-benchmark\' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
        $env:CSV_WORKBENCH_CACHE_DIR = Join-Path $workspace '.bench\desktop-benchmark-cache'
        $env:CSV_WORKBENCH_BENCH_FILES = ConvertTo-Json -InputObject @($Files | ForEach-Object { (Resolve-Path -LiteralPath $_).Path }) -Compress
        $env:CSV_WORKBENCH_BENCH_OUTPUT = $reportDirectory
        $arguments = @('scripts/desktop-benchmark.mjs')
    }
    if ($CachePath) { $env:CSV_WORKBENCH_CACHE_DIR = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($CachePath) }
    [System.IO.Directory]::CreateDirectory($reportDirectory) | Out-Null
    $env:CSV_WORKBENCH_TEST_START_MS = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds().ToString()
    $app = Start-Process -FilePath $binary -WorkingDirectory (Split-Path -Parent $binary) -PassThru -WindowStyle Hidden
    $ready = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($app.HasExited) { throw "桌面进程启动后退出，退出码 $($app.ExitCode)" }
        try {
            Invoke-RestMethod -Uri 'http://127.0.0.1:9223/json/version' -NoProxy -TimeoutSec 1 | Out-Null
            $ready = $true
            break
        } catch { Start-Sleep -Milliseconds 200 }
    }
    if (-not $ready) { throw '未连接到桌面 WebView 的测试端口。' }
    $outFile = Join-Path $reportDirectory 'test.stdout.txt'
    $errFile = Join-Path $reportDirectory 'test.stderr.txt'
    $test = Start-Process -FilePath (Get-Command node).Source -ArgumentList $arguments -PassThru -WindowStyle Hidden -RedirectStandardOutput $outFile -RedirectStandardError $errFile
    $known = [System.Collections.Generic.HashSet[int]]::new()
    $known.Add($app.Id) | Out-Null
    $peakWorking = 0L
    $peakPrivate = 0L
    $maxProcesses = 0
    $runtimeExecutables = @{}
    while (-not $test.HasExited) {
        $inventory = @(Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId)
        do {
            $added = $false
            foreach ($item in $inventory) {
                if ($known.Contains([int]$item.ParentProcessId) -and $known.Add([int]$item.ProcessId)) { $added = $true }
            }
        } while ($added)
        $working = 0L
        $private = 0L
        $count = 0
        foreach ($processId in $known) {
            $child = Get-Process -Id $processId -ErrorAction SilentlyContinue
            if ($child) {
                $working += $child.WorkingSet64; $private += $child.PrivateMemorySize64; $count++
                if ($child.ProcessName -eq 'msedgewebview2' -and $child.Path -and -not $runtimeExecutables.ContainsKey($child.Path)) {
                    $runtimeExecutables[$child.Path] = (Get-Item -LiteralPath $child.Path).VersionInfo.FileVersion
                }
            }
        }
        $peakWorking = [Math]::Max($peakWorking, $working)
        $peakPrivate = [Math]::Max($peakPrivate, $private)
        $maxProcesses = [Math]::Max($maxProcesses, $count)
        Start-Sleep -Milliseconds 200
    }
    $test.WaitForExit()
    Get-Content -LiteralPath $outFile
    [ordered]@{
        status = 'measured'; exitCode = $test.ExitCode
        fixture = $(if ($Files.Count -eq 0) { 'fixtures/linked.csv' } else { $null }); files = $Files; profile = $profile
        binaryPath = $binary
        binarySha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
        webviewExecutables = $runtimeExecutables
        sampledFullApplicationPeakWorkingSetBytes = $peakWorking
        sampledFullApplicationPeakPrivateBytes = $peakPrivate
        maxProcesses = $maxProcesses
        scope = 'App and all observed descendants, including WebView2; excludes the Playwright test runner.'
        sampleInterval = '200ms plus CIM inspection time; sampled peak is a lower bound'
    } | ConvertTo-Json | Tee-Object -FilePath (Join-Path $reportDirectory 'process.json')
    if ($test.ExitCode -ne 0) { throw (Get-Content -LiteralPath $errFile -Raw) }
} finally {
    if ($app -and -not $app.HasExited) {
        $app.CloseMainWindow() | Out-Null
        if (-not $app.WaitForExit(3000)) { $app.Kill() }
    }
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $browserArguments
    $env:CSV_WORKBENCH_CACHE_DIR = $cacheSetting
    $env:CSV_WORKBENCH_BENCH_FILES = $oldBenchFiles
    $env:CSV_WORKBENCH_BENCH_OUTPUT = $oldBenchOutput
    $env:CSV_WORKBENCH_TEST_START_MS = $oldTestStart
    Pop-Location
}
