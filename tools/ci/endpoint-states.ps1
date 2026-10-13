# Prints render endpoints with their states and checks stable endpoint numbers.
param(
    [Parameter(Mandatory)] [string] $InstanceId,
    [int] $Expect = 3
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'endpoints.ps1')
$states = @{ 1 = 'active'; 2 = 'disabled'; 4 = 'not present'; 8 = 'unplugged' }
$device = Get-PnpDevice -InstanceId $InstanceId -ErrorAction Stop
. (Join-Path $PSScriptRoot 'package-identities.ps1')
if ($device.HardwareID -contains $SpatialPackage.HardwareId) {
    $items = @(Get-SpatialEndpoints -InstanceId $InstanceId)
    foreach ($item in $items) { Write-Host "$($item.endpoint) $($item.id) $($states[[int]$item.state]) '$($item.name)'" }
    $active = @($items | Where-Object state -eq 1)
    if ($active.Count -ne $Expect) { throw "$($active.Count) of $Expect render endpoints of $InstanceId are active" }
    foreach ($ep in 0..($Expect - 1)) {
        if (@($active | Where-Object endpoint -eq $ep).Count -ne 1) { throw "expected one active endpoint $ep" }
    }
} else {
    # SysVAD control job has no Spatial mini-pair contract. Count only the
    # supplied node's endpoints, without matching display names. The owner
    # property reads like '{1}.ROOT\MEDIA\0000': the instance id must end it
    # whole, after a '.' or '\' (ROOT\MEDIA\0000 is not ROOT\MEDIA\10000).
    $ownerPattern = '(^|[.\\])' + [regex]::Escape($InstanceId) + '$'
    $active = @()
    foreach ($endpoint in Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render') {
        $properties = Get-ItemProperty (Join-Path $endpoint.PSPath 'Properties')
        $owner = [string]$properties.'{b3f8fa53-0004-438e-9003-51a46e139bfc},2'
        if ($owner -notmatch $ownerPattern) { continue }
        $state = [int](Get-ItemProperty $endpoint.PSPath).DeviceState
        Write-Host "$($endpoint.PSChildName) $($states[$state]) '$($properties.'{a45c254e-df1c-4efd-8020-67d146a850e0},2')'"
        if ($state -eq 1) { $active += $endpoint }
    }
    if ($active.Count -lt $Expect) { throw "$($active.Count) of $Expect control-driver endpoints are active" }
}
Write-Host "$($active.Count) of $Expect render endpoints of $InstanceId are active"
