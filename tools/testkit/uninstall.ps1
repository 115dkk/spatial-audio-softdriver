#Requires -RunAsAdministrator
# Removes current and legacy root devices, packages, services and test certificates.
param([switch] $KeepCertificates)
$ErrorActionPreference = 'Continue'
$helper = Join-Path $PSScriptRoot 'package-identities.ps1'
if (-not (Test-Path -LiteralPath $helper)) { $helper = Join-Path $PSScriptRoot '..\ci\package-identities.ps1' }
. $helper
$packages = @($SpatialPackage) + $SpatialLegacyPackages
$hardwareIds = @($packages | ForEach-Object HardwareId)
$infNames = @($packages | ForEach-Object Inf)
$certificateSubjects = @($packages | ForEach-Object CertificateSubject)
$failed = $false
function Test-OurCertificate($Certificate) {
    foreach ($subject in $certificateSubjects) { if ($Certificate.Subject -like $subject) { return $true } }
    return $false
}
function Get-OurDevices {
    Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object {
        @($_.HardwareID | Where-Object { $_ -in $hardwareIds }).Count -gt 0
    }
}
$devices = @(Get-OurDevices)
$owners = @($devices | ForEach-Object InstanceId)
foreach ($device in $devices) {
    try {
        . (Join-Path (Split-Path $helper) 'exclusive-policy.ps1')
        Clear-SsrExclusivePolicy -InstanceId $device.InstanceId
    } catch { Write-Warning "policy cleanup: $($_.Exception.Message)"; $failed = $true }
    Write-Host "removing device $($device.InstanceId)"
    & pnputil.exe /remove-device $device.InstanceId
    if ($LASTEXITCODE -ne 0) { Write-Warning "could not remove $($device.InstanceId) ($LASTEXITCODE)"; $failed = $true }
}
$drivers = @(Get-WindowsDriver -Online -ErrorAction SilentlyContinue | Where-Object {
    (Split-Path -Leaf $_.OriginalFileName) -in $infNames
})
foreach ($driver in $drivers) {
    $published = $driver.Driver
    if (-not $published) { $published = $driver.PublishedName }
    Write-Host "deleting driver package $published (original $($driver.OriginalFileName))"
    & pnputil.exe /delete-driver $published /uninstall /force
    if ($LASTEXITCODE -ne 0) { Write-Warning "could not delete $published ($LASTEXITCODE)"; $failed = $true }
}
# Remove orphan legacy services as well, but never while one of their devices
# or packages failed to uninstall. No direct service-registry deletion.
$remainingDevices = @(Get-OurDevices)
$remainingDrivers = @(Get-WindowsDriver -Online -ErrorAction SilentlyContinue | Where-Object {
    (Split-Path -Leaf $_.OriginalFileName) -in $infNames
})
foreach ($package in $packages) {
    if (@($remainingDevices | Where-Object { $_.HardwareID -contains $package.HardwareId }).Count -or
        @($remainingDrivers | Where-Object { (Split-Path -Leaf $_.OriginalFileName) -ieq $package.Inf }).Count) { continue }
    if (Test-Path "HKLM:\SYSTEM\CurrentControlSet\Services\$($package.Service)") {
        & sc.exe delete $package.Service
        if ($LASTEXITCODE -notin @(0, 1060, 1072)) { $failed = $true }
    }
}
if (-not $KeepCertificates) {
    foreach ($name in 'Root', 'TrustedPublisher') {
        $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($name, 'LocalMachine')
        try {
            $store.Open('ReadWrite')
            foreach ($certificate in @($store.Certificates | Where-Object { Test-OurCertificate $_ })) {
                Write-Host "removing certificate $name $($certificate.Thumbprint)"
                $store.Remove($certificate)
            }
        } finally { $store.Close() }
    }
}
$remainingCerts = @()
if (-not $KeepCertificates) {
    foreach ($name in 'Root', 'TrustedPublisher') {
        $remainingCerts += @(Get-ChildItem "Cert:\LocalMachine\$name" | Where-Object { Test-OurCertificate $_ })
    }
}
$serviceKeys = @($packages | Where-Object { Test-Path "HKLM:\SYSTEM\CurrentControlSet\Services\$($_.Service)" })
# Once PnP removal destroys topology, only saved ownership can identify its
# tombstones. Never delete unrelated endpoints by display name or alter ACLs.
$remainingEndpoints = @(Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render' -ErrorAction SilentlyContinue | Where-Object {
    $props = Get-ItemProperty (Join-Path $_.PSPath 'Properties') -ErrorAction SilentlyContinue
    $owner = [string]$props.'{b3f8fa53-0004-438e-9003-51a46e139bfc},2'
    @($owners | Where-Object { $owner.Equals($_, [StringComparison]::OrdinalIgnoreCase) -or $owner.EndsWith("\$_", [StringComparison]::OrdinalIgnoreCase) }).Count -gt 0
})
Write-Host "remaining: devices=$($remainingDevices.Count), driver packages=$($remainingDrivers.Count), certificates=$($remainingCerts.Count), service keys=$($serviceKeys.Count), endpoints=$($remainingEndpoints.Count)"
if ($serviceKeys.Count -and $remainingDevices.Count -eq 0 -and $remainingDrivers.Count -eq 0) {
    Write-Warning '서비스 키는 장치와 패키지를 지운 뒤에도 재부팅 전까지 남을 수 있습니다. 재부팅 후 다시 확인하십시오.'
}
if ($remainingDevices.Count -or $remainingDrivers.Count -or $remainingCerts.Count -or $remainingEndpoints.Count) { $failed = $true }
exit $(if ($failed) { 1 } else { 0 })
