# Finds a Windows kit tool (signtool, Inf2Cat) for the CI install scripts;
# dot-source it. The installed kits come first. The windows-2025 image ships
# the SDK (signtool) but no WDK, so Inf2Cat then comes from the NuGet WDK at
# the version driver/packages.config pins, restored into RUNNER_TEMP. CI only.

function Find-KitTool([string] $Name, [string] $Arch) {
    $installed = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $tool = Find-KitToolUnder $installed $Name $Arch
    if ($tool) { return $tool }
    $restored = Restore-NuGetWdk
    $tool = Find-KitToolUnder $restored $Name $Arch
    if (-not $tool) { throw "$Name ($Arch) not found under $installed or $restored" }
    $tool
}

function Find-KitToolUnder([string] $Root, [string] $Name, [string] $Arch) {
    Get-ChildItem -Path $Root -Recurse -Filter $Name -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq $Arch } |
        Sort-Object { $_.Directory.Parent.Name } -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

# The x64 WDK package alone (its SDK dependencies are not needed for the tools);
# returns the folder it was restored into.
function Restore-NuGetWdk {
    $packageId = 'Microsoft.Windows.WDK.x64'
    $config = [xml](Get-Content -Raw (Join-Path $PSScriptRoot '..\..\driver\packages.config'))
    $version = ($config.packages.package | Where-Object id -eq $packageId).version
    if (-not $version) { throw "driver/packages.config pins no $packageId" }
    $out = Join-Path $env:RUNNER_TEMP 'nuget-wdk'
    if (Test-Path (Join-Path $out "$packageId.$version")) { return $out }
    . (Join-Path $PSScriptRoot '..\vs-tools.ps1')
    $nuget = Get-NuGetExe $env:RUNNER_TEMP
    Write-Host "> nuget install $packageId $version (the image has no WDK)"
    & $nuget install $packageId -Version $version -OutputDirectory $out -DependencyVersion Ignore -NonInteractive | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "nuget install $packageId $version failed ($LASTEXITCODE)" }
    $out
}
