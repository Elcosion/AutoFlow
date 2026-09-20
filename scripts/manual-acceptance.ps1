[CmdletBinding()]
param(
    [ValidateSet('Validate', 'Install', 'OpenHarness')]
    [string]$Operation = 'Validate',

    [string]$Source,

    [string]$Destination
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$defaultSource = Join-Path $repoRoot 'tests\manual\macros'
$harnessPath = Join-Path $repoRoot 'tests\manual\runtime-safety-harness.html'

# These are handbook fixture limits. They are static safety bounds for this
# package, not a real-time SLA or a replacement for the runtime's validation.
$manualMaxWaitMs = 120000L
$manualMinPollMs = 50L
$manualMaxPollMs = 5000L
$manualMaxMacroSteps = 512
$manualMaxTextLength = 16384

if ([string]::IsNullOrWhiteSpace($Source)) {
    $Source = $defaultSource
}

$allowedRuleProperties = @(
    'id', 'name', 'enabled', 'triggerKeys', 'mode', 'repeatCount', 'speed',
    'recordMouseMove', 'recordMouseClicks', 'target', 'program'
)
$allowedRhaiCalls = @(
    'wait_ms', 'wait_random_ms', 'key_down', 'key_up', 'press', 'move_to',
    'mouse_down', 'mouse_up', 'click', 'scroll', 'type_text', 'is_cancelled',
    'active_window_title', 'window_exists', 'window_rect', 'wait_window',
    'pixel_matches', 'wait_pixel', 'find_image', 'wait_image', 'bio_move_to',
    'bio_click', 'bio_type_text', 'stop_with_message'
)

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Get-PropertyNames {
    param([object]$Object)
    return @($Object.PSObject.Properties.Name)
}

function Assert-Properties {
    param(
        [object]$Object,
        [string[]]$Allowed,
        [string[]]$Required,
        [string]$Context
    )
    $names = Get-PropertyNames $Object
    foreach ($name in $names) {
        Assert-Condition ($Allowed -contains $name) "$Context contains unsupported property '$name'"
    }
    foreach ($name in $Required) {
        Assert-Condition ($names -contains $name) "$Context is missing required property '$name'"
    }
}

function Assert-I32 {
    param([object]$Value, [string]$Context)
    $parsed = 0L
    Assert-Condition ([long]::TryParse([string]$Value, [ref]$parsed)) "$Context must be an integer"
    Assert-Condition ($parsed -ge [int]::MinValue -and $parsed -le [int]::MaxValue) "$Context is outside signed 32-bit bounds"
}

function Assert-ManualInteger {
    param([string]$Value, [string]$Context)
    Assert-Condition ($Value -match '^\s*-?\d+\s*$') "$Context must be a literal integer in a manual fixture"
    $parsed = 0L
    Assert-Condition ([long]::TryParse($Value.Trim(), [ref]$parsed)) "$Context is not a valid integer"
    return $parsed
}

function Assert-ManualWait {
    param([string]$Value, [string]$Context)
    $milliseconds = Assert-ManualInteger $Value $Context
    Assert-Condition ($milliseconds -ge 0 -and $milliseconds -le $manualMaxWaitMs) "$Context must be between 0 and $manualMaxWaitMs ms (manual fixture limit)"
    return $milliseconds
}

function Assert-ManualPoll {
    param([string]$Value, [string]$Context)
    $milliseconds = Assert-ManualInteger $Value $Context
    Assert-Condition ($milliseconds -ge $manualMinPollMs -and $milliseconds -le $manualMaxPollMs) "$Context must be between $manualMinPollMs and $manualMaxPollMs ms (manual fixture limit)"
    return $milliseconds
}

function Split-RhaiArguments {
    param([string]$ArgumentsText)
    $parts = [System.Collections.Generic.List[string]]::new()
    $builder = [Text.StringBuilder]::new()
    $quote = $false
    $escape = $false
    $braceDepth = 0
    $bracketDepth = 0
    for ($index = 0; $index -lt $ArgumentsText.Length; $index += 1) {
        $character = $ArgumentsText[$index]
        if ($quote) {
            [void]$builder.Append($character)
            if ($escape) {
                $escape = $false
            } elseif ($character -eq '\') {
                $escape = $true
            } elseif ($character -eq '"') {
                $quote = $false
            }
            continue
        }
        if ($character -eq '"') {
            $quote = $true
            [void]$builder.Append($character)
            continue
        }
        switch ($character) {
            '{' { $braceDepth += 1 }
            '}' { $braceDepth -= 1 }
            '[' { $bracketDepth += 1 }
            ']' { $bracketDepth -= 1 }
        }
        if ($character -eq ',' -and $braceDepth -eq 0 -and $bracketDepth -eq 0) {
            $parts.Add($builder.ToString().Trim())
            [void]$builder.Clear()
        } else {
            [void]$builder.Append($character)
        }
    }
    if ($builder.Length -gt 0 -or $ArgumentsText.Trim().Length -gt 0) {
        $parts.Add($builder.ToString().Trim())
    }
    return @($parts)
}

function Get-RhaiCalls {
    param([string]$SourceText)
    $calls = [System.Collections.Generic.List[object]]::new()
    $matches = [regex]::Matches($SourceText, '(?m)\b([A-Za-z_][A-Za-z0-9_]*)\s*\(')
    foreach ($match in $matches) {
        $open = $match.Index + $match.Length - 1
        $depth = 1
        $quote = $false
        $escape = $false
        $close = -1
        for ($index = $open + 1; $index -lt $SourceText.Length; $index += 1) {
            $character = $SourceText[$index]
            if ($quote) {
                if ($escape) {
                    $escape = $false
                } elseif ($character -eq '\') {
                    $escape = $true
                } elseif ($character -eq '"') {
                    $quote = $false
                }
                continue
            }
            if ($character -eq '"') {
                $quote = $true
            } elseif ($character -eq '(') {
                $depth += 1
            } elseif ($character -eq ')') {
                $depth -= 1
                if ($depth -eq 0) {
                    $close = $index
                    break
                }
            }
        }
        Assert-Condition ($close -ge 0) "Rhai call '$($match.Groups[1].Value)' has no closing parenthesis"
        $calls.Add([pscustomobject]@{
            Name = $match.Groups[1].Value
            Arguments = @(Split-RhaiArguments $SourceText.Substring($open + 1, $close - $open - 1))
            Index = $match.Index
        })
    }
    return @($calls | Sort-Object Index)
}

function Get-RhaiStringArgument {
    param([object]$Call, [int]$Index, [string]$Context)
    Assert-Condition ($Index -ge 0 -and $Index -lt $Call.Arguments.Count) "$Context is missing argument $Index"
    $argument = [string]$Call.Arguments[$Index]
    Assert-Condition ($argument -match '^\s*"(?:[^"\\]|\\.)*"\s*$') "$Context argument $Index must be a literal string"
    return $argument.Trim().Substring(1, $argument.Trim().Length - 2)
}

function Assert-RhaiPairs {
    param([object[]]$Calls, [string]$DownCall, [string]$UpCall, [string]$Context)
    $held = @{}
    $sawPairAction = $false
    foreach ($call in $Calls) {
        if ($call.Name -ne $DownCall -and $call.Name -ne $UpCall) { continue }
        $sawPairAction = $true
        $expectedArgumentCount = if ($DownCall -eq 'mouse_down') { 3 } else { 1 }
        Assert-Condition ($call.Arguments.Count -eq $expectedArgumentCount) "$Context $($call.Name) must have the expected literal key/button arguments"
        $key = Get-RhaiStringArgument $call 0 "$Context.$($call.Name)"
        $key = $key.ToLowerInvariant()
        if ($call.Name -eq $DownCall) {
            Assert-Condition (-not $held.ContainsKey($key)) "$Context presses '$key' twice before release"
            $held[$key] = $true
        } else {
            Assert-Condition ($held.ContainsKey($key)) "$Context releases '$key' without a preceding down"
            $held.Remove($key)
        }
    }
    Assert-Condition ($held.Count -eq 0) "$Context leaves a key/button held"
    if ($sawPairAction) {
        Write-Warning "${Context}: static down/up pairing cannot prove F12 cancellation cleanup; verify the physical cleanup at the assigned manual level."
    }
}

function Get-RhaiBlockEnd {
    param([string]$SourceText, [int]$OpenBrace)
    Assert-Condition ($OpenBrace -ge 0 -and $OpenBrace -lt $SourceText.Length -and $SourceText[$OpenBrace] -eq '{') 'Rhai guard does not start with a block brace'
    $depth = 0
    $quote = $false
    $escape = $false
    for ($index = $OpenBrace; $index -lt $SourceText.Length; $index += 1) {
        $character = $SourceText[$index]
        if ($quote) {
            if ($escape) {
                $escape = $false
            } elseif ($character -eq '\') {
                $escape = $true
            } elseif ($character -eq '"') {
                $quote = $false
            }
            continue
        }
        if ($character -eq '"') {
            $quote = $true
        } elseif ($character -eq '{') {
            $depth += 1
        } elseif ($character -eq '}') {
            $depth -= 1
            if ($depth -eq 0) {
                return $index
            }
        }
    }
    throw 'Rhai guard block has no closing brace'
}

function Assert-RhaiMouseCoordinates {
    param([object[]]$Calls, [string]$SourceText, [string]$Context)
    $mouseCalls = @($Calls | Where-Object { $_.Name -in @('move_to', 'mouse_down', 'mouse_up', 'click', 'scroll', 'bio_move_to', 'bio_click') })
    if ($mouseCalls.Count -eq 0) { return }

    $rectMatches = [regex]::Matches($SourceText, '(?m)\blet\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*window_rect\s*\(\s*"AutoFlow Manual Acceptance Harness"\s*\)\s*;')
    Assert-Condition ($rectMatches.Count -eq 1) "$Context mouse actions require exactly one let <name> = window_rect(\"AutoFlow Manual Acceptance Harness\") assignment"
    $rectVariable = $rectMatches[0].Groups[1].Value
    $rectEscaped = [regex]::Escape($rectVariable)
    $foundMatches = [regex]::Matches($SourceText, "(?m)\bif\s+$rectEscaped\.found\s*\{")
    Assert-Condition ($foundMatches.Count -eq 1) "$Context mouse actions require exactly one if $rectVariable.found { ... } guard"
    $guardOpen = $foundMatches[0].Index + $foundMatches[0].Length - 1
    $guardClose = Get-RhaiBlockEnd $SourceText $guardOpen
    Assert-Condition ($foundMatches[0].Index -gt $rectMatches[0].Index) "$Context must use window_rect before its found guard"

    $coordinateNames = @{}
    foreach ($call in $mouseCalls) {
        switch ($call.Name) {
            'move_to' { $usedIndexes = @(0, 1) }
            'mouse_down' { $usedIndexes = @(1, 2) }
            'mouse_up' { $usedIndexes = @(1, 2) }
            'click' { $usedIndexes = @(1, 2) }
            'bio_move_to' { $usedIndexes = @(0, 1) }
            'bio_click' { $usedIndexes = @(1, 2) }
            'scroll' { throw "$Context uses scroll; the manual package does not permit scrolling outside a separately approved fixture" }
        }
        foreach ($usedIndex in $usedIndexes) {
            Assert-Condition ($usedIndex -lt $call.Arguments.Count) "$Context $($call.Name) is missing a coordinate argument"
            $coordinate = ([string]$call.Arguments[$usedIndex]).Trim()
            Assert-Condition ($coordinate -match '^[A-Za-z_][A-Za-z0-9_]*$') "$Context $($call.Name) uses a non-variable or absolute coordinate '$coordinate'"
            $coordinateNames[$coordinate] = $true
        }
    }

    $allDeclarations = @{}
    # This deliberately supports only simple, semicolon-terminated declarations.
    # The same lexical pattern is used for discovery and removal so a second
    # declaration on the same line cannot evade the shadow/reassignment checks.
    $declarationPattern = '\blet\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^;\r\n]+)\s*;'
    foreach ($match in [regex]::Matches($SourceText, $declarationPattern)) {
        $variable = $match.Groups[1].Value
        $prefixIndex = $match.Index - 1
        while ($prefixIndex -ge 0 -and [char]::IsWhiteSpace($SourceText[$prefixIndex])) {
            $prefixIndex -= 1
        }
        $atStatementBoundary = ($prefixIndex -lt 0) -or ($SourceText[$prefixIndex] -in @(';', '{', '}'))
        Assert-Condition $atStatementBoundary "$Context let declaration '$variable' must start after a statement boundary; this constrained validator does not parse arbitrary Rhai"
        if (-not $allDeclarations.ContainsKey($variable)) { $allDeclarations[$variable] = @() }
        $allDeclarations[$variable] += [pscustomobject]@{
            Index = $match.Index
            Expression = $match.Groups[2].Value.Trim()
        }
    }

    Assert-Condition $allDeclarations.ContainsKey($rectVariable) "$Context window_rect variable '$rectVariable' was not discovered as a declaration"
    $rectDeclarations = @($allDeclarations[$rectVariable])
    Assert-Condition ($rectDeclarations.Count -eq 1) "$Context rect variable '$rectVariable' is redeclared or shadowed"
    Assert-Condition ($rectDeclarations[0].Index -eq $rectMatches[0].Index) "$Context rect variable '$rectVariable' declaration does not match its required window_rect assignment"
    Assert-Condition ($rectDeclarations[0].Expression -match '^\s*window_rect\s*\(\s*"AutoFlow Manual Acceptance Harness"\s*\)\s*$') "$Context rect variable '$rectVariable' must be assigned directly from window_rect"

    # Remove exactly the declarations discovered above before looking for
    # assignments. This keeps declaration coverage identical to discovery.
    $nonDeclarationSource = [regex]::Replace($SourceText, $declarationPattern, '')
    # Keep assignment matching distinct from equality and ordering operators.
    # The member/index form covers writes such as w.x = and w["x"] *=.
    $assignmentOperatorPattern = '(?<![=<>!])(?:\+=|-=|\*=|/=|=(?![=<>]))'
    $rectReassignmentPattern = "(?m)\b$rectEscaped\s*$assignmentOperatorPattern"
    Assert-Condition (-not [regex]::IsMatch($nonDeclarationSource, $rectReassignmentPattern)) "$Context rect variable '$rectVariable' is reassigned"
    $rectMemberAssignmentPattern = "(?m)\b$rectEscaped\s*(?:(?:\.\s*[A-Za-z_][A-Za-z0-9_]*)|(?:\[\s*[^\]\r\n]+\s*\]))+\s*$assignmentOperatorPattern"
    Assert-Condition (-not [regex]::IsMatch($nonDeclarationSource, $rectMemberAssignmentPattern)) "$Context rect variable '$rectVariable' property or index is reassigned"

    $coordinateDeclarations = @{}
    foreach ($variable in @($coordinateNames.Keys)) {
        Assert-Condition $allDeclarations.ContainsKey($variable) "$Context coordinate '$variable' is not declared"
        $declarations = @($allDeclarations[$variable])
        Assert-Condition ($declarations.Count -eq 1) "$Context coordinate '$variable' is redeclared or shadowed"
        $declaration = $declarations[0]
        Assert-Condition ($declaration.Index -gt $guardOpen -and $declaration.Index -lt $guardClose) "$Context coordinate declaration '$variable' must be inside if $rectVariable.found"
        Assert-Condition ($declaration.Expression -match "\b$rectEscaped\.(?:x|y|width|height)\b") "$Context coordinate '$variable' must be based directly on $rectVariable"
        $coordinateDeclarations[$variable] = $declaration
    }

    $xFormulaPatterns = @(
        "^\s*$rectEscaped\.x\s*\+\s*$rectEscaped\.width\s*/\s*2\s*$",
        "^\s*$rectEscaped\.x\s*\+\s*$rectEscaped\.width\s*\*\s*(45|50|55)\s*/\s*100\s*$"
    )
    $yFormulaPatterns = @(
        "^\s*$rectEscaped\.y\s*\+\s*$rectEscaped\.height\s*/\s*2\s*$",
        "^\s*$rectEscaped\.y\s*\+\s*$rectEscaped\.height\s*\*\s*50\s*/\s*100\s*$"
    )

    $coordinateInfo = @{}
    foreach ($variable in @($coordinateDeclarations.Keys)) {
        $declaration = $coordinateDeclarations[$variable]
        $expression = $declaration.Expression
        $axis = $null
        if ($expression -match ($xFormulaPatterns -join '|')) { $axis = 'x' }
        if ($expression -match ($yFormulaPatterns -join '|')) {
            Assert-Condition ($null -eq $axis) "$Context coordinate '$variable' has conflicting x/y formulas"
            $axis = 'y'
        }
        Assert-Condition ($null -ne $axis) "$Context coordinate '$variable' uses an unapproved formula; only center or 45%/50%/55% harness positions are allowed"
        $reassignmentPattern = "(?m)\b$([regex]::Escape($variable))\s*$assignmentOperatorPattern"
        Assert-Condition (-not [regex]::IsMatch($nonDeclarationSource, $reassignmentPattern)) "$Context coordinate '$variable' is reassigned"
        $coordinateInfo[$variable] = [pscustomobject]@{ Axis = $axis; Index = $declaration.Index }
    }

    foreach ($call in $mouseCalls) {
        Assert-Condition ($call.Index -gt $guardOpen -and $call.Index -lt $guardClose) "$Context mouse call '$($call.Name)' is outside the if $rectVariable.found block"
        $arguments = @($call.Arguments)
        $coordinateAxes = @{}
        switch ($call.Name) {
            'move_to' { $coordinateIndexes = @(0, 1); $coordinateAxes = @{ 0 = 'x'; 1 = 'y' }; $minimumCount = 2; $maximumCount = 2 }
            'mouse_down' { $coordinateIndexes = @(1, 2); $coordinateAxes = @{ 1 = 'x'; 2 = 'y' }; $minimumCount = 3; $maximumCount = 3 }
            'mouse_up' { $coordinateIndexes = @(1, 2); $coordinateAxes = @{ 1 = 'x'; 2 = 'y' }; $minimumCount = 3; $maximumCount = 3 }
            'click' { $coordinateIndexes = @(1, 2); $coordinateAxes = @{ 1 = 'x'; 2 = 'y' }; $minimumCount = 3; $maximumCount = 3 }
            'scroll' {
                throw "$Context uses scroll; the manual package does not permit scrolling outside a separately approved fixture"
            }
            'bio_move_to' { $coordinateIndexes = @(0, 1); $coordinateAxes = @{ 0 = 'x'; 1 = 'y' }; $minimumCount = 2; $maximumCount = 3 }
            'bio_click' { $coordinateIndexes = @(1, 2); $coordinateAxes = @{ 1 = 'x'; 2 = 'y' }; $minimumCount = 3; $maximumCount = 4 }
        }
        Assert-Condition ($arguments.Count -ge $minimumCount -and $arguments.Count -le $maximumCount) "$Context $($call.Name) must use explicit harness-derived coordinates"
        foreach ($coordinateIndex in $coordinateIndexes) {
            $coordinate = ([string]$arguments[$coordinateIndex]).Trim()
            Assert-Condition ($coordinate -match '^[A-Za-z_][A-Za-z0-9_]*$') "$Context $($call.Name) uses a non-variable or absolute coordinate '$coordinate'"
            Assert-Condition $coordinateInfo.ContainsKey($coordinate) "$Context coordinate '$coordinate' is not assigned from $rectVariable"
            Assert-Condition ($coordinateInfo[$coordinate].Index -lt $call.Index) "$Context coordinate '$coordinate' must be declared before its mouse call"
            Assert-Condition ($coordinateInfo[$coordinate].Axis -eq $coordinateAxes[$coordinateIndex]) "$Context coordinate '$coordinate' has the wrong axis formula"
        }
        if ($call.Name -in @('mouse_down', 'mouse_up', 'click', 'bio_click')) {
            $button = Get-RhaiStringArgument $call 0 "$Context.$($call.Name)"
            Assert-Condition ($button.ToLowerInvariant() -in @('left', 'right', 'middle', 'x1', 'x2')) "$Context has unsupported mouse button '$button'"
        }
    }
}

function Assert-RhaiCallBounds {
    param([object[]]$Calls, [string]$Context)
    foreach ($call in $Calls) {
        switch ($call.Name) {
            'wait_ms' {
                Assert-Condition ($call.Arguments.Count -eq 1) "$Context wait_ms requires one argument"
                [void](Assert-ManualWait $call.Arguments[0] "$Context wait_ms")
            }
            'wait_random_ms' {
                Assert-Condition ($call.Arguments.Count -eq 2) "$Context wait_random_ms requires two arguments"
                $minimum = Assert-ManualWait $call.Arguments[0] "$Context wait_random_ms minimum"
                $maximum = Assert-ManualWait $call.Arguments[1] "$Context wait_random_ms maximum"
                Assert-Condition ($maximum -ge $minimum) "$Context wait_random_ms maximum must not be below minimum"
            }
            'wait_window' {
                Assert-Condition ($call.Arguments.Count -eq 3) "$Context wait_window requires title, timeout and poll"
                [void](Get-RhaiStringArgument $call 0 "$Context.wait_window")
                [void](Assert-ManualWait $call.Arguments[1] "$Context wait_window timeout")
                [void](Assert-ManualPoll $call.Arguments[2] "$Context wait_window poll")
            }
            'wait_pixel' {
                Assert-Condition ($call.Arguments.Count -eq 8) "$Context wait_pixel requires eight arguments"
                [void](Assert-ManualWait $call.Arguments[6] "$Context wait_pixel timeout")
                [void](Assert-ManualPoll $call.Arguments[7] "$Context wait_pixel poll")
            }
            'find_image' {
                Assert-Condition ($call.Arguments.Count -in @(6, 7)) "$Context find_image has an unsupported argument count"
                [void](Get-RhaiStringArgument $call 0 "$Context.find_image")
                Assert-Condition ([string]$call.Arguments[5] -match '^\s*(?:0(?:\.\d+)?|1(?:\.0+)?)\s*$') "$Context find_image threshold must be a literal number from 0.0 to 1.0"
                if ($call.Arguments.Count -eq 7) {
                    Assert-Condition ([string]$call.Arguments[6] -match '^\s*#\{') "$Context find_image options must be a map"
                }
            }
            'wait_image' {
                Assert-Condition ($call.Arguments.Count -in @(8, 9)) "$Context wait_image has an unsupported argument count"
                [void](Get-RhaiStringArgument $call 0 "$Context.wait_image")
                Assert-Condition ([string]$call.Arguments[5] -match '^\s*(?:0(?:\.\d+)?|1(?:\.0+)?)\s*$') "$Context wait_image threshold must be a literal number from 0.0 to 1.0"
                [void](Assert-ManualWait $call.Arguments[6] "$Context wait_image timeout")
                [void](Assert-ManualPoll $call.Arguments[7] "$Context wait_image poll")
                if ($call.Arguments.Count -eq 9) {
                    Assert-Condition ([string]$call.Arguments[8] -match '^\s*#\{') "$Context wait_image options must be a map"
                }
            }
            'pixel_matches' {
                Assert-Condition ($call.Arguments.Count -eq 6) "$Context pixel_matches requires six arguments"
                [void](Assert-ManualInteger $call.Arguments[5] "$Context pixel_matches tolerance")
                $tolerance = [long]$call.Arguments[5]
                Assert-Condition ($tolerance -ge 0 -and $tolerance -le 255) "$Context pixel_matches tolerance must be 0..255"
            }
            'window_rect' {
                Assert-Condition ($call.Arguments.Count -eq 1) "$Context window_rect requires one title"
                $title = Get-RhaiStringArgument $call 0 "$Context.window_rect"
                Assert-Condition ($title -ceq 'AutoFlow Manual Acceptance Harness') "$Context window_rect may only target the acceptance harness"
            }
            default { }
        }
    }
}

function Test-RhaiProgram {
    param([object]$Program, [string]$Context)
    Assert-Properties $Program @('kind', 'source', 'apiVersion') @('kind', 'source', 'apiVersion') $Context
    Assert-Condition ($Program.kind -ceq 'rhai') "$Context kind must be 'rhai'"
    Assert-Condition ([int]$Program.apiVersion -eq 1) "$Context apiVersion must be 1"
    $sourceText = [string]$Program.source
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($sourceText)) "$Context source must not be empty"
    Assert-Condition ($sourceText.Length -le 1048576) "$Context source exceeds 1 MiB"
    Assert-Condition ($sourceText -notmatch '(?i)\bF1[02]\b') "$Context must not contain F10 or F12"
    Assert-Condition ($sourceText -notmatch '(?i)([A-Z]:[\\/]|\\\\|/Users/|/home/|%USERPROFILE%|\$env:|\\Users\\|AppData|ProgramData)') "$Context contains an absolute or user-specific path"
    Assert-Condition ($sourceText -notmatch '(?i)\b(import|eval|exec|spawn|shell|process|http|https|network|module)\b') "$Context contains a forbidden capability token"

    $calls = @(Get-RhaiCalls $sourceText)
    foreach ($call in $calls) {
        Assert-Condition ($allowedRhaiCalls -contains $call.Name) "$Context calls non-whitelisted function '$($call.Name)'"
    }
    foreach ($match in [regex]::Matches($sourceText, '(?<![A-Za-z0-9_.])-?\d+(?![A-Za-z0-9_.])')) {
        $number = 0L
        Assert-Condition ([long]::TryParse($match.Value, [ref]$number)) "$Context contains an invalid integer literal"
        Assert-Condition ($number -ge [int]::MinValue -and $number -le [int]::MaxValue) "$Context contains an integer outside signed 32-bit bounds"
    }

    Assert-RhaiCallBounds $calls $Context
    Assert-RhaiMouseCoordinates $calls $sourceText $Context
    Assert-RhaiPairs $calls 'key_down' 'key_up' $Context
    Assert-RhaiPairs $calls 'mouse_down' 'mouse_up' $Context

    $visionCalls = @($calls | Where-Object { $_.Name -in @('find_image', 'wait_image', 'pixel_matches', 'wait_pixel') })
    if ($visionCalls.Count -gt 0) {
        Assert-Condition (@($calls | Where-Object { $_.Name -eq 'window_rect' }).Count -eq 1) "$Context vision calls must use one acceptance-harness window_rect"
        Assert-Condition (@($calls | Where-Object { $_.Name -eq 'stop_with_message' }).Count -gt 0) "$Context vision fixture must report an explicit stop_with_message outcome"
        $visionInputCalls = @($calls | Where-Object { $_.Name -in @('key_down', 'key_up', 'press', 'move_to', 'mouse_down', 'mouse_up', 'click', 'scroll', 'type_text', 'bio_move_to', 'bio_click', 'bio_type_text') })
        Assert-Condition ($visionInputCalls.Count -eq 0) "$Context vision fixture must remain observer-only with no downstream input calls"
    }
}

function Test-MacroStep {
    param([object]$Step, [string]$Context)
    $names = Get-PropertyNames $Step
    Assert-Condition ($names -contains 'type') "$Context is missing type"
    switch ([string]$Step.type) {
        'delay' {
            Assert-Properties $Step @('type', 'durationMs', 'durationMaxMs') @('type', 'durationMs') $Context
            $duration = 0L
            Assert-Condition ([long]::TryParse([string]$Step.durationMs, [ref]$duration)) "$Context durationMs must be an integer"
            Assert-Condition ($duration -ge 0 -and $duration -le $manualMaxWaitMs) "$Context durationMs must be between 0 and $manualMaxWaitMs"
            if ($names -contains 'durationMaxMs') {
                $maximum = 0L
                Assert-Condition ([long]::TryParse([string]$Step.durationMaxMs, [ref]$maximum)) "$Context durationMaxMs must be an integer"
                Assert-Condition ($maximum -ge $duration -and $maximum -le $manualMaxWaitMs) "$Context durationMaxMs is outside the accepted manual-fixture range"
            }
        }
        'key' {
            Assert-Properties $Step @('type', 'key', 'action') @('type', 'key', 'action') $Context
            Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$Step.key)) "$Context key must not be empty"
            Assert-Condition (@('down', 'up') -contains [string]$Step.action) "$Context key action must be down or up"
            Assert-Condition ([string]$Step.key -notmatch '(?i)^F1[02]$') "$Context must not use F10 or F12"
        }
        'mouseButton' {
            Assert-Properties $Step @('type', 'button', 'action', 'x', 'y') @('type', 'button', 'action', 'x', 'y') $Context
            Assert-Condition (@('left', 'right', 'middle', 'x1', 'x2') -contains [string]$Step.button) "$Context has unsupported mouse button"
            Assert-Condition (@('down', 'up') -contains [string]$Step.action) "$Context mouse action must be down or up"
            Assert-I32 $Step.x "$Context.x"
            Assert-I32 $Step.y "$Context.y"
            throw "$Context uses fixed graphical mouse coordinates; manual fixtures must use Rhai window_rect"
        }
        'mouseMove' {
            Assert-Properties $Step @('type', 'x', 'y') @('type', 'x', 'y') $Context
            Assert-I32 $Step.x "$Context.x"
            Assert-I32 $Step.y "$Context.y"
            throw "$Context uses fixed graphical mouse coordinates; manual fixtures must use Rhai window_rect"
        }
        'wheel' {
            Assert-Properties $Step @('type', 'deltaX', 'deltaY') @('type', 'deltaX', 'deltaY') $Context
            Assert-I32 $Step.deltaX "$Context.deltaX"
            Assert-I32 $Step.deltaY "$Context.deltaY"
        }
        'text' {
            Assert-Properties $Step @('type', 'text') @('type', 'text') $Context
            $text = [string]$Step.text
            Assert-Condition (-not [string]::IsNullOrEmpty($text) -and $text.Length -le $manualMaxTextLength) "$Context text must contain 1..$manualMaxTextLength characters"
        }
        default { throw "$Context has unsupported step type '$($Step.type)'" }
    }
}

function Test-MacroProgram {
    param([object]$Program, [string]$Context)
    Assert-Properties $Program @('kind', 'steps') @('kind', 'steps') $Context
    Assert-Condition ($Program.kind -ceq 'macro') "$Context kind must be 'macro'"
    $steps = @($Program.steps)
    Assert-Condition ($steps.Count -gt 0 -and $steps.Count -le $manualMaxMacroSteps) "$Context must contain 1..$manualMaxMacroSteps steps"
    $pressedKeys = @{}
    $pressedButtons = @{}
    for ($index = 0; $index -lt $steps.Count; $index += 1) {
        $step = $steps[$index]
        Test-MacroStep $step "$Context.steps[$index]"
        if ([string]$step.type -ceq 'key') {
            $key = ([string]$step.key).ToLowerInvariant()
            if ([string]$step.action -ceq 'down') {
                Assert-Condition (-not $pressedKeys.ContainsKey($key)) "$Context presses key '$key' twice"
                $pressedKeys[$key] = $true
            } else {
                Assert-Condition ($pressedKeys.ContainsKey($key)) "$Context releases key '$key' without a matching down"
                $pressedKeys.Remove($key)
            }
        }
        if ([string]$step.type -ceq 'mouseButton') {
            $button = ([string]$step.button).ToLowerInvariant()
            if ([string]$step.action -ceq 'down') {
                Assert-Condition (-not $pressedButtons.ContainsKey($button)) "$Context presses mouse '$button' twice"
                $pressedButtons[$button] = $true
            } else {
                Assert-Condition ($pressedButtons.ContainsKey($button)) "$Context releases mouse '$button' without a matching down"
                $pressedButtons.Remove($button)
            }
        }
    }
    Assert-Condition ($pressedKeys.Count -eq 0) "$Context leaves a key held"
    Assert-Condition ($pressedButtons.Count -eq 0) "$Context leaves a mouse button held"
}

function Test-Fixtures {
    param([string]$FixtureDirectory)
    $resolvedSource = [IO.Path]::GetFullPath($FixtureDirectory)
    Assert-Condition (Test-Path -LiteralPath $resolvedSource -PathType Container) "Fixture directory does not exist: $resolvedSource"
    $files = @(Get-ChildItem -LiteralPath $resolvedSource -File -Filter '*.json' | Sort-Object Name)
    Assert-Condition ($files.Count -gt 0) "No JSON fixtures found in $resolvedSource"
    $ids = @{}
    $names = @{}

    foreach ($file in $files) {
        try {
            $raw = Get-Content -LiteralPath $file.FullName -Raw -Encoding UTF8
            $rule = $raw | ConvertFrom-Json
        } catch {
            throw "$($file.Name) is not valid JSON: $($_.Exception.Message)"
        }
        $context = $file.Name
        Assert-Condition ($raw -notmatch '(?i)([A-Z]:[\\/]|\\\\|/Users/|/home/|%USERPROFILE%|\$env:|\\Users\\|AppData|ProgramData)') "$context contains an absolute or user-specific path"
        # Scenario names may mention the physical emergency key; only the
        # executable program payload is forbidden from using it.
        Assert-Properties $rule $allowedRuleProperties @('id', 'name', 'enabled', 'triggerKeys', 'mode', 'program') $context
        Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$rule.id)) "$context id must not be empty"
        Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$rule.name)) "$context name must not be empty"
        Assert-Condition ([string]$rule.name -match '(?i)MANUAL_TEST|人工验收') "$context name must contain MANUAL_TEST or 人工验收"
        Assert-Condition (-not $ids.ContainsKey([string]$rule.id)) "$context duplicates id '$($rule.id)'"
        Assert-Condition (-not $names.ContainsKey([string]$rule.name)) "$context duplicates name '$($rule.name)'"
        $ids[[string]$rule.id] = $true
        $names[[string]$rule.name] = $true
        Assert-Condition ($rule.enabled -is [bool] -and -not $rule.enabled) "$context must be disabled"
        Assert-Condition (@($rule.triggerKeys).Count -eq 0) "$context triggerKeys must be empty"
        Assert-Condition ([string]$rule.mode -ceq 'once') "$context mode must be once"
        Assert-Condition (-not ((Get-PropertyNames $rule) -contains 'behaviorPolicy')) "$context must not contain behaviorPolicy"
        $programJson = $rule.program | ConvertTo-Json -Depth 100 -Compress
        Assert-Condition ($programJson -notmatch '(?i)\bF1[02]\b') "$context program must not use F10 or F12"
        if ([string]$rule.program.kind -ceq 'macro') {
            Test-MacroProgram $rule.program "$context.program"
        } elseif ([string]$rule.program.kind -ceq 'rhai') {
            Test-RhaiProgram $rule.program "$context.program"
        } else {
            throw "$context program.kind must be macro or rhai"
        }
        Write-Host "VALID $($file.Name)" -ForegroundColor Green
    }
    Write-Host "Validated $($files.Count) disabled manual fixture(s); no application was launched and no input was sent." -ForegroundColor Cyan
    return $files
}

function Resolve-HarnessForOpen {
    $resolvedHarness = [IO.Path]::GetFullPath($harnessPath)
    Assert-Condition (Test-Path -LiteralPath $resolvedHarness -PathType Leaf) "Harness file does not exist: $resolvedHarness"
    Assert-Condition ([IO.Path]::GetFileName($resolvedHarness) -ieq 'runtime-safety-harness.html') 'OpenHarness may only open runtime-safety-harness.html.'
    $expectedParent = [IO.Path]::GetFullPath((Join-Path $repoRoot 'tests\manual'))
    Assert-Condition ([IO.Path]::GetDirectoryName($resolvedHarness) -ieq $expectedParent) 'OpenHarness may only open the repository harness file.'
    return $resolvedHarness
}

# OpenHarness is intentionally the only operation that can launch anything,
# and it can launch only the repository-local HTML harness after an explicit
# operator request. Validate and Install return without starting a process.
if ($Operation -ceq 'OpenHarness') {
    $resolvedHarness = Resolve-HarnessForOpen
    Write-Host "Opening local offline harness: $resolvedHarness" -ForegroundColor Cyan
    Start-Process -FilePath $resolvedHarness
    return
}

$validatedFiles = @(Test-Fixtures $Source)
if ($Operation -ceq 'Validate') {
    return
}

Assert-Condition (-not [string]::IsNullOrWhiteSpace($Destination)) 'Install requires an explicit -Destination path selected from AutoFlow.'
$resolvedDestination = [IO.Path]::GetFullPath($Destination)
Assert-Condition (Test-Path -LiteralPath $resolvedDestination -PathType Container) "Install destination must already exist: $resolvedDestination"
$destinationInfo = Get-Item -LiteralPath $resolvedDestination
Assert-Condition ($destinationInfo.Name -ieq 'scripts') 'Install destination leaf must be scripts.'
Assert-Condition ($null -ne $destinationInfo.Parent -and $destinationInfo.Parent.Name -ieq 'data') 'Install destination parent must be data.'
Assert-Condition ($resolvedDestination -cne ([IO.Path]::GetPathRoot($resolvedDestination))) 'Refusing to install into a drive root.'

$collisions = @($validatedFiles | Where-Object { Test-Path -LiteralPath (Join-Path $resolvedDestination $_.Name) -PathType Leaf })
if ($collisions.Count -gt 0) {
    $backupRoot = Join-Path ([IO.Path]::GetTempPath()) ("AutoFlow-manual-acceptance-backup-" + [guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($backupRoot) | Out-Null
    foreach ($file in $collisions) {
        Copy-Item -LiteralPath (Join-Path $resolvedDestination $file.Name) -Destination (Join-Path $backupRoot $file.Name)
    }
    throw "Install refused to overwrite $($collisions.Count) existing file(s). Backups were copied to $backupRoot. Remove or rename collisions deliberately, then rerun."
}

foreach ($file in $validatedFiles) {
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $resolvedDestination $file.Name)
}
Write-Host "Installed $($validatedFiles.Count) disabled fixture(s) into the explicitly selected $resolvedDestination." -ForegroundColor Cyan
Write-Host 'AutoFlow was not launched and no keyboard or mouse input was sent.' -ForegroundColor Cyan
