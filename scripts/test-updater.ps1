param([string]$BinaryPath)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $BinaryPath) { $BinaryPath = Join-Path $projectRoot 'target/debug/SightOCR.exe' }
$BinaryPath = [IO.Path]::GetFullPath($BinaryPath)
if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) { throw 'Build SightOCR first or pass -BinaryPath.' }
$testRoot = Join-Path $projectRoot ('target/updater-smoke-' + [guid]::NewGuid().ToString('N'))
$testPrefix = [IO.Path]::GetFullPath($testRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$utf8 = New-Object System.Text.UTF8Encoding($false)
New-Item -ItemType Directory -Path $testRoot -Force | Out-Null

function Assert-TestPath([string]$Candidate) {
    if (-not [IO.Path]::GetFullPath($Candidate).StartsWith($testPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing operation outside isolated updater directory: $Candidate"
    }
}
function Start-Hidden([string]$File, [string[]]$Arguments) {
    Assert-TestPath $File
    return Start-Process -FilePath $File -ArgumentList $Arguments -WindowStyle Hidden -PassThru
}
function Read-Lines([string]$File) {
    if (Test-Path -LiteralPath $File) { return @([IO.File]::ReadAllLines($File) | Where-Object { $_.Length -gt 0 }) }
    return @()
}
function Wait-ForStart([string]$File, [Diagnostics.Process]$Process) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt 10000) {
        if (@(Read-Lines $File).Count -gt 0) { return }
        if ($Process.HasExited) { throw 'Synthetic app exited before writing its start event.' }
        Start-Sleep -Milliseconds 25
    }
    throw 'Synthetic app did not start within 10 seconds.'
}

$fixtureExe = Join-Path $testRoot 'UpdateFixture.exe'
$csc = Join-Path $env:windir 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
& $csc /nologo /codepage:65001 /target:winexe "/out:$fixtureExe" (Join-Path $PSScriptRoot 'updater/UpdateFixture.cs')
if ($LASTEXITCODE -ne 0) { throw 'Synthetic updater fixture compilation failed.' }
if (-not ('UpdateFixture' -as [type])) { [Reflection.Assembly]::LoadFrom($fixtureExe) | Out-Null }

$reports = @()
foreach ($scenario in @('success', 'failure', 'bad-hash')) {
    $caseRoot = Join-Path $testRoot ($scenario + ' 中文 空格')
    $staging = Join-Path $caseRoot '下载 暂存'
    $installed = Join-Path $caseRoot '安装 目录'
    foreach ($directory in @($staging, $installed)) {
        Assert-TestPath $directory
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }
    $helperFile = Join-Path $staging 'SightOCR-Updater.exe'
    $installerFile = Join-Path $staging 'SightOCR-Setup-2.0.3.exe'
    $appFile = Join-Path $installed 'SightOCR.exe'
    Copy-Item -LiteralPath $BinaryPath -Destination $helperFile
    Copy-Item -LiteralPath $fixtureExe -Destination $installerFile
    Copy-Item -LiteralPath $fixtureExe -Destination $appFile
    [IO.File]::WriteAllText((Join-Path $staging 'fixture-mode.txt'), $scenario, $utf8)
    $appEvents = Join-Path $installed 'app-events.log'
    $setupEvents = Join-Path $staging 'setup-events.log'
    $releaseParent = Join-Path $installed 'release-parent'
    $original = $null
    $helper = $null
    $errorText = $null
    try {
        $original = Start-Hidden $appFile @('--hold-for-helper')
        Wait-ForStart $appEvents $original
        $hash = (Get-FileHash -LiteralPath $installerFile -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($scenario -eq 'bad-hash') { $hash = '0' * 64 }
        $manifest = [ordered]@{
            version = '2.0.3' # Synthetic test release; does not change the package version.
            install_dir = $installed
            size = (Get-Item -LiteralPath $installerFile).Length
            sha256 = $hash
            parent_pid = $original.Id
        }
        $manifestFile = Join-Path $staging 'update.json'
        [IO.File]::WriteAllText($manifestFile, ($manifest | ConvertTo-Json -Compress), $utf8)
        $helper = Start-Hidden $helperFile @('--apply-update', ('"' + $manifestFile + '"'))
        Start-Sleep -Milliseconds 750
        if ($helper.HasExited -or (Test-Path -LiteralPath $setupEvents)) {
            throw 'The helper did not wait for its exact original app process to exit.'
        }
        [IO.File]::WriteAllText($releaseParent, '', $utf8)
        if (-not $original.WaitForExit(10000) -or $original.ExitCode -ne 0) { throw 'Synthetic parent did not exit normally.' }

        $timer = [Diagnostics.Stopwatch]::StartNew()
        while (-not $helper.HasExited -and $timer.ElapsedMilliseconds -lt 30000) {
            $message = [UpdateFixture]::DismissOwnUpdateError($helper.Id, (Join-Path $staging 'dialog-controls.txt'))
            if ($message) { $errorText = $message }
            Start-Sleep -Milliseconds 50
        }
        if (-not $helper.HasExited) { throw 'Isolated update helper exceeded 30 seconds.' }
        # Recovery uses spawn; wait briefly for the synthetic app to finish logging.
        $timer.Restart()
        while (@(Read-Lines $appEvents).Count -lt 2 -and $timer.ElapsedMilliseconds -lt 5000) {
            Start-Sleep -Milliseconds 25
        }
        $starts = @(Read-Lines $appEvents)
        $setupStarts = @(Read-Lines $setupEvents)
        if ($starts.Count -ne 2) { throw "Expected exactly one app restart after the original instance: $($starts -join ', ')" }
        if ($scenario -eq 'success') {
            if ($helper.ExitCode -ne 0 -or $errorText -or $setupStarts.Count -ne 1 -or $starts[1] -ne 'started:--from-setup') {
                throw 'Successful update did not restart exactly once through Setup and exit successfully.'
            }
        } else {
            if ($helper.ExitCode -eq 0 -or -not $errorText -or $starts[1] -ne 'started:') {
                throw "Failed update invariant ($scenario): exit=$($helper.ExitCode); error=$errorText; starts=$($starts -join ', ')"
            }
            if ($scenario -eq 'failure' -and ($setupStarts.Count -ne 1 -or -not $errorText.Contains('42'))) {
                throw 'Setup exit code 42 was not reported by the helper.'
            }
            if ($scenario -eq 'bad-hash' -and ($setupStarts.Count -ne 0 -or -not $errorText.Contains('SHA256'))) {
                throw 'Corrupt installer digest did not prevent execution with a checksum error.'
            }
        }
        $arguments = @()
        if ($scenario -ne 'bad-hash') {
            $arguments = @(Read-Lines (Join-Path $staging 'setup-arguments.txt'))
            foreach ($expected in @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/UPDATE', ('/DIR=' + $installed))) {
                if ($arguments -cnotcontains $expected) { throw "Installer argument was changed or split: $expected" }
            }
            $logArgument = @($arguments | Where-Object { $_.StartsWith('/LOG=') })
            if ($logArgument.Count -ne 1) { throw 'Expected exactly one installer log argument.' }
            $logPath = $logArgument[0].Substring(5)
            if ($logPath.StartsWith('\\?\')) { $logPath = $logPath.Substring(4) }
            if ($logPath -cne (Join-Path $staging 'installer.log')) { throw 'Installer log path was changed or split.' }
            if ($arguments.Count -ne 6) { throw 'The installer received unexpected extra arguments.' }
        }
        $reports += [pscustomobject]@{
            Scenario = $scenario
            HelperExitCode = $helper.ExitCode
            WaitedForOriginalProcess = $true
            AppStarts = $starts
            InstallerStarts = $setupStarts.Count
            InstallerArguments = $arguments
            ErrorMessage = $errorText
            Passed = $true
        }
    } finally {
        # Only the two exact process objects started for this isolated case.
        [IO.File]::WriteAllText($releaseParent, '', $utf8)
        foreach ($process in @($helper, $original)) {
            if ($process -and -not $process.HasExited) {
                Assert-TestPath $process.MainModule.FileName
                if (-not $process.WaitForExit(1000)) { $process.Kill(); $process.WaitForExit() }
            }
        }
    }
}
$reportFile = Join-Path $testRoot 'report.json'
[ordered]@{
    Complete = $true
    SourceBinary = $BinaryPath
    SourceSHA256 = (Get-FileHash -LiteralPath $BinaryPath -Algorithm SHA256).Hash
    IsolatedRoot = $testRoot
    Cases = $reports
    Scope = 'Local helper supervision with synthetic installers; no network, real installation, registry, settings or models.'
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $reportFile -Encoding UTF8
Write-Host "Updater smoke report: $reportFile"
