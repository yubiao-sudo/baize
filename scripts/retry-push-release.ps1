# Retry git push + GitHub Release creation until success (for blocked network).
# Usage:
#   .\retry-push-release.ps1
#   .\retry-push-release.ps1 -IntervalSec 30 -MaxAttempts 20
param(
    [int]$IntervalSec = 60,   # seconds between retries
    [int]$MaxAttempts = 0     # max attempts per phase, 0 = infinite
)

$ErrorActionPreference = "Continue"
$root = Split-Path $PSScriptRoot -Parent            # baize repo root
$releaseScript = Join-Path $PSScriptRoot "create-release.ps1"

function Ts { Get-Date -Format "yyyy-MM-dd HH:mm:ss" }

Write-Output "[$(Ts)] repo root: $root"
Write-Output "[$(Ts)] release script: $releaseScript"
Write-Output "[$(Ts)] interval: ${IntervalSec}s  max attempts per phase: $MaxAttempts"

# ---- Phase 1: push origin main ----
Write-Output "[$(Ts)] ===== Phase 1: git push origin main ====="
$attempt = 0
while ($MaxAttempts -eq 0 -or $attempt -lt $MaxAttempts) {
    $attempt++
    Write-Output "[$(Ts)] [push] attempt #$attempt"
    Push-Location $root
    git push origin main 2>&1 | Out-Host
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -eq 0) {
        Write-Output "[$(Ts)] [push] SUCCESS"
        break
    }
    Write-Output "[$(Ts)] [push] failed (exit=$code), retry in ${IntervalSec}s..."
    Start-Sleep -Seconds $IntervalSec
}

# ---- Phase 2: create GitHub Release + upload installer ----
Write-Output "[$(Ts)] ===== Phase 2: create GitHub Release ====="
$attempt = 0
while ($MaxAttempts -eq 0 -or $attempt -lt $MaxAttempts) {
    $attempt++
    Write-Output "[$(Ts)] [release] attempt #$attempt"
    & powershell -NoProfile -ExecutionPolicy Bypass -File $releaseScript
    $code = $LASTEXITCODE
    if ($code -eq 0) {
        Write-Output "[$(Ts)] [release] SUCCESS"
        break
    }
    Write-Output "[$(Ts)] [release] failed (exit=$code), retry in ${IntervalSec}s..."
    Start-Sleep -Seconds $IntervalSec
}

Write-Output "[$(Ts)] ===== all done, exiting ====="