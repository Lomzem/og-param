param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$OutputDirectory,

    [Parameter(Position = 1)]
    [string]$OutputBasename = 'og-param'
)

$ErrorActionPreference = 'Stop'

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    throw 'docker is not installed or is not on PATH'
}

$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$outputPath = (Resolve-Path -LiteralPath $OutputDirectory).Path
$image = if ($env:OG_PARAM_ARTIFACT_IMAGE) {
    $env:OG_PARAM_ARTIFACT_IMAGE
} else {
    'og-param-artifacts:local'
}

if ($env:OG_PARAM_SKIP_DOCKER_BUILD -ne '1') {
    & docker build --target artifacts --tag $image $workspace
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

& docker run --rm `
    --network none `
    --mount "type=bind,source=$outputPath,target=/output" `
    $image $OutputBasename
exit $LASTEXITCODE
