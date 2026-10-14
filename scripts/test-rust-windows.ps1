param(
  [switch]$NoDefaultFeatures,
  [switch]$BuildOnly,
  [ValidateSet("all", "core", "integration")]
  [string]$TestGroup = "all"
)

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$gateway = Join-Path $repoRoot "src-tauri\target\debug\toolport-gateway.exe"
$resolvedGateway = if (Test-Path $gateway) { (Resolve-Path $gateway).Path } else { $null }

if ($resolvedGateway) {
  Get-Process toolport-gateway -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and ((Resolve-Path $_.Path).Path -eq $resolvedGateway) } |
    ForEach-Object {
      Write-Host "Stopping debug gateway process $($_.Id) so cargo can rebuild $resolvedGateway"
      Stop-Process -Id $_.Id -Force
    }
}

$cargoArgs = @(
  "test",
  "--features", "test-support,search-static",
  "--manifest-path", (Join-Path $repoRoot "src-tauri\Cargo.toml")
)
switch ($TestGroup) {
  "core" { $cargoArgs += @("--lib", "--bins") }
  "integration" { $cargoArgs += @("--test", "*") }
  "all" { $cargoArgs += @("--lib", "--bins", "--tests") }
}
if ($NoDefaultFeatures) { $cargoArgs += "--no-default-features" }

if ($BuildOnly) { $cargoArgs += @("--no-run", "--timings") }

& cargo @cargoArgs
exit $LASTEXITCODE
