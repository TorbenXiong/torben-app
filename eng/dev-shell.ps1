[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$workspaceRoot = Split-Path -Parent $repositoryRoot
$toolRoot = Join-Path $workspaceRoot '.tools\torben-app'
$nodeRoot = Join-Path $toolRoot 'node\node-v24.19.0-win-x64'
$cargoRoot = Join-Path $toolRoot 'cargo'
$rustupRoot = Join-Path $toolRoot 'rustup'
$pnpmRoot = Join-Path $toolRoot 'pnpm'

foreach ($required in @(
    (Join-Path $nodeRoot 'node.exe'),
    (Join-Path $cargoRoot 'bin\cargo.exe'),
    (Join-Path $pnpmRoot 'pnpm.cmd')
)) {
    if (-not (Test-Path -LiteralPath $required)) {
        throw "Torben App local tool is missing: $required"
    }
}

$env:CARGO_HOME = $cargoRoot
$env:RUSTUP_HOME = $rustupRoot
$env:pnpm_config_store_dir = Join-Path $toolRoot 'pnpm-store'
$env:PATH = "$(Join-Path $cargoRoot 'bin');$pnpmRoot;$nodeRoot;$env:PATH"

Write-Output 'Torben App development tools are active for this PowerShell process.'
node --version
pnpm --version
rustc --version
cargo --version
