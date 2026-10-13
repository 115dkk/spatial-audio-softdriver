#Requires -RunAsAdministrator
# Builds the distributable test kit, installs through the same script used on a
# test-signing PC, then checks CI-only service defaults and emits job outputs.
# Problem code 52 (image refused) and an endpoint-count result are accepted here;
# later CI steps decide whether loaded endpoints work.
param(
    # Folder with spatialsoftreceiver.sys and spatialsoftreceiver.inf (the build job's artifact).
    [Parameter(Mandatory)] [string] $Package
)
$ErrorActionPreference = 'Stop'
$HardwareId = 'Root\SpatialSoftReceiver'
$Service = 'SpatialSoftReceiver'

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
if ($image -notmatch 'spatialsoftreceiver\.sys$') { throw "service ImagePath is '$image'" }
$parameters = Get-ItemProperty "$serviceKey\Parameters"
$expected = @{ EndpointMask = 0x7; CodecMask0 = 0x6C; CodecMask1 = 0x6C; CodecMask2 = 0x6C; CodecMask3 = 0x6C; WritePacket = 0 }
foreach ($ep in 0..3) {
    $expected["PcmRateMask$ep"] = 0x3f
    $expected["PcmShapeMask$ep"] = 0x1f
    $expected["PcmMaxMaskedChannels$ep"] = 18
    $expected["PcmDirectChannels$ep"] = -1 # REG_DWORD is returned as signed Int32
}
foreach ($name in $expected.Keys) {
    if ($parameters.$name -ne $expected[$name]) {
        throw "Parameters\$name is '$($parameters.$name)', expected $($expected[$name])"
    }
}
$kinds = @{}
$key = Get-Item "$serviceKey\Parameters"
foreach ($name in $expected.Keys) { $kinds[$name] = $key.GetValueKind($name) }
foreach ($ep in 0..2) {
    $name = "EndpointName$ep"
    if ($key.GetValueKind($name) -ne 'String' -or [string]::IsNullOrEmpty($parameters.$name) -or $parameters.$name.Length -gt 31) {
        throw "Parameters\$name must be a nonempty REG_SZ of at most 31 UTF-16 code units"
    }
}
$formatKinds = 0..3 | ForEach-Object { $kinds["CodecMask$_"] }
if (@($formatKinds | Where-Object { $_ -ne 'QWord' }).Count) {
    throw "codec masks are $($formatKinds -join '/'), expected QWord"
}

foreach ($name in $expected.Keys | Where-Object { $_ -like 'Pcm*' }) {
    if ($kinds[$name] -ne 'DWord') { throw "$name must be DWord" }
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
| Parameters | EndpointMask=0x$('{0:X}' -f $parameters.EndpointMask), CodecMask0..3=$((0..3 | ForEach-Object { '0x{0:X}' -f $parameters.PSObject.Properties["CodecMask$_"].Value }) -join ', '), WritePacket=$($parameters.WritePacket) |
"@
if ($env:GITHUB_OUTPUT) {
    Add-Content -Path $env:GITHUB_OUTPUT -Value "loaded=$($loaded.ToString().ToLower())"
}
Write-Host "driver package installed; kernel load: $(if ($loaded) { 'yes' } else { "refused ($problem)" })"
