# Prints the MMDevices render endpoints of one device node with their states
# and fails unless the expected number of them is active. CI only.
param(
    # Instance id of the driver's device node, e.g. ROOT\MEDIA\0000.
    [Parameter(Mandatory)] [string] $InstanceId,
    [int] $Expect = 2
)
$states = @{ 1 = 'active'; 2 = 'disabled'; 4 = 'not present'; 8 = 'unplugged' }
$active = 0
foreach ($endpoint in Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render' -ErrorAction SilentlyContinue) {
    $properties = Get-ItemProperty (Join-Path $endpoint.PSPath 'Properties') -ErrorAction SilentlyContinue
    if ($properties.'{b3f8fa53-0004-438e-9003-51a46e139bfc},2' -notlike "*$InstanceId") { continue }
    $state = [int](Get-ItemProperty $endpoint.PSPath).DeviceState
    $label = if ($states.ContainsKey($state)) { $states[$state] } else { '0x{0:X}' -f $state }
    Write-Host "$($endpoint.PSChildName) $label '$($properties.'{a45c254e-df1c-4efd-8020-67d146a850e0},2')'"
    if ($state -eq 1) { $active++ }
}
Write-Host "$active of $Expect render endpoints of $InstanceId are active"
if ($active -lt $Expect) { exit 1 }
