param(
    [Parameter(Mandatory)][string]$File,
    [string]$Cache = '.bench\benchmark-cache',
    [string]$Output = '.bench\reports',
    [ValidateSet('open', 'exercise')][string]$Mode = 'open',
    [ValidateSet('debug', 'release')][string]$Profile = 'debug',
    [string]$OsCacheState = 'uncontrolled'
)
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
Push-Location $workspace
try {
    $source = (Resolve-Path -LiteralPath $File).Path
    $cachePath = [System.IO.Path]::GetFullPath($(if ([System.IO.Path]::IsPathRooted($Cache)) { $Cache } else { Join-Path $workspace $Cache }))
    $outputPath = [System.IO.Path]::GetFullPath($(if ([System.IO.Path]::IsPathRooted($Output)) { $Output } else { Join-Path $workspace $Output }))
    [System.IO.Directory]::CreateDirectory($outputPath) | Out-Null
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $reportPath = Join-Path $outputPath "$stamp-$Mode"
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Join-Path $workspace "target\$Profile\csvbench.exe"
    $startInfo.WorkingDirectory = $workspace
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $binaryHash = (Get-FileHash -LiteralPath $startInfo.FileName -Algorithm SHA256).Hash
    $startInfo.ArgumentList.Add($Mode)
    $startInfo.ArgumentList.Add($source)
    $startInfo.ArgumentList.Add($cachePath)
    $process = [System.Diagnostics.Process]::Start($startInfo)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $peakWorkingSet = 0L
    $peakPrivate = 0L
    $maxProcesses = 0
    $known = [System.Collections.Generic.HashSet[int]]::new()
    $known.Add($process.Id) | Out-Null
    $samples = 0
    $mainPeak = 0L
    $peakDatasetBytes = 0L
    $peakResultBytes = 0L
    $peakTempBytes = 0L
    $peakImportBytes = 0L
    $peakCacheBytes = 0L
    $lastDiskSample = -2.0
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    while (-not $process.HasExited) {
        $process.Refresh()
        try { $mainPeak = [Math]::Max($mainPeak, $process.PeakWorkingSet64) } catch {}
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
                $working += $child.WorkingSet64
                $private += $child.PrivateMemorySize64
                $count++
            }
        }
        $peakWorkingSet = [Math]::Max($peakWorkingSet, $working)
        $peakPrivate = [Math]::Max($peakPrivate, $private)
        $maxProcesses = [Math]::Max($maxProcesses, $count)
        if ($timer.Elapsed.TotalSeconds - $lastDiskSample -ge 1) {
            $lastDiskSample = $timer.Elapsed.TotalSeconds
            $datasetBytes = 0L
            $resultBytes = 0L
            $tempBytes = 0L
            $importBytes = 0L
            $cacheBytes = 0L
            if ([System.IO.Directory]::Exists($cachePath)) {
                try {
                    foreach ($cacheFile in [System.IO.Directory]::EnumerateFiles($cachePath, '*', [System.IO.SearchOption]::AllDirectories)) {
                        try {
                            $length = ([System.IO.FileInfo]::new($cacheFile)).Length
                            $cacheBytes += $length
                            if ($cacheFile.Contains('\datasets\')) { $datasetBytes += $length }
                            elseif ($cacheFile.Contains('\spill\')) { $tempBytes += $length }
                            elseif ($cacheFile -match '\\(query|candidate)-[^\\]+\\') { $resultBytes += $length }
                            elseif ($cacheFile -match '\\import-[^\\]+\\') { $importBytes += $length }
                        } catch {}
                    }
                } catch {}
            }
            $peakDatasetBytes = [Math]::Max($peakDatasetBytes, $datasetBytes)
            $peakResultBytes = [Math]::Max($peakResultBytes, $resultBytes)
            $peakTempBytes = [Math]::Max($peakTempBytes, $tempBytes)
            $peakImportBytes = [Math]::Max($peakImportBytes, $importBytes)
            $peakCacheBytes = [Math]::Max($peakCacheBytes, $cacheBytes)
        }
        $samples++
        Start-Sleep -Milliseconds 200
    }
    $process.WaitForExit()
    $outText = $stdout.GetAwaiter().GetResult()
    $errText = $stderr.GetAwaiter().GetResult()
    [System.IO.File]::WriteAllText("$reportPath.stdout.json", $outText)
    [System.IO.File]::WriteAllText("$reportPath.stderr.txt", $errText)
    $cpu = Get-CimInstance Win32_Processor | Select-Object Name,NumberOfCores,NumberOfLogicalProcessors
    $os = Get-CimInstance Win32_OperatingSystem | Select-Object Caption,Version,TotalVisibleMemorySize
    $disks = Get-CimInstance Win32_DiskDrive | Select-Object Model,MediaType,Size
    $report = [ordered]@{
        status = 'measured'
        exitCode = $process.ExitCode
        file = $source
        actualBytes = (Get-Item -LiteralPath $source).Length
        mode = $Mode
        cachePath = $cachePath
        osCacheState = $OsCacheState
        wallSeconds = $timer.Elapsed.TotalSeconds
        sampledTreePeakWorkingSetBytes = $peakWorkingSet
        sampledTreePeakPrivateBytes = $peakPrivate
        mainProcessPeakWorkingSetBytes = $mainPeak
        maxProcessCount = $maxProcesses
        samples = $samples
        sampledDiskPeak = @{
            datasets = $peakDatasetBytes; resultsAndCandidates = $peakResultBytes
            duckdbTemporary = $peakTempBytes; imports = $peakImportBytes; cacheTotal = $peakCacheBytes
            interval = '1s plus process sampling time; per-category peaks may occur at different times'
        }
        sampleInterval = '200ms plus CIM/process inspection time'
        cpu = $cpu
        os = $os
        disks = $disks
        scope = 'Native benchmark CLI process tree; no WebView is started by this CLI.'
        build = $Profile
        binarySha256 = $binaryHash
    }
    $report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath "$reportPath.process.json" -Encoding utf8
    $outText
    $report | ConvertTo-Json -Depth 6
    if ($process.ExitCode -ne 0) { throw $errText }
} finally { Pop-Location }
