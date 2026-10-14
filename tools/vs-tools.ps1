# Finds Visual Studio and the NuGet command line client for the build and CI
# scripts; dot-source it. Each tool comes from PATH first; NuGet otherwise is
# the pinned client, downloaded once into the folder the caller names.

$NuGetUri = 'https://dist.nuget.org/win-x86-commandline/v6.12.1/nuget.exe'

function Get-NuGetExe([string] $Directory) {
    $command = Get-Command nuget.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    New-Item -ItemType Directory -Force -Path $Directory | Out-Null
    $nuget = Join-Path $Directory 'nuget.exe'
    if (-not (Test-Path -LiteralPath $nuget -PathType Leaf)) {
        Write-Host "Downloading NuGet command line client to $nuget"
        Invoke-WebRequest -Uri $NuGetUri -OutFile $nuget -UseBasicParsing
    }
    $nuget
}

function Get-VsWhere {
    $command = Get-Command vswhere.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
}

# The newest Visual Studio that has every component in $Requires; throws when
# there is none, naming the components.
function Get-VsInstallPath([string[]] $Requires) {
    $vswhere = Get-VsWhere
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        throw "vswhere was not found at $vswhere"
    }
    $path = & $vswhere -latest -products * -requires $Requires -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $path) {
        throw "vswhere found no Visual Studio with $($Requires -join ', ')"
    }
    "$path".Trim()
}
