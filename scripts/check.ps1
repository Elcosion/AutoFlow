$ErrorActionPreference = "Stop"

Write-Host "== Frontend typecheck =="
npm.cmd run typecheck
Write-Host "== Frontend tests =="
npm.cmd run test:run
Write-Host "== Frontend format check =="
npm.cmd run format:check

Push-Location (Join-Path $PSScriptRoot "..\src-tauri")
try {
    Write-Host "== Rust format check =="
    cargo fmt --all -- --check
    Write-Host "== Rust tests =="
    cargo test
    Write-Host "== Rust clippy =="
    cargo clippy --all-targets --all-features -- -D warnings
}
finally {
    Pop-Location
}
