# SPDX-License-Identifier: MS-PL
# Copyright (c) 2026 115dkk
# Licensed under the Microsoft Public License (MS-PL).

[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Release',

    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Invoke-Native {
    param(
        [Parameter(Mandatory)]
        [string]$FilePath,

        [Parameter(Mandatory)]
        [string[]]$ArgumentList
    )

    & $FilePath @ArgumentList
    if ($LASTEXITCODE -ne 0) {
        throw "Native command failed with exit code ${LASTEXITCODE}: $FilePath $($ArgumentList -join ' ')"
    }
}

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$driverRoot = Join-Path $repositoryRoot 'driver'
$projectRoot = Join-Path $driverRoot 'bioatmos'
$solutionPath = Join-Path $projectRoot 'bioatmos.sln'
$packagesConfig = Join-Path $driverRoot 'packages.config'
$packagesDirectory = Join-Path $driverRoot 'packages'
$targetTools = Join-Path $repositoryRoot 'target/tools'
$outputDirectory = Join-Path $repositoryRoot "target/driver/$Configuration/$Platform"

$nugetCommand = Get-Command nuget.exe -ErrorAction SilentlyContinue
if ($nugetCommand) {
    $nuget = $nugetCommand.Source
} else {
    New-Item -ItemType Directory -Force -Path $targetTools | Out-Null
    $nuget = Join-Path $targetTools 'nuget.exe'
    if (-not (Test-Path -LiteralPath $nuget -PathType Leaf)) {
        $nugetUri = 'https://dist.nuget.org/win-x86-commandline/v6.12.1/nuget.exe'
        Write-Host "Downloading NuGet command line client to $nuget"
        Invoke-WebRequest -Uri $nugetUri -OutFile $nuget -UseBasicParsing
    }
}

Invoke-Native -FilePath $nuget -ArgumentList @(
    'restore', $packagesConfig,
    '-PackagesDirectory', $packagesDirectory,
    '-NonInteractive'
)

$vswhereCommand = Get-Command vswhere.exe -ErrorAction SilentlyContinue
if ($vswhereCommand) {
    $vswhere = $vswhereCommand.Source
} else {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
}

$msbuildCommand = Get-Command msbuild.exe -ErrorAction SilentlyContinue
$msbuild = if ($msbuildCommand) { $msbuildCommand.Source } else { $null }

if (-not $msbuild -and (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
    $installationPath = (& $vswhere -latest -products * -requires Microsoft.Component.MSBuild Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $installationPath) {
        throw 'vswhere could not locate a Visual Studio installation containing MSBuild.'
    }

    $devShellModule = Join-Path $installationPath 'Common7/Tools/Microsoft.VisualStudio.DevShell.dll'
    if (Test-Path -LiteralPath $devShellModule -PathType Leaf) {
        Import-Module $devShellModule
        $hostArchitecture = if ([Environment]::Is64BitOperatingSystem) { 'amd64' } else { 'x86' }
        Enter-VsDevShell -VsInstallPath $installationPath -SkipAutomaticLocation -DevCmdArguments "-arch=$hostArchitecture -host_arch=$hostArchitecture"
    }

    $msbuildCandidate = Join-Path $installationPath 'MSBuild/Current/Bin/MSBuild.exe'
    if (Test-Path -LiteralPath $msbuildCandidate -PathType Leaf) {
        $msbuild = $msbuildCandidate
    }
}

if (-not $msbuild) {
    throw 'MSBuild was not found. Install Visual Studio Build Tools with the Desktop C++ workload, or put msbuild.exe on PATH.'
}

$stampDate = Get-Date -Format 'MM/dd/yyyy'
Invoke-Native -FilePath $msbuild -ArgumentList @(
    $solutionPath,
    '/m',
    '/restore:false',
    "/p:Configuration=$Configuration",
    "/p:Platform=$Platform",
    "/p:InfDateStamp=$stampDate",
    '/p:SignMode=Off',
    '/p:EnableInf2Cat=false',
    '/verbosity:minimal'
)

$buildDirectory = Join-Path $repositoryRoot "target/driver-build/$Configuration/$Platform"
$artifacts = @(
    Join-Path $buildDirectory 'bioatmos.sys'
    Join-Path $buildDirectory 'bioatmos.inf'
    Join-Path $buildDirectory 'bioatmos.pdb'
)

foreach ($artifact in $artifacts) {
    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
        throw "Expected build artifact was not produced: $artifact"
    }
}

$hostToolArchitecture = if ([Environment]::Is64BitOperatingSystem) { 'x64' } else { 'x86' }
$infverifCandidates = Get-ChildItem -LiteralPath $packagesDirectory -Filter 'infverif.exe' -File -Recurse |
    Where-Object {
        $_.FullName -match 'Microsoft\.Windows\.WDK\.' -and
        $_.FullName -match "[\\/]tools[\\/].*[\\/]$hostToolArchitecture[\\/]infverif\.exe$"
    } |
    Sort-Object FullName

if (-not $infverifCandidates) {
    throw "The $hostToolArchitecture host InfVerif.exe was not found beneath restored WDK packages at $packagesDirectory"
}

$infverif = $infverifCandidates | Select-Object -First 1
if (-not $infverif) {
    throw "InfVerif.exe was not found beneath restored WDK packages at $packagesDirectory"
}

Invoke-Native -FilePath $infverif.FullName -ArgumentList @('/w', '/v', (Join-Path $buildDirectory 'bioatmos.inf'))

New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
foreach ($artifact in $artifacts) {
    Copy-Item -LiteralPath $artifact -Destination $outputDirectory -Force
}

Write-Host "Driver artifacts copied to $outputDirectory"
