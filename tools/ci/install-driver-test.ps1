# Test-signs the driver package and installs it on a GitHub-hosted Windows
# runner (ADR 0027): catalog, signatures, driver store, root device node,
# service and its Parameters defaults. ADR 0027 expected the kernel to refuse
# the test-signed image (problem code 52, CM_PROB_UNSIGNED_DRIVER); the first
# run showed the runner's kernel loading it, so either outcome passes here and
# the step output `loaded` says which. When it loads, the job goes on to play
# through both endpoints and read the records back (driver_flow_check.py).
# Any other problem code fails. CI only; the certificate is made here and
# thrown away.
param(
    # Folder with bioatmos.sys and bioatmos.inf (the build job's artifact).
    [Parameter(Mandatory)] [string] $Package
)
$ErrorActionPreference = 'Stop'

$HardwareId = 'Root\BioAtmos'
$Service = 'bioatmos'

. (Join-Path $PSScriptRoot 'kit-tools.ps1')

function Invoke-Checked([string] $FilePath, [string[]] $Arguments) {
    Write-Host "> $FilePath $($Arguments -join ' ')"
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path -Leaf $FilePath) failed ($LASTEXITCODE)" }
}

$signtool = Find-KitTool 'signtool.exe' 'x64'
$inf2cat = Find-KitTool 'Inf2Cat.exe' 'x86'

$work = Join-Path $env:RUNNER_TEMP 'bioatmos-package'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $work | Out-Null
Copy-Item (Join-Path $Package 'bioatmos.sys'), (Join-Path $Package 'bioatmos.inf') $work

# A throwaway code-signing certificate, trusted on this runner only.
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=BIO Atmos CI test signing' `
    -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddDays(2)
foreach ($store in 'Root', 'TrustedPublisher') {
    $s = New-Object System.Security.Cryptography.X509Certificates.X509Store($store, 'LocalMachine')
    $s.Open('ReadWrite'); $s.Add($cert); $s.Close()
}

$sign = @('sign', '/fd', 'sha256', '/sm', '/s', 'My', '/sha1', $cert.Thumbprint)
Invoke-Checked $signtool ($sign + (Join-Path $work 'bioatmos.sys'))
Invoke-Checked $inf2cat @("/driver:$work", '/os:10_VB_X64,ServerFE_X64', '/uselocaltime')
Invoke-Checked $signtool ($sign + (Join-Path $work 'bioatmos.cat'))
Invoke-Checked $signtool @('verify', '/pa', '/v', (Join-Path $work 'bioatmos.sys'))
Invoke-Checked $signtool @('verify', '/pa', '/v', '/c', (Join-Path $work 'bioatmos.cat'), (Join-Path $work 'bioatmos.sys'))

Invoke-Checked pnputil.exe @('/add-driver', (Join-Path $work 'bioatmos.inf'))

Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'RootDevice.cs'))
try {
    $reboot = [RootDevice]::Install((Join-Path $work 'bioatmos.inf'), $HardwareId)
    Write-Host "UpdateDriverForPlugAndPlayDevices returned (reboot requested: $reboot)"
} catch {
    # The kernel refusing the image may surface here as well; the device's
    # problem code below decides.
    Write-Host "UpdateDriverForPlugAndPlayDevices: $($_.Exception.Message)"
}

$device = Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $HardwareId } | Select-Object -First 1
if (-not $device) { throw "no device with hardware id $HardwareId was created" }
$problem = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_ProblemCode').Data
$driver = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data
Write-Host "device $($device.InstanceId): status $($device.Status), problem $problem, driver $driver"
if (-not $driver) { throw 'the device has no driver from our package' }
# What lets a test-signed image load: test signing or integrity checks off in
# the boot entry, or neither (then the result below is worth a note in ADR 0027).
$boot = (bcdedit /enum '{current}' | Select-String -Pattern 'testsigning|nointegritychecks|bootdebug|debug ') -join '; '
Write-Host "boot entry: $(if ($boot) { $boot } else { 'no testsigning or nointegritychecks entry' })"
$serviceState = (Get-CimInstance Win32_SystemDriver -Filter "Name='$Service'").State
Write-Host "service $Service state: $serviceState"
$loaded = switch ($problem) {
    0 { if ($serviceState -ne 'Running') { throw "problem code 0 but the service is $serviceState" }; $true }
    52 { $false }
    default { throw "the device ended with problem code $problem (0 = loaded, 52 = image refused)" }
}

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
| Inf2Cat, signtool sign and verify | ok |
| driver store (pnputil) | ok |
| root device $HardwareId | $($device.InstanceId), driver $driver |
| kernel load | $(if ($loaded) { "loaded (service $serviceState)" } else { 'refused, problem code 52' }); boot entry: $(if ($boot) { $boot } else { 'default' }) |
| service ImagePath | $image |
| Parameters | SecondEndpoint=$($parameters.SecondEndpoint), FormatMask0=0x$('{0:X}' -f $parameters.FormatMask0) ($($kinds.FormatMask0)), FormatMask1=0x$('{0:X}' -f $parameters.FormatMask1), WritePacket=$($parameters.WritePacket) |
"@
if ($env:GITHUB_OUTPUT) { Add-Content -Path $env:GITHUB_OUTPUT -Value "loaded=$($loaded.ToString().ToLower())" }
Write-Host "driver package installed; kernel load: $(if ($loaded) { 'yes' } else { 'refused (52)' })"
