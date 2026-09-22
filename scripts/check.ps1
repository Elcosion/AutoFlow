$ErrorActionPreference = "Stop"

Write-Host "== Frontend typecheck =="
npm.cmd run typecheck
$exitCode = $LASTEXITCODE
if ($exitCode -ne 0) { exit $exitCode }
Write-Host "== Frontend tests =="
npm.cmd run test:run
$exitCode = $LASTEXITCODE
if ($exitCode -ne 0) { exit $exitCode }
Write-Host "== Frontend format check =="
npm.cmd run format:check
$exitCode = $LASTEXITCODE
if ($exitCode -ne 0) { exit $exitCode }

Push-Location (Join-Path $PSScriptRoot "..\src-tauri")
try {
    Write-Host "== Rust format check =="
    cargo fmt --all -- --check
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) { exit $exitCode }
    Write-Host "== Rust tests =="
    cargo test
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) { exit $exitCode }
    Write-Host "== Rust clippy =="
    cargo clippy --all-targets --all-features -- -D warnings
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) { exit $exitCode }
}
finally {
    Pop-Location
}
