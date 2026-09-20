[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$validator = Join-Path $repoRoot 'scripts\manual-acceptance.ps1'
$canonicalFixture = Join-Path $repoRoot 'tests\manual\macros\05-short-move-in-harness.json'
$harnessPath = Join-Path $repoRoot 'tests\manual\runtime-safety-harness.html'
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$tempRoot = [IO.Path]::GetFullPath((Join-Path $tempBase ("autoflow-manual-validator-tests-" + [guid]::NewGuid().ToString('N'))))

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Invoke-Validate {
    param([string]$FixtureDirectory)
    $ErrorActionPreference = 'Continue'
    $output = & powershell -NoProfile -ExecutionPolicy Bypass -File $validator `
        -Operation Validate -Source $FixtureDirectory 2>&1 | Out-String
    [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = $output }
}

try {
    $harness = Get-Content -LiteralPath $harnessPath -Raw -Encoding UTF8
    Assert-Condition ($harness -match '<title>AutoFlow Manual Acceptance Harness</title>') 'Harness title changed'
    Assert-Condition ($harness -match '#vision-target\[hidden\]\s*\{\s*display:\s*none;\s*\}') 'Harness hidden target rule is missing'
    Assert-Condition ($harness -match 'safe-zone-panel' -and $harness -match '45%' -and $harness -match '55%' -and $harness -match 'overflow:\s*hidden') 'Harness safe geometry guard is missing'
    Assert-Condition ($harness -notmatch '(?i)fetch|XMLHttpRequest|WebSocket|dispatchEvent|SendInput|https?://') 'Harness contains a network or synthetic-input capability'
    Write-Host 'PASS harness static safety checks' -ForegroundColor Green

    [IO.Directory]::CreateDirectory($tempRoot) | Out-Null

    $canonicalDirectory = Join-Path $tempRoot 'canonical'
    [IO.Directory]::CreateDirectory($canonicalDirectory) | Out-Null
    Copy-Item -LiteralPath $canonicalFixture -Destination (Join-Path $canonicalDirectory '05-short-move-in-harness.json')
    $canonicalResult = Invoke-Validate $canonicalDirectory
    Assert-Condition ($canonicalResult.ExitCode -eq 0) "Canonical Validate failed:`n$($canonicalResult.Output)"
    Write-Host 'PASS canonical fixture Validate' -ForegroundColor Green

    $cases = @(
        [pscustomobject]@{
            Name = 'action-outside-found-guard'
            ExpectedPattern = 'outside the if w\.found block'
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
if w.found {
    let x = w.x + w.width / 2;
    let y = w.y + w.height / 2;
}
move_to(x, y);
"@
        },
        [pscustomobject]@{
            Name = 'huge-harness-offset'
            ExpectedPattern = 'uses an unapproved formula'
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
if w.found {
    let x = w.x + w.width * 500 / 100;
    let y = w.y + w.height / 2;
    move_to(x, y);
}
"@
        },
        [pscustomobject]@{
            Name = 'coordinate-reassignment'
            ExpectedPattern = "coordinate 'x' is reassigned"
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
if w.found {
    let x = w.x + w.width * 45 / 100;
    let y = w.y + w.height * 50 / 100;
    x = x + 1;
    move_to(x, y);
}
"@
        },
        [pscustomobject]@{
            Name = 'same-line-coordinate-shadowing'
            ExpectedPattern = "coordinate 'x' is redeclared or shadowed"
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
if w.found {
    let x = w.x + w.width * 45 / 100; let x = w.x + w.width * 55 / 100;
    let y = w.y + w.height * 50 / 100;
    move_to(x, y);
}
"@
        },
        [pscustomobject]@{
            Name = 'rect-variable-reassignment'
            ExpectedPattern = "rect variable 'w' is reassigned"
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
w = w;
if w.found {
    let x = w.x + w.width * 45 / 100;
    let y = w.y + w.height * 50 / 100;
    move_to(x, y);
}
"@
        },
        [pscustomobject]@{
            Name = 'rect-variable-shadowing'
            ExpectedPattern = "rect variable 'w' is redeclared or shadowed"
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
if w.found {
    let x = w.x + w.width * 45 / 100; let w = w;
    let y = w.y + w.height * 50 / 100;
    move_to(x, y);
}
"@
        },
        [pscustomobject]@{
            Name = 'rect-property-reassignment'
            ExpectedPattern = "rect variable 'w' property or index is reassigned"
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
if w.found {
    let x = w.x + w.width * 45 / 100;
    let y = w.y + w.height * 50 / 100;
    w.x = w.x;
    move_to(x, y);
}
"@
        },
        [pscustomobject]@{
            Name = 'rect-index-reassignment'
            ExpectedPattern = "rect variable 'w' property or index is reassigned"
            Source = @"
let w = window_rect("AutoFlow Manual Acceptance Harness");
if w.found {
    let x = w.x + w.width * 45 / 100;
    let y = w.y + w.height * 50 / 100;
    w["x"] *= 1;
    move_to(x, y);
}
"@
        }
    )

    foreach ($testCase in $cases) {
        $caseDirectory = Join-Path $tempRoot $testCase.Name
        [IO.Directory]::CreateDirectory($caseDirectory) | Out-Null
        $fixture = Get-Content -LiteralPath $canonicalFixture -Raw -Encoding UTF8 | ConvertFrom-Json
        $fixture.id = "manual-acceptance-negative-$($testCase.Name)"
        $fixture.name = "MANUAL_TEST negative - $($testCase.Name)"
        $fixture.program.source = $testCase.Source.Trim()
        $fixturePath = Join-Path $caseDirectory "$($testCase.Name).json"
        $fixture | ConvertTo-Json -Depth 100 | Set-Content -LiteralPath $fixturePath -Encoding UTF8

        $result = Invoke-Validate $caseDirectory
        Assert-Condition ($result.ExitCode -ne 0) "Negative case '$($testCase.Name)' unexpectedly passed Validate. Output:`n$($result.Output)"
        Assert-Condition ($result.Output -match $testCase.ExpectedPattern) "Negative case '$($testCase.Name)' was rejected for an unexpected reason; expected /$($testCase.ExpectedPattern)/. Output:`n$($result.Output)"
        Write-Host "PASS rejected $($testCase.Name)" -ForegroundColor Green
    }

    Write-Host 'Manual acceptance validator regression tests passed; only Validate was invoked.' -ForegroundColor Cyan
} finally {
    # Recursive deletion is allowed only for the exact test root created above.
    # Resolve both paths and refuse cleanup if the root is not a strict child of
    # the system temp directory with the expected generated leaf name.
    $resolvedTempBase = [IO.Path]::GetFullPath($tempBase).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $resolvedTempRoot = [IO.Path]::GetFullPath($tempRoot).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $tempLeaf = [IO.Path]::GetFileName($resolvedTempRoot)
    $tempParent = [IO.Path]::GetDirectoryName($resolvedTempRoot)
    if ([string]::IsNullOrWhiteSpace($tempParent) -or [string]::IsNullOrWhiteSpace($tempLeaf) -or
        $resolvedTempRoot -ieq $resolvedTempBase -or $tempParent.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) -ine $resolvedTempBase -or
        $tempLeaf -notmatch '^autoflow-manual-validator-tests-[0-9a-f]{32}$') {
        throw "Refusing recursive cleanup: temporary test root '$resolvedTempRoot' is not a strict, validated child of '$resolvedTempBase'."
    }
    if (Test-Path -LiteralPath $resolvedTempRoot -PathType Container) {
        Remove-Item -LiteralPath $resolvedTempRoot -Recurse -Force
    }
}
