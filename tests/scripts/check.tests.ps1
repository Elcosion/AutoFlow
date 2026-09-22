$ErrorActionPreference = "Stop"

function Assert-True {
    param(
        [bool]$Condition,
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Assert-Equal {
    param(
        $Expected,
        $Actual,
        [string]$Message
    )

    if ($Expected -ne $Actual) {
        throw "$Message`nExpected: $Expected`nActual:   $Actual"
    }
}

function Assert-Syntax {
    param([string]$Path)

    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile(
        $Path,
        [ref]$tokens,
        [ref]$errors
    )
    if ($errors.Count -ne 0) {
        $messages = ($errors | ForEach-Object { $_.Message }) -join [Environment]::NewLine
        throw "PowerShell syntax errors in ${Path}:`n$messages"
    }
}

function Write-Shim {
    param(
        [string]$Path,
        [string]$Tool
    )

    $contents = @"
@echo off
setlocal
>>"%CHECK_TEST_LOG%" echo $Tool^|%CD%^|%*
if /I not "%CHECK_TEST_FAIL_TOOL%"=="$Tool" exit /b 0
if /I not "%CHECK_TEST_FAIL_ARGS%"=="%*" exit /b 0
if not "%CHECK_TEST_STDOUT_MARKER%"=="" echo %CHECK_TEST_STDOUT_MARKER%
if not "%CHECK_TEST_STDERR_MARKER%"=="" 1>&2 echo %CHECK_TEST_STDERR_MARKER%
exit /b %CHECK_TEST_FAIL_CODE%
"@
    [System.IO.File]::WriteAllText(
        $Path,
        ($contents -replace "`n", "`r`n"),
        [System.Text.Encoding]::ASCII
    )
}

function Read-Invocations {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return @()
    }
    return @(
        [System.IO.File]::ReadAllLines($Path) | ForEach-Object {
            $fields = $_ -split '\|', 3
            [pscustomobject]@{
                Tool = $fields[0]
                Cwd  = $fields[1]
                Args = $fields[2]
            }
        }
    )
}

function Assert-Invocations {
    param(
        [object[]]$Actual,
        [object[]]$Expected,
        [string]$Scenario
    )

    Assert-Equal $Expected.Count $Actual.Count "$Scenario invocation count"
    for ($index = 0; $index -lt $Expected.Count; $index++) {
        Assert-Equal $Expected[$index].Tool $Actual[$index].Tool "$Scenario tool at index $index"
        Assert-True (
            [string]::Equals(
                $Expected[$index].Cwd,
                $Actual[$index].Cwd,
                [StringComparison]::OrdinalIgnoreCase
            )
        ) "$Scenario cwd at index $index expected '$($Expected[$index].Cwd)' but got '$($Actual[$index].Cwd)'"
        Assert-Equal $Expected[$index].Args $Actual[$index].Args "$Scenario args at index $index"
    }
}

function Invoke-CheckScenario {
    param(
        [string]$PowerShellPath,
        [string]$CheckPath,
        [string]$WorkingDirectory,
        [string]$ShimDirectory,
        [string]$LogPath,
        [string]$FailTool = "",
        [string]$FailArgs = "",
        [int]$FailCode = 0,
        [string]$StdoutMarker = "",
        [string]$StderrMarker = ""
    )

    if (Test-Path -LiteralPath $LogPath) {
        Remove-Item -LiteralPath $LogPath -Force
    }
    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $PowerShellPath
    $startInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$CheckPath`""
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.EnvironmentVariables["PATH"] = $ShimDirectory
    $startInfo.EnvironmentVariables["CHECK_TEST_LOG"] = $LogPath
    $startInfo.EnvironmentVariables["CHECK_TEST_FAIL_TOOL"] = $FailTool
    $startInfo.EnvironmentVariables["CHECK_TEST_FAIL_ARGS"] = $FailArgs
    $startInfo.EnvironmentVariables["CHECK_TEST_FAIL_CODE"] = [string]$FailCode
    $startInfo.EnvironmentVariables["CHECK_TEST_STDOUT_MARKER"] = $StdoutMarker
    $startInfo.EnvironmentVariables["CHECK_TEST_STDERR_MARKER"] = $StderrMarker

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo
    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    $exitCode = $process.ExitCode
    $process.Dispose()

    return [pscustomobject]@{
        ExitCode    = $exitCode
        Stdout      = $stdout
        Stderr      = $stderr
        Invocations = @(Read-Invocations $LogPath)
    }
}

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$checkPath = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot "scripts\check.ps1"))
$testPath = [System.IO.Path]::GetFullPath($MyInvocation.MyCommand.Path)
$powershellPath = [System.IO.Path]::GetFullPath(
    (Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe")
)
$tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$tempPrefix = $tempBase.TrimEnd([char[]]@('\', '/')) + [System.IO.Path]::DirectorySeparatorChar
$tempRoot = Join-Path $tempBase ("autoflow-check-tests-" + [Guid]::NewGuid().ToString("N"))
$tempRoot = [System.IO.Path]::GetFullPath($tempRoot)

Assert-True (Test-Path -LiteralPath $powershellPath -PathType Leaf) "Windows PowerShell executable not found"
Assert-Syntax $checkPath
Assert-Syntax $testPath
Assert-True ($tempRoot.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase)) "Unsafe temp root"
Assert-True (-not (Test-Path -LiteralPath $tempRoot)) "Temp root already exists"

try {
    $shimDirectory = Join-Path $tempRoot "bin"
    [void][System.IO.Directory]::CreateDirectory($shimDirectory)
    Write-Shim (Join-Path $shimDirectory "npm.cmd") "npm"
    Write-Shim (Join-Path $shimDirectory "cargo.cmd") "cargo"
    $logPath = Join-Path $tempRoot "invocations.log"
    $rustRoot = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot "src-tauri"))
    $allExpected = @(
        [pscustomobject]@{ Tool = "npm"; Cwd = $repositoryRoot; Args = "run typecheck" },
        [pscustomobject]@{ Tool = "npm"; Cwd = $repositoryRoot; Args = "run test:run" },
        [pscustomobject]@{ Tool = "npm"; Cwd = $repositoryRoot; Args = "run format:check" },
        [pscustomobject]@{ Tool = "cargo"; Cwd = $rustRoot; Args = "fmt --all -- --check" },
        [pscustomobject]@{ Tool = "cargo"; Cwd = $rustRoot; Args = "test" },
        [pscustomobject]@{ Tool = "cargo"; Cwd = $rustRoot; Args = "clippy --all-targets --all-features -- -D warnings" }
    )

    $success = Invoke-CheckScenario $powershellPath $checkPath $repositoryRoot $shimDirectory $logPath
    Assert-Equal 0 $success.ExitCode "all-success exit code"
    Assert-Invocations $success.Invocations $allExpected "all-success"

    $npmFailure = Invoke-CheckScenario `
        $powershellPath $checkPath $repositoryRoot $shimDirectory $logPath `
        -FailTool "npm" `
        -FailArgs "run test:run" `
        -FailCode 37 `
        -StdoutMarker "npm-test-stdout-marker" `
        -StderrMarker "npm-test-stderr-marker"
    Assert-Equal 37 $npmFailure.ExitCode "npm test:run failure exit code"
    Assert-Invocations $npmFailure.Invocations $allExpected[0..1] "npm test:run failure"
    Assert-True $npmFailure.Stdout.Contains("npm-test-stdout-marker") "npm stdout marker was not preserved"
    Assert-True $npmFailure.Stderr.Contains("npm-test-stderr-marker") "npm stderr marker was not preserved"

    $cargoFailure = Invoke-CheckScenario `
        $powershellPath $checkPath $repositoryRoot $shimDirectory $logPath `
        -FailTool "cargo" `
        -FailArgs "fmt --all -- --check" `
        -FailCode 41 `
        -StdoutMarker "cargo-fmt-stdout-marker" `
        -StderrMarker "cargo-fmt-stderr-marker"
    Assert-Equal 41 $cargoFailure.ExitCode "cargo fmt failure exit code"
    Assert-Invocations $cargoFailure.Invocations $allExpected[0..3] "cargo fmt failure"
    Assert-True $cargoFailure.Stdout.Contains("cargo-fmt-stdout-marker") "cargo stdout marker was not preserved"
    Assert-True $cargoFailure.Stderr.Contains("cargo-fmt-stderr-marker") "cargo stderr marker was not preserved"

    Write-Host "check.ps1 harness passed"
}
finally {
    $cleanupTarget = [System.IO.Path]::GetFullPath($tempRoot)
    if (-not $cleanupTarget.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean unsafe path: $cleanupTarget"
    }
    if (Test-Path -LiteralPath $cleanupTarget) {
        Remove-Item -LiteralPath $cleanupTarget -Recurse -Force
    }
}
