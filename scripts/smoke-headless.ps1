# Windows adapter for the authoritative cross-platform headless smoke suite.
# Run from any directory after: npm run build:gateway

$ErrorActionPreference = "Stop"
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$gatewayBin = $env:TOOLPORT_GATEWAY_BIN
if ([string]::IsNullOrWhiteSpace($gatewayBin)) {
    $gatewayBin = Join-Path $repoRoot "src-tauri\target\debug\toolport-gateway.exe"
    if (-not (Test-Path $gatewayBin)) {
        $gatewayBin = Join-Path $repoRoot "src-tauri\target\release\toolport-gateway.exe"
    }
}
if (-not (Test-Path $gatewayBin)) {
    Write-Error "Build toolport-gateway first: npm run build:gateway"
}

$node = Get-Command node -ErrorAction Stop
$previousGatewayBin = $env:TOOLPORT_GATEWAY_BIN
$exitCode = 1

try {
    $env:TOOLPORT_GATEWAY_BIN = $gatewayBin
    & $node.Source (Join-Path $PSScriptRoot "smoke-headless.mjs")
    $exitCode = $LASTEXITCODE
} finally {
    if ($null -eq $previousGatewayBin) {
        Remove-Item Env:TOOLPORT_GATEWAY_BIN -ErrorAction SilentlyContinue
    } else {
        $env:TOOLPORT_GATEWAY_BIN = $previousGatewayBin
    }
}

exit $exitCode
