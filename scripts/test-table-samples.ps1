param(
    [string]$BinaryPath,
    [string]$OutputDirectory,
    [string]$ResourcesDirectory
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $BinaryPath) { $BinaryPath = Join-Path $projectRoot 'target/release/sightocr-mcp.exe' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $projectRoot 'target/table-samples-after' }
$BinaryPath = [IO.Path]::GetFullPath($BinaryPath)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$targetPrefix = [IO.Path]::GetFullPath((Join-Path $projectRoot 'target')).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if (-not $OutputDirectory.StartsWith($targetPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Sample outputs must be in a subdirectory of the project target directory.'
}
if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
    throw 'Build the release executable first or pass -BinaryPath.'
}
if ($ResourcesDirectory) {
    $ResourcesDirectory = [IO.Path]::GetFullPath($ResourcesDirectory)
    foreach ($name in @('oneocr.dll', 'onnxruntime.dll', 'oneocr.onemodel')) {
        if (-not (Test-Path -LiteralPath (Join-Path $ResourcesDirectory $name) -PathType Leaf)) {
            throw "Missing OCR resource: $name"
        }
    }
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$fixtureRoot = Join-Path $projectRoot 'tests/fixtures/table-lines'
$expectations = Get-Content -LiteralPath (Join-Path $fixtureRoot 'sample-expectations.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$fixtures = Get-Content -LiteralPath (Join-Path $fixtureRoot 'fixtures.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Compact-Cell([string]$Text) { return $Text -replace '\s', '' }
function Column-Number([string]$Letters) {
    $number = 0
    foreach ($letter in $Letters.ToUpperInvariant().ToCharArray()) { $number = $number * 26 + [int]$letter - [int][char]'A' + 1 }
    return $number
}
function Read-Cell($Grid, [int]$Row, [int]$Column) {
    if ($Row -lt 1 -or $Row -gt $Grid.Count -or $Column -lt 1 -or $Column -gt $Grid[$Row - 1].Count) { return '<missing cell>' }
    return [string]$Grid[$Row - 1][$Column - 1]
}
function Check-Cell($Grid, $Issues, [int]$Row, [int]$Column, [string]$Expected) {
    $actual = Read-Cell $Grid $Row $Column
    if ((Compact-Cell $actual) -cne (Compact-Cell $Expected)) {
        $Issues.Add("R${Row}C${Column}: expected [$Expected], got [$actual]")
    }
}
function Check-Merge($Grid, $Issues, [string]$Range) {
    if ($Range -notmatch '^([A-Z]+)(\d+):([A-Z]+)(\d+)') { throw "Invalid independently specified merge: $Range" }
    $left = Column-Number $Matches[1]
    $top = [int]$Matches[2]
    $right = Column-Number $Matches[3]
    $bottom = [int]$Matches[4]
    # The logo in table1 contains no OCR text; other visual merges need a group anchor.
    if ($Range -notmatch '\(logo\)' -and [string]::IsNullOrWhiteSpace((Read-Cell $Grid $top $left))) {
        $Issues.Add("Merged range $Range has no text at its upper-left anchor.")
    }
    for ($row = $top; $row -le $bottom; $row++) {
        for ($column = $left; $column -le $right; $column++) {
            if ($row -ne $top -or $column -ne $left) { Check-Cell $Grid $Issues $row $column '' }
        }
    }
}
function Check-KeyAnchors([string]$Id, $Grid, $Issues) {
    # These few locations come from visual review, not the current executable's output.
    switch ($Id) {
        'table1' {
            Check-Cell $Grid $Issues 7 1 '合同简要内容'
            foreach ($part in @('甲方:', '乙方:', '项目名称:', '一、工作内容:')) {
                if ((Compact-Cell (Read-Cell $Grid 7 2)) -notlike "*$part*") { $Issues.Add("R7C2 is missing contract-body fragment [$part].") }
            }
            Check-Cell $Grid $Issues 10 1 '项目负责人'
            Check-Cell $Grid $Issues 10 2 '同意 签字:马翔'
            Check-Cell $Grid $Issues 15 1 '绿发中心总经理'
        }
        'table2' {
            Check-Cell $Grid $Issues 3 1 '法律'
            Check-Cell $Grid $Issues 3 2 '314'
            Check-Cell $Grid $Issues 3 3 '308'
            Check-Merge $Grid $Issues 'A3:A8'
            Check-Merge $Grid $Issues 'B3:B8'
            Check-Merge $Grid $Issues 'C3:C8'
            Check-Cell $Grid $Issues 9 1 '行政法规'
            Check-Cell $Grid $Issues 9 2 '613'
            Check-Cell $Grid $Issues 9 3 '608'
            Check-Merge $Grid $Issues 'A9:A13'
            Check-Merge $Grid $Issues 'B9:B13'
            Check-Merge $Grid $Issues 'C9:C13'
            Check-Cell $Grid $Issues 20 1 '附件'
            Check-Merge $Grid $Issues 'A20:A23'
            Check-Merge $Grid $Issues 'B20:B23'
            Check-Cell $Grid $Issues 24 2 '18920'
            Check-Merge $Grid $Issues 'B24:G24'
        }
        'table5' {
            Check-Cell $Grid $Issues 2 1 '住宅建筑'
            Check-Cell $Grid $Issues 5 1 '公共建筑'
            $scores = @('5', '7', '12', '5', '7', '12')
            for ($index = 0; $index -lt $scores.Count; $index++) { Check-Cell $Grid $Issues ($index + 2) 4 $scores[$index] }
        }
        'table6' {
            Check-Cell $Grid $Issues 3 1 '锅炉'
            Check-Cell $Grid $Issues 3 2 '燃煤'
            Check-Cell $Grid $Issues 4 2 '燃油燃气'
            Check-Cell $Grid $Issues 5 5 '节能评价值'
            Check-Cell $Grid $Issues 8 5 '5分'
            Check-Cell $Grid $Issues 8 6 '10分'
        }
        'table7' {
            $scores = @('15', '20', '15', '20')
            for ($index = 0; $index -lt $scores.Count; $index++) { Check-Cell $Grid $Issues ($index + 3) 7 $scores[$index] }
            Check-Cell $Grid $Issues 2 3 '平均4~ 6层'
            Check-Cell $Grid $Issues 2 5 '平均10~ 18层'
        }
        'table8' {
            Check-Cell $Grid $Issues 3 1 '初一(1)班课程表'
            Check-Cell $Grid $Issues 4 1 '星期/科目/时间'
            Check-Cell $Grid $Issues 4 3 '星期一'
            Check-Cell $Grid $Issues 7 3 '大课间活动'
            for ($column = 4; $column -le 7; $column++) { Check-Cell $Grid $Issues 7 $column '' }
            Check-Cell $Grid $Issues 12 5 '人工智能基础'
            Check-Cell $Grid $Issues 20 1 '六至八年级综合实践活动课程由学校统筹实施,保障每周1课时。'
        }
        'table9' {
            Check-Cell $Grid $Issues 1 1 '苏州市景范中学作息时间表'
            Check-Cell $Grid $Issues 3 1 '时间'
            Check-Cell $Grid $Issues 4 1 '上午'
            Check-Cell $Grid $Issues 4 3 '大预备铃'
            for ($row = 5; $row -le 12; $row++) { Check-Cell $Grid $Issues $row 1 '' }
            Check-Cell $Grid $Issues 13 1 '下午'
            Check-Cell $Grid $Issues 20 1 '备注'
        }
    }
}

$reports = New-Object 'System.Collections.Generic.List[object]'
$previousResources = [Environment]::GetEnvironmentVariable('SIGHTOCR_RESOURCES', 'Process')
try {
    if ($ResourcesDirectory) { [Environment]::SetEnvironmentVariable('SIGHTOCR_RESOURCES', $ResourcesDirectory, 'Process') }
    foreach ($index in 1..9) {
        $id = "table$index"
        $expected = $expectations.$id
        $fixture = @($fixtures.samples | Where-Object { $_.id -eq $id })[0]
        $image = Join-Path $projectRoot $fixture.image
        $output = Join-Path $OutputDirectory "$id.tsv"
        $stderr = Join-Path $OutputDirectory "$id.stderr.txt"
        $issues = New-Object 'System.Collections.Generic.List[string]'
        $grid = New-Object 'System.Collections.Generic.List[object]'
        $expectedColumns = if ($expected.columns) { [int]$expected.columns } else { [int]$expected.atomicColumns }
        $dataCells = 0
        $merges = @()
        $exitCode = $null
        $timer = [Diagnostics.Stopwatch]::StartNew()
        try {
            if ((Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant() -cne $fixture.image_sha256) {
                throw 'Original sample image changed; refusing to compare against its frozen expectations.'
            }
            if (Test-Path -LiteralPath $output -PathType Leaf) { Remove-Item -LiteralPath $output }
            $start = [Diagnostics.ProcessStartInfo]::new()
            $start.FileName = $BinaryPath
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
            $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
            $start.RedirectStandardInput = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            $process = [Diagnostics.Process]::Start($start)
            try {
                $response = $process.StandardOutput.ReadToEndAsync()
                $errors = $process.StandardError.ReadToEndAsync()
                $requests = @(
                    @{jsonrpc='2.0';id=1;method='initialize';params=@{protocolVersion='2025-11-25';capabilities=@{};clientInfo=@{name='table-test';version='1'}}},
                    @{jsonrpc='2.0';method='notifications/initialized'},
                    @{jsonrpc='2.0';id=2;method='tools/call';params=@{name='sightocr_ocr';arguments=@{image_path=$image;table=$true}}}
                )
                foreach ($request in $requests) { $process.StandardInput.WriteLine(($request | ConvertTo-Json -Compress -Depth 8)) }
                $process.StandardInput.Close()
                if (-not $process.WaitForExit(60000)) { $process.Kill(); $process.WaitForExit(); throw 'Sample OCR exceeded 60 seconds.' }
                $exitCode = $process.ExitCode
                [IO.File]::WriteAllText($stderr, $errors.Result)
                if ($exitCode -ne 0) { throw "MCP exited $exitCode" }
                $reply = ($response.Result.Trim() -split "`n")[-1] | ConvertFrom-Json
                if ($reply.result.isError -or $null -eq $reply.result.structuredContent.text) { throw ("MCP table recognition failed: " + ($reply | ConvertTo-Json -Compress -Depth 8)) }
                $text = $reply.result.structuredContent.text.TrimEnd([char[]]"`r`n")
                [IO.File]::WriteAllText($output, $text, [Text.UTF8Encoding]::new($false))
            } finally { $process.Dispose() }
            if ($text.Length -gt 0) {
                foreach ($line in ($text -split '\r?\n')) { $grid.Add([string[]]($line -split "`t")) }
            }
            if ($grid.Count -ne [int]$expected.logicalRows) { $issues.Add("Expected $($expected.logicalRows) rows, got $($grid.Count).") }
            # Titles above and notes below a framed table are single-cell rows.
            $above = if ($expected.outsideRows) { [int]$expected.outsideRows[0] } else { 0 }
            $below = if ($expected.outsideRows) { [int]$expected.outsideRows[1] } else { 0 }
            for ($row = 0; $row -lt $grid.Count; $row++) {
                $outside = $row -lt $above -or $row -ge $grid.Count - $below
                if ($outside -and $grid[$row].Count -ne 1) { $issues.Add("Row $($row + 1): expected a single title/note cell, got $($grid[$row].Count).") }
                if (-not $outside -and $grid[$row].Count -ne $expectedColumns) { $issues.Add("Row $($row + 1): expected $expectedColumns columns, got $($grid[$row].Count).") }
            }
            if ($null -ne $expected.data) {
                for ($row = 0; $row -lt $expected.data.Count; $row++) {
                    for ($column = 0; $column -lt $expected.data[$row].Count; $column++) {
                        Check-Cell $grid $issues ([int]$expected.headerRows + $row + 1) ($column + 1) ([string]$expected.data[$row][$column])
                        $dataCells++
                    }
                }
            }
            if ($expected.keyMerges) { $merges += @($expected.keyMerges) }
            if ($expected.desiredVisualMerges) { $merges += @($expected.desiredVisualMerges) }
            foreach ($range in $merges) { Check-Merge $grid $issues $range }
            Check-KeyAnchors $id $grid $issues
        } catch {
            $issues.Add($_.Exception.Message)
        }
        $timer.Stop()
        $report = [ordered]@{
            sample = $id; passed = $issues.Count -eq 0; image = $image; output = $output
            expected_rows = [int]$expected.logicalRows; expected_columns = $expectedColumns
            actual_rows = $grid.Count; actual_column_counts = @($grid | ForEach-Object { $_.Count } | Sort-Object -Unique)
            data_cells_checked = $dataCells; declared_merges_checked = $merges.Count
            exit_code = $exitCode; elapsed_ms = $timer.ElapsedMilliseconds; issues = @($issues.ToArray())
        }
        $reports.Add($report)
        $status = if ($report.passed) { 'PASS' } else { 'FAIL' }
        Write-Host "$id ${status}: $($report.actual_rows) rows; column counts $($report.actual_column_counts -join ','); $($issues.Count) issues"
    }
} finally {
    [Environment]::SetEnvironmentVariable('SIGHTOCR_RESOURCES', $previousResources, 'Process')
}
$failed = @($reports | Where-Object { -not $_.passed }).Count
$summary = [ordered]@{
    passed = $failed -eq 0; binary = $BinaryPath
    binary_version = [Diagnostics.FileVersionInfo]::GetVersionInfo($BinaryPath).ProductVersion
    binary_sha256 = (Get-FileHash -LiteralPath $BinaryPath -Algorithm SHA256).Hash.ToLowerInvariant()
    comparison = 'Independent visual expectations; ignore whitespace inside cells only; no OCR substitutions.'
    sample_count = $reports.Count; failed_samples = $failed; samples = @($reports.ToArray())
}
$reportPath = Join-Path $OutputDirectory 'report.json'
[IO.File]::WriteAllText($reportPath, ($summary | ConvertTo-Json -Depth 12), $utf8)
Write-Host "Table sample report: $reportPath"
if ($failed -gt 0) { throw "$failed table samples did not match the independent expectations. See report.json." }
