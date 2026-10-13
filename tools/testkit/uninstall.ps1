#Requires -RunAsAdministrator
# Removes all BIO Atmos root devices, packages, and test certificates.
param([switch] $KeepCertificates)
$ErrorActionPreference = 'Continue'
$HardwareId = 'Root\BioAtmos'
$Service = 'bioatmos'
$failed = $false

$devices = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $HardwareId })
foreach ($device in $devices) {
    Write-Host "removing device $($device.InstanceId)"
    & pnputil.exe /remove-device $device.InstanceId
    if ($LASTEXITCODE -ne 0) { Write-Warning "could not remove $($device.InstanceId) ($LASTEXITCODE)"; $failed = $true }
}

$drivers = @(Get-WindowsDriver -Online -ErrorAction SilentlyContinue | Where-Object {
    (Split-Path -Leaf $_.OriginalFileName) -ieq 'bioatmos.inf'
})
foreach ($driver in $drivers) {
    $published = $driver.Driver
    if (-not $published) { $published = $driver.PublishedName }
    Write-Host "deleting driver package $published (original bioatmos.inf)"
    & pnputil.exe /delete-driver $published /uninstall /force
    if ($LASTEXITCODE -ne 0) { Write-Warning "could not delete $published ($LASTEXITCODE)"; $failed = $true }
}

if (-not $KeepCertificates) {
    foreach ($name in 'Root', 'TrustedPublisher') {
        $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($name, 'LocalMachine')
        try {
            $store.Open('ReadWrite')
            foreach ($certificate in @($store.Certificates | Where-Object { $_.Subject -like 'CN=BIO Atmos test signing*' })) {
                Write-Host "removing certificate $name $($certificate.Thumbprint)"
                $store.Remove($certificate)
            }
        } finally { $store.Close() }
    }
}

$remainingDevices = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $HardwareId })
$remainingDrivers = @(Get-WindowsDriver -Online -ErrorAction SilentlyContinue | Where-Object {
    (Split-Path -Leaf $_.OriginalFileName) -ieq 'bioatmos.inf'
})
$remainingCerts = @()
if (-not $KeepCertificates) {
    foreach ($name in 'Root', 'TrustedPublisher') {
        $remainingCerts += @(Get-ChildItem "Cert:\LocalMachine\$name" | Where-Object { $_.Subject -like 'CN=BIO Atmos test signing*' })
    }
}
$serviceKey = Test-Path "HKLM:\SYSTEM\CurrentControlSet\Services\$Service"
Write-Host "remaining: devices=$($remainingDevices.Count), driver packages=$($remainingDrivers.Count), certificates=$($remainingCerts.Count), service key=$serviceKey"
if ($serviceKey -and $remainingDevices.Count -eq 0 -and $remainingDrivers.Count -eq 0) {
    Write-Warning '서비스 키는 장치와 패키지를 지운 뒤에도 재부팅 전까지 남을 수 있습니다. 재부팅 후 다시 확인하십시오.'
}
if ($remainingDevices.Count -or $remainingDrivers.Count -or $remainingCerts.Count) { $failed = $true }
# A service marked for deletion may legitimately remain until reboot; do not fail
# solely on the service registry key when devices and packages are gone.
exit $(if ($failed) { 1 } else { 0 })
