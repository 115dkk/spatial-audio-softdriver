#Requires -RunAsAdministrator
# Builds the distributable test kit, installs through the same script used on a
# test-signing PC, then checks CI-only service defaults and emits job outputs.
# Problem code 52 (image refused) and an endpoint-count result are accepted here;
# later CI steps decide whether loaded endpoints work.
param(
    # Folder with bioatmos.sys and bioatmos.inf (the build job's artifact).
    [Parameter(Mandatory)] [string] $Package
)
$ErrorActionPreference = 'Stop'
$HardwareId = 'Root\BioAtmos'
$Service = 'bioatmos'

$version = "ci-$env:GITHUB_RUN_ID"
if (-not $env:GITHUB_RUN_ID) { $version = 'ci-local' }
$out = Join-Path $env:RUNNER_TEMP 'kit'
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $out | Out-Null
& (Join-Path $PSScriptRoot 'driver-test-kit.ps1') -Package $Package -Version $version -OutDir $out
if (-not $?) { throw 'driver-test-kit.ps1 failed' }
$zip = Join-Path $out "bio-atmos-driver-test-$version-x64.zip"
$expanded = Join-Path $out 'expanded'
Expand-Archive -Path $zip -DestinationPath $expanded -Force
$kit = Join-Path $expanded "bio-atmos-driver-test-$version-x64"
# Before installing, so the collect and uninstall steps run even when the
# install fails.
if ($env:GITHUB_OUTPUT) { Add-Content -Path $env:GITHUB_OUTPUT -Value "kit=$kit" }

& (Join-Path $kit 'install.ps1') -AllowWithoutTestSigning
$installExit = $LASTEXITCODE
if ($installExit -notin @(0, 3, 4)) { throw "test-kit install.ps1 failed ($installExit)" }
$result = Get-Content -Raw (Join-Path $kit 'install-result.json') | ConvertFrom-Json
$loaded = [bool]$result.loaded
$device = Get-PnpDevice | Where-Object { $_.HardwareID -contains $HardwareId } | Select-Object -First 1
if (-not $device) { throw "no device with hardware id $HardwareId was created" }
$problem = [int](Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode').Data
$driver = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data
$boot = (bcdedit /enum '{current}' | Select-String -Pattern 'testsigning|nointegritychecks|bootdebug|debug ') -join '; '
$serviceState = (Get-CimInstance Win32_SystemDriver -Filter "Name='$Service'").State

$serviceKey = "HKLM:\SYSTEM\CurrentControlSet\Services\$Service"
$image = (Get-ItemProperty $serviceKey).ImagePath
if ($image -notmatch 'bioatmos\.sys$') { throw "service ImagePath is '$image'" }
$parameters = Get-ItemProperty "$serviceKey\Parameters"
$expected = @{ SecondEndpoint = 1; FormatMask0 = 0x6F; FormatMask1 = 0x6F; WritePacket = 0 }
foreach ($name in $expected.Keys) {
    if ($parameters.$name -ne $expected[$name]) {
        throw "Parameters\$name is '$($parameters.$name)', expected $($expected[$name])"
    }
}
$kinds = @{}
$key = Get-Item "$serviceKey\Parameters"
foreach ($name in $expected.Keys) { $kinds[$name] = $key.GetValueKind($name) }
if ($kinds.FormatMask0 -ne 'QWord' -or $kinds.FormatMask1 -ne 'QWord') {
    throw "format masks are $($kinds.FormatMask0)/$($kinds.FormatMask1), expected QWord"
}

Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value @"
## Driver package (test-signed, x64)

| step | result |
| --- | --- |
| test kit sign, catalog and verify | ok |
| driver store and root device | $($device.InstanceId), driver $driver |
| kernel load | $(if ($loaded) { "loaded (service $serviceState)" } else { 'refused, problem code 52' }); boot entry: $(if ($boot) { $boot } else { 'default' }) |
| install script exit | $installExit |
| service ImagePath | $image |
| Parameters | SecondEndpoint=$($parameters.SecondEndpoint), FormatMask0=0x$('{0:X}' -f $parameters.FormatMask0) ($($kinds.FormatMask0)), FormatMask1=0x$('{0:X}' -f $parameters.FormatMask1), WritePacket=$($parameters.WritePacket) |
"@
if ($env:GITHUB_OUTPUT) {
    Add-Content -Path $env:GITHUB_OUTPUT -Value "loaded=$($loaded.ToString().ToLower())"
}
Write-Host "driver package installed; kernel load: $(if ($loaded) { 'yes' } else { "refused ($problem)" })"
