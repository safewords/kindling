<#
.SYNOPSIS
    Build this project's iPXE on Windows, inside a container.

.DESCRIPTION
    The same build as ipxe/build.sh, for a Windows machine with Docker Desktop
    or Podman and no bash. It runs the container build in ipxe/Containerfile,
    which runs ipxe/build.sh --native inside it, so the compile itself is
    identical on every host. Outputs land in tftproot/ipxe-build/<arch>/;
    nothing in service is touched. `pxe:ipxe --install` puts a build in place.

.EXAMPLE
    .\ipxe\build.ps1 -EmbedBase http://10.0.1.109:8080

.EXAMPLE
    .\ipxe\build.ps1 -Arch x86_64-efi,bios -EmbedBase none
#>
[CmdletBinding()]
param(
    # bios, x86_64-efi, i386-efi, arm64-efi, or all.
    [string[]] $Arch = @('all'),
    # The server URL to bake in; `none` (or empty) finds it through DHCP.
    [string] $EmbedBase = $env:PXE_EMBED_BASE,
    [int] $EmbedPort = 8080,
    [int] $DhcpTries = 3,
    # docker or podman; the first one that is running when not given.
    [string] $Engine = $env:IPXE_ENGINE
)

$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$repo = Split-Path -Parent $here
$out = Join-Path $repo 'tftproot\ipxe-build'

function Test-EngineRunning([string] $name) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) { return $false }
    & $name info *> $null
    return $LASTEXITCODE -eq 0
}

if (-not $Engine) {
    foreach ($candidate in 'docker', 'podman') {
        if (Test-EngineRunning $candidate) { $Engine = $candidate; break }
    }
}
if (-not $Engine) {
    $installed = @('docker', 'podman') | Where-Object { Get-Command $_ -ErrorAction SilentlyContinue }
    if ($installed) {
        throw "Found $($installed -join ' and '), but no engine is running. Start Docker Desktop, or run ``podman machine start``, and try again."
    }
    throw 'Neither docker nor podman is installed. Install Docker Desktop or Podman Desktop and try again.'
}

if (-not $EmbedBase) { $EmbedBase = 'none' }
$archList = ($Arch -join ',')

New-Item -ItemType Directory -Force $out | Out-Null

# A container build that exports its own output, rather than a `run` with the
# repository mounted: Docker Desktop on Hyper-V refuses to mount a path that
# is not listed under File Sharing, and a build needs nothing mounted at all.
& $Engine build --platform linux/amd64 `
    -f (Join-Path $here 'Containerfile') `
    --target output `
    --build-arg "ARCHES=$archList" `
    --build-arg "EMBED_BASE=$EmbedBase" `
    --build-arg "EMBED_PORT=$EmbedPort" `
    --build-arg "DHCP_TRIES=$DhcpTries" `
    --build-arg "IPXE_MAKE_ARGS=$env:IPXE_MAKE_ARGS" `
    --output "type=local,dest=$out" `
    $here
exit $LASTEXITCODE
