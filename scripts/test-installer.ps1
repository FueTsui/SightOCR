#Requires -Version 7
# Isolated end-to-end test of SightOCR's own installer.
#
# Uses a debug build of SightOCR-Setup, whose SIGHTOCR_SETUP_TEST_ROOT mode
# redirects every location (registry, shortcuts, Program Files, AppData) into a
# throwaway folder under target/. No administrator rights are needed and a real
# SightOCR installation is never touched. A synthetic program stands in for
# SightOCR so closing and relaunching can be observed.
#
# -Updater names a SightOCR 2.0.x executable. Its real --apply-update code is
# used to install the new package, proving in-app updates from that version.
# -Package names a real package folder; it is installed (still isolated) and
# its MCP server must recognise tests/fixtures/basic.png with the real model.
param(
    [string]$Updater = (Join-Path $env:ProgramFiles 'SightOCR\SightOCR.exe'),
    # A real package folder (with OneOCR resources) for the installed-OCR check.
    [string]$Package = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist\SightOCR')
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $projectRoot
$version = [regex]::Match((Get-Content -Raw Cargo.toml), '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value

& cargo build --bin SightOCR-Setup
if ($LASTEXITCODE -ne 0) { throw 'Building SightOCR-Setup failed.' }
$stub = Join-Path $projectRoot 'target\debug\SightOCR-Setup.exe'

$work = Join-Path $projectRoot ('target\setup-test-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$root = Join-Path $work 'root'
New-Item -ItemType Directory -Path $work | Out-Null
$env:SIGHTOCR_SETUP_TEST_ROOT = $root
$regBase = 'HKCU:\Software\SightOCR.SetupTest'
$keyName = '{B5261760-0D41-4798-9917-D5AA8C2510C8}}_is1'
$mainKey = "$regBase\MachineUninstall\$keyName"
$runKey = "$regBase\Run"
$programDir = Join-Path $root 'program-files\SightOCR'
$passed = 0

function Assert([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "FAILED: $Message" }
    Write-Host "  ok  $Message"
    $script:passed++
}

function Invoke-Program([string]$File, [string[]]$Arguments, [int]$TimeoutSeconds = 180) {
    $info = [Diagnostics.ProcessStartInfo]::new($File)
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($info)
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $process.Kill($true)
        throw "$File did not finish within $TimeoutSeconds s."
    }
    return $process.ExitCode
}

function Assert-TestRoot {
    $resolvedRoot = [IO.Path]::GetFullPath($root)
    $targetPrefix = [IO.Path]::GetFullPath((Join-Path $projectRoot 'target')).TrimEnd('\') + '\'
    if (-not $resolvedRoot.StartsWith($targetPrefix, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolvedRoot) -ne 'root' -or
        [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($resolvedRoot)) -notmatch '^setup-test-[0-9a-f]{8}$') {
        throw "Refusing operation outside the isolated installer test root: $resolvedRoot"
    }
    return $resolvedRoot
}

function Stop-TestPrograms {
    $testPrefix = (Assert-TestRoot).TrimEnd('\') + '\'
    Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith($testPrefix, [StringComparison]::OrdinalIgnoreCase) } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 300
}

function Reset-State {
    $resolvedRoot = Assert-TestRoot
    Stop-TestPrograms
    if (Test-Path -LiteralPath $resolvedRoot) { Remove-Item -LiteralPath $resolvedRoot -Recurse -Force }
    if (Test-Path -LiteralPath $regBase) { Remove-Item -LiteralPath $regBase -Recurse -Force }
    New-Item -ItemType Directory -Path $root | Out-Null
}

function Get-Value([string]$Key, [string]$Name) {
    $item = Get-ItemProperty -LiteralPath $Key -Name $Name -ErrorAction SilentlyContinue
    if ($item) { return $item.$Name }
    return $null
}

function New-Shortcut([string]$Path, [string]$Target) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $Path) -Force | Out-Null
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($Path)
    $shortcut.TargetPath = $Target
    $shortcut.Save()
}

function Get-ShortcutTarget([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    return (New-Object -ComObject WScript.Shell).CreateShortcut($Path).TargetPath
}

function Get-Started([string]$Dir) {
    $log = Join-Path $Dir 'events.log'
    if (-not (Test-Path -LiteralPath $log)) { return 0 }
    return @(Get-Content -LiteralPath $log | Where-Object { $_ -like 'started:*' }).Count
}

function Wait-Until([scriptblock]$Condition, [int]$Seconds = 15) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds 250
    }
    return [bool](& $Condition)
}

try {
    # Synthetic SightOCR: a hidden window that answers SightOCR's exit message
    # and records start/exit events next to itself.
    $fixture = Join-Path $work 'fixture\SightOCR.exe'
    New-Item -ItemType Directory -Path (Split-Path -Parent $fixture) | Out-Null
    $csc = Join-Path $env:windir 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    & $csc /nologo /target:winexe /reference:System.Windows.Forms.dll "/out:$fixture" (Join-Path $PSScriptRoot 'installer\ShutdownFixture.cs')
    if ($LASTEXITCODE -ne 0) { throw 'Compiling the synthetic program failed.' }

    $fakePackage = Join-Path $work 'package'
    foreach ($dir in 'resources\oneocr', 'docs', 'skills\sightocr') {
        New-Item -ItemType Directory -Path (Join-Path $fakePackage $dir) -Force | Out-Null
    }
    Copy-Item $fixture (Join-Path $fakePackage 'SightOCR.exe')
    Copy-Item $fixture (Join-Path $fakePackage 'sightocr-mcp.exe')
    Set-Content (Join-Path $fakePackage 'README.md') "new readme $version" -Encoding utf8
    Set-Content (Join-Path $fakePackage 'LICENSE') 'MIT' -Encoding utf8
    Set-Content (Join-Path $fakePackage 'docs\USAGE.md') 'usage' -Encoding utf8
    Set-Content (Join-Path $fakePackage 'docs\MCP.md') 'mcp' -Encoding utf8
    Set-Content (Join-Path $fakePackage 'skills\sightocr\SKILL.md') 'skill' -Encoding utf8
    $random = [Random]::new(7)
    foreach ($name in 'oneocr.dll', 'onnxruntime.dll', 'oneocr.onemodel') {
        $bytes = [byte[]]::new(300000); $random.NextBytes($bytes)
        [IO.File]::WriteAllBytes((Join-Path $fakePackage "resources\oneocr\$name"), $bytes)
    }
    $setup = Join-Path $work "SightOCR-Setup-$version.exe"
    & $stub --pack $fakePackage $setup
    if ($LASTEXITCODE -ne 0) { throw 'Packing failed.' }
    $packageFiles = Get-ChildItem -LiteralPath $fakePackage -Recurse -File |
        ForEach-Object { $_.FullName.Substring($fakePackage.Length + 1) }

    Write-Host "`n[1] Fresh silent install"
    Reset-State
    $code = Invoke-Program $setup @('/VERYSILENT', '/SUPPRESSMSGBOXES', "/LOG=$work\1.log")
    Assert ($code -eq 0) "exit code 0 (got $code)"
    $same = $true
    foreach ($file in $packageFiles) {
        $installed = Join-Path $programDir $file
        if (-not (Test-Path -LiteralPath $installed) -or
            (Get-FileHash -LiteralPath $installed).Hash -ne (Get-FileHash -LiteralPath (Join-Path $fakePackage $file)).Hash) { $same = $false }
    }
    Assert $same 'every packaged file installed byte for byte'
    Assert (Test-Path (Join-Path $programDir 'unins000.exe')) 'uninstaller written'
    Assert ((Get-Value $mainKey 'DisplayVersion') -eq $version) "registered version $version"
    Assert ((Get-Value $mainKey 'InstallLocation') -eq "$programDir\") 'registered install location'
    Assert ((Get-Value $mainKey 'UninstallString') -eq "`"$programDir\unins000.exe`"") 'registered uninstaller'
    Assert ((Get-ShortcutTarget (Join-Path $root 'StartMenu\SightOCR.lnk')) -eq "$programDir\SightOCR.exe") 'Start menu shortcut (default task)'
    Assert (-not (Test-Path (Join-Path $root 'Desktop\SightOCR.lnk'))) 'no desktop shortcut by default'
    Assert ($null -eq (Get-Value $runKey 'SightOCR')) 'no startup entry by default'
    Assert ((Get-Started $programDir) -eq 0) 'silent install does not start SightOCR'
    Assert (-not (Get-ChildItem -LiteralPath $programDir -Directory -Filter '.setup-*')) 'no staging folder left'

    Write-Host "`n[2] Update an Inno Setup 2.0.3 installation with the real 2.0.3 updater"
    if (-not (Test-Path -LiteralPath $Updater)) { throw "Updater not found: $Updater" }
    $updaterVersion = (Get-Item -LiteralPath $Updater).VersionInfo.FileVersion
    Write-Host "  using $Updater ($updaterVersion)"
    Reset-State
    foreach ($file in $packageFiles) {
        $target = Join-Path $programDir $file
        New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $fakePackage $file) -Destination $target
    }
    Set-Content (Join-Path $programDir 'README.md') 'old readme 2.0.3' -Encoding utf8
    foreach ($old in 'docs\RUST_REFACTOR.md', 'docs\VALIDATION.md', 'docs\UI_DESIGN.md', 'unins001.exe', 'unins001.dat', 'unins001.msg') {
        Set-Content (Join-Path $programDir $old) 'inno' -Encoding utf8
    }
    Set-Content (Join-Path $programDir 'run-as-admin') '1' -NoNewline
    Set-Content (Join-Path $programDir 'config.json') '{"kept":true}' -Encoding utf8
    New-Item -Path $mainKey -Force | Out-Null
    New-ItemProperty -Path $mainKey -Name 'DisplayName' -Value 'SightOCR 版本 2.0.3' | Out-Null
    New-ItemProperty -Path $mainKey -Name 'DisplayVersion' -Value '2.0.3' | Out-Null
    New-ItemProperty -Path $mainKey -Name 'InstallLocation' -Value "$programDir\" | Out-Null
    New-ItemProperty -Path $mainKey -Name 'UninstallString' -Value "`"$programDir\unins001.exe`"" | Out-Null
    New-ItemProperty -Path $mainKey -Name 'Inno Setup: App Path' -Value $programDir | Out-Null
    New-Item -Path $runKey -Force | Out-Null
    New-ItemProperty -Path $runKey -Name 'SightOCR' -Value "`"$programDir\SightOCR.exe`" --silent" | Out-Null
    New-Shortcut (Join-Path $root 'StartMenu\SightOCR.lnk') "$programDir\SightOCR.exe"
    $app = Start-Process -FilePath (Join-Path $programDir 'SightOCR.exe') -WindowStyle Hidden -PassThru
    Assert (Wait-Until { (Get-Started $programDir) -eq 1 }) 'old SightOCR running'
    $stage = Join-Path $work 'SightOCR-update-test'
    New-Item -ItemType Directory -Path $stage | Out-Null
    Copy-Item -LiteralPath $Updater -Destination (Join-Path $stage 'SightOCR-Updater.exe')
    $staged = Join-Path $stage "SightOCR-Setup-$version.exe"
    Copy-Item -LiteralPath $setup -Destination $staged
    $manifest = [ordered]@{
        version = $version
        install_dir = $programDir
        size = (Get-Item -LiteralPath $staged).Length
        sha256 = (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash.ToLowerInvariant()
        parent_pid = $app.Id
    } | ConvertTo-Json -Compress
    [IO.File]::WriteAllText((Join-Path $stage 'update.json'), $manifest, [Text.UTF8Encoding]::new($false))
    $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $stage 'SightOCR-Updater.exe'))
    $info.ArgumentList.Add('--apply-update'); $info.ArgumentList.Add((Join-Path $stage 'update.json'))
    $info.UseShellExecute = $false
    $helper = [Diagnostics.Process]::Start($info)
    # SightOCR exits after handing over; the updater waits for exactly that.
    Start-Sleep -Seconds 2
    Assert (-not $helper.HasExited) 'updater waits while SightOCR is still running'
    Stop-Process -Id $app.Id -Force
    if (-not $helper.WaitForExit(180000)) { $helper.Kill($true); throw 'The 2.0.3 updater did not finish (an error dialog may be open).' }
    Assert ($helper.ExitCode -eq 0) "2.0.3 updater reports success (exit $($helper.ExitCode))"
    Assert ((Get-Content (Join-Path $programDir 'README.md') -Raw).Trim() -eq "new readme $version") 'program files replaced'
    Assert (-not (Test-Path (Join-Path $programDir 'docs\RUST_REFACTOR.md'))) 'obsolete documents removed'
    Assert (-not (Get-ChildItem -LiteralPath $programDir -Filter 'unins001.*')) 'Inno Setup uninstaller files removed'
    Assert ($null -ne (Get-Item (Join-Path $programDir 'unins000.exe'))) 'new uninstaller in place'
    Assert ((Get-Content (Join-Path $programDir 'config.json') -Raw).Contains('kept')) 'user file in the folder kept'
    Assert (Test-Path (Join-Path $programDir 'run-as-admin')) 'administrator choice kept'
    Assert ((Get-Value $runKey 'SightOCR') -eq "`"$programDir\SightOCR.exe`" --silent") 'startup entry kept'
    Assert ((Get-ShortcutTarget (Join-Path $root 'StartMenu\SightOCR.lnk')) -eq "$programDir\SightOCR.exe") 'existing shortcut kept'
    Assert ((Get-Value $mainKey 'DisplayName') -eq 'SightOCR') 'registration renamed'
    Assert ((Get-Value $mainKey 'DisplayVersion') -eq $version) "registration now $version"
    Assert ($null -eq (Get-Value $mainKey 'Inno Setup: App Path')) 'Inno Setup values cleared'
    Assert (Wait-Until { (Get-Started $programDir) -ge 2 }) 'SightOCR started again after the update'
    Assert (Test-Path (Join-Path $stage 'installer.log')) 'log written where the updater asked'
    Stop-TestPrograms

    Write-Host "`n[3] In-app update refuses while the MCP server runs"
    $readme = (Get-FileHash (Join-Path $programDir 'README.md')).Hash
    $mcp = Start-Process -FilePath (Join-Path $programDir 'sightocr-mcp.exe') -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 800
    $code = Invoke-Program $setup @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/UPDATE', "/DIR=$programDir", "/LOG=$work\3.log")
    Stop-Process -Id $mcp.Id -Force -ErrorAction SilentlyContinue
    Assert ($code -eq 7) "exit code 7 (got $code)"
    Assert ((Get-Content "$work\3.log" -Raw) -match 'MCP') 'reason logged'
    Assert ((Get-FileHash (Join-Path $programDir 'README.md')).Hash -eq $readme) 'nothing replaced'
    Stop-TestPrograms

    Write-Host "`n[4] A failure while replacing files restores the previous version"
    $exe = Join-Path $programDir 'SightOCR.exe'
    [IO.File]::AppendAllText($exe, 'previous-build')
    $before = (Get-FileHash $exe).Hash
    $uninstallerBefore = (Get-FileHash (Join-Path $programDir 'unins000.exe')).Hash
    $lock = [IO.File]::Open((Join-Path $programDir 'docs\USAGE.md'), 'Open', 'Read', 'Read')
    try {
        $code = Invoke-Program $setup @('/VERYSILENT', '/SUPPRESSMSGBOXES', "/LOG=$work\4.log")
    } finally { $lock.Close() }
    Assert ($code -eq 3) "exit code 3 (got $code)"
    Assert ((Get-FileHash $exe).Hash -eq $before) 'program restored byte for byte'
    Assert ((Get-FileHash (Join-Path $programDir 'unins000.exe')).Hash -eq $uninstallerBefore) 'uninstaller restored'
    Assert (-not (Get-ChildItem -LiteralPath $programDir -Directory -Filter '.setup-*')) 'staging folder removed'

    Write-Host "`n[5] Update moves an early default folder to Program Files"
    Reset-State
    $legacy = Join-Path $root 'legacy-user\SightOCR'
    $code = Invoke-Program $setup @('/VERYSILENT', '/SUPPRESSMSGBOXES', "/DIR=$legacy", '/TASKS=startmenu,autostart,runasadmin')
    Assert ($code -eq 0) 'installed in the early default folder'
    Assert ((Get-Value $runKey 'SightOCR') -eq "`"$legacy\SightOCR.exe`" --silent") '/TASKS applied (startup)'
    Assert (Test-Path (Join-Path $legacy 'run-as-admin')) '/TASKS applied (administrator)'
    Set-Content (Join-Path $legacy 'config.json') '{"legacy":true}' -Encoding utf8
    $code = Invoke-Program $setup @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/UPDATE', "/DIR=$legacy", '/NOLAUNCH', "/LOG=$work\5.log")
    Assert ($code -eq 0) "exit code 0 (got $code)"
    Assert (Test-Path (Join-Path $programDir 'SightOCR.exe')) 'installed in Program Files'
    Assert (-not (Test-Path (Join-Path $legacy 'SightOCR.exe'))) 'old folder uninstalled'
    Assert (@(Get-ChildItem (Join-Path $root 'AppData\Local\SightOCR\UpgradeBackup') -Recurse -Filter config.json).Count -eq 1) 'old configuration backed up'
    Assert (Test-Path (Join-Path $programDir 'run-as-admin')) 'administrator choice carried over'
    Assert ((Get-Value $runKey 'SightOCR') -eq "`"$programDir\SightOCR.exe`" --silent") 'startup entry moved'
    Assert ((Get-ShortcutTarget (Join-Path $root 'StartMenu\SightOCR.lnk')) -eq "$programDir\SightOCR.exe") 'shortcut moved'
    Assert ((Get-Value $mainKey 'InstallLocation') -eq "$programDir\") 'registration moved'
    Assert (@(Get-ChildItem "$regBase\MachineUninstall").Count -eq 1) 'single registration'
    Assert ((Get-Started $programDir) -eq 0) '/NOLAUNCH respected'

    Write-Host "`n[6] Silent uninstall"
    $settings = Join-Path $root 'AppData\Roaming\SightOCR'
    New-Item -ItemType Directory -Path $settings -Force | Out-Null
    Set-Content (Join-Path $settings 'config.json') '{}' -Encoding utf8
    $code = Invoke-Program (Join-Path $programDir 'unins000.exe') @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$work\6.log")
    Assert ($code -eq 0) "exit code 0 (got $code)"
    Assert (-not (Test-Path (Join-Path $programDir 'SightOCR.exe'))) 'program removed'
    Assert (-not (Test-Path $mainKey)) 'registration removed'
    Assert (-not (Test-Path (Join-Path $root 'StartMenu\SightOCR.lnk'))) 'shortcut removed'
    Assert ($null -eq (Get-Value $runKey 'SightOCR')) 'startup entry removed'
    Assert (Test-Path (Join-Path $settings 'config.json')) 'settings kept by default'
    Assert (Wait-Until { -not (Test-Path -LiteralPath $programDir) } 20) 'uninstaller and empty folder removed'

    Write-Host "`n[7] A damaged package is refused"
    Reset-State
    $damaged = Join-Path $work 'damaged.exe'
    $bytes = [IO.File]::ReadAllBytes($setup)
    $bytes[$bytes.Length - 5000] = $bytes[$bytes.Length - 5000] -bxor 0xff
    [IO.File]::WriteAllBytes($damaged, $bytes)
    $code = Invoke-Program $damaged @('/VERYSILENT', '/SUPPRESSMSGBOXES', "/LOG=$work\7.log")
    Assert ($code -eq 1) "exit code 1 (got $code)"
    Assert (-not (Test-Path (Join-Path $programDir 'SightOCR.exe'))) 'nothing installed'


    Write-Host "`n[8] Real package: installed program recognises text with the packaged model"
    if (-not (Test-Path -LiteralPath (Join-Path $Package 'resources\oneocr\oneocr.onemodel'))) {
        Write-Host "  skipped (no package with OneOCR resources at $Package)"
    } else {
        Reset-State
        $realSetup = Join-Path $work "real\SightOCR-Setup-$version.exe"
        New-Item -ItemType Directory -Path (Split-Path -Parent $realSetup) | Out-Null
        & $stub --pack $Package $realSetup
        if ($LASTEXITCODE -ne 0) { throw 'Packing the real package failed.' }
        $code = Invoke-Program $realSetup @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/TASKS=', "/LOG=$work\8.log") 600
        Assert ($code -eq 0) "exit code 0 (got $code)"
        $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $programDir 'sightocr-mcp.exe'))
        $info.UseShellExecute = $false
        $info.RedirectStandardInput = $true
        $info.RedirectStandardOutput = $true
        $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
        $info.CreateNoWindow = $true
        $probe = [Diagnostics.Process]::Start($info)
        try {
            $image = Join-Path $projectRoot 'tests\fixtures\basic.png'
            $requests = @(
                @{jsonrpc = '2.0'; id = 1; method = 'initialize'; params = @{protocolVersion = '2025-11-25'; capabilities = @{}; clientInfo = @{name = 'installer'; version = '1' } } },
                @{jsonrpc = '2.0'; method = 'notifications/initialized' },
                @{jsonrpc = '2.0'; id = 2; method = 'tools/call'; params = @{name = 'sightocr_ocr'; arguments = @{image_path = $image; table = $true } } }
            )
            foreach ($request in $requests) { $probe.StandardInput.WriteLine(($request | ConvertTo-Json -Compress -Depth 8)) }
            $probe.StandardInput.Close()
            $output = $probe.StandardOutput.ReadToEndAsync()
            if (-not $probe.WaitForExit(60000)) { $probe.Kill(); throw 'Installed MCP OCR timed out.' }
            $reply = ($output.Result.Trim() -split "`n")[-1] | ConvertFrom-Json
            $text = [string]$reply.result.structuredContent.text
        } finally { $probe.Dispose() }
        Assert (-not $reply.result.isError -and $text.Contains("Alpha`t100") -and $text.Contains("Beta`t200")) 'installed MCP recognises the sample table'
        $code = Invoke-Program (Join-Path $programDir 'unins000.exe') @('/VERYSILENT', '/SUPPRESSMSGBOXES')
        Assert ($code -eq 0) 'real package uninstalls'
    }

    Write-Host "`nAll $passed checks passed. Logs: $work"
} finally {
    Stop-TestPrograms
    if (Test-Path -LiteralPath $regBase) { Remove-Item -LiteralPath $regBase -Recurse -Force }
    Remove-Item Env:\SIGHTOCR_SETUP_TEST_ROOT -ErrorAction SilentlyContinue
}
