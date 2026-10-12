# Test-signs the driver package and installs it on a GitHub-hosted Windows
# runner, as far as a machine without test-signing mode allows (ADR 0027):
# catalog, signatures, driver store, root device node, service and its
# Parameters defaults. The kernel then refuses the test-signed image, so the
# device must end with problem code 52 (CM_PROB_UNSIGNED_DRIVER); any other
# outcome fails. CI only; the certificate is made here and thrown away.
param(
    # Folder with bioatmos.sys and bioatmos.inf (the build job's artifact).
    [Parameter(Mandatory)] [string] $Package
)
$ErrorActionPreference = 'Stop'

$HardwareId = 'Root\BioAtmos'
$Service = 'bioatmos'

function Find-KitTool([string] $Name, [string] $Arch) {
    $root = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $tool = Get-ChildItem -Path $root -Recurse -Filter $Name -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq $Arch } |
        Sort-Object { $_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $tool) { throw "$Name ($Arch) not found under $root" }
    $tool.FullName
}

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
if ($problem -ne 52) { throw "expected problem code 52 (unsigned image refused by the kernel), got $problem" }

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
| kernel load without test signing | refused, problem code $problem (expected 52) |
| service ImagePath | $image |
| Parameters | SecondEndpoint=$($parameters.SecondEndpoint), FormatMask0=0x$('{0:X}' -f $parameters.FormatMask0) ($($kinds.FormatMask0)), FormatMask1=0x$('{0:X}' -f $parameters.FormatMask1), WritePacket=$($parameters.WritePacket) |
"@
Write-Host 'driver package installed as far as this runner allows'
