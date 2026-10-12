# Prints what the Windows audio stack made of an installed audio driver: its
# KS device interfaces, the MMDevices render and capture endpoints with their
# states, the audio services and their recent events (ADR 0027). Never fails;
# CI only.
param(
    # Instance id of the driver's device node, e.g. ROOT\MEDIA\0000.
    [string] $InstanceId = ''
)
$ErrorActionPreference = 'Continue'

$categories = [ordered]@{
    KSCATEGORY_AUDIO    = '{6994AD04-93EF-11D0-A3CC-00A0C9223196}'
    KSCATEGORY_RENDER   = '{65E8773E-8F56-11D0-A3B9-00A0C9223196}'
    KSCATEGORY_REALTIME = '{EB115FFC-10C8-4964-831D-6DCB02E6F23F}'
    KSCATEGORY_TOPOLOGY = '{DDA54A40-1E4C-11D1-A050-405705C10000}'
}
Write-Host '::group::KS device interfaces'
$needle = ($InstanceId -replace '\\', '#')
foreach ($name in $categories.Keys) {
    $root = "HKLM:\SYSTEM\CurrentControlSet\Control\DeviceClasses\$($categories[$name])"
    Get-ChildItem $root -ErrorAction SilentlyContinue | Where-Object { -not $needle -or $_.PSChildName -like "*$needle*" } | ForEach-Object {
        foreach ($reference in Get-ChildItem $_.PSPath -ErrorAction SilentlyContinue) {
            $linked = (Get-ItemProperty (Join-Path $reference.PSPath 'Control') -ErrorAction SilentlyContinue).Linked
            Write-Host "$name $($_.PSChildName) $($reference.PSChildName) linked=$linked"
        }
    }
}
Write-Host '::endgroup::'

$states = @{ 1 = 'active'; 2 = 'disabled'; 4 = 'not present'; 8 = 'unplugged' }
foreach ($flow in 'Render', 'Capture') {
    Write-Host "::group::MMDevices $flow endpoints"
    foreach ($endpoint in Get-ChildItem "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\$flow" -ErrorAction SilentlyContinue) {
        $state = (Get-ItemProperty $endpoint.PSPath -ErrorAction SilentlyContinue).DeviceState
        $properties = Get-ItemProperty (Join-Path $endpoint.PSPath 'Properties') -ErrorAction SilentlyContinue
        $friendly = $properties.'{a45c254e-df1c-4efd-8020-67d146a850e0},2'
        # The KS filter the endpoint belongs to; one of the two keys carries it.
        $device = $properties.'{b3f8fa53-0004-438e-9003-51a46e139bfc},2', $properties.'{233164c8-1b2c-4c7d-bc68-b671687a2567},1' |
            Where-Object { $_ } | Select-Object -First 1
        $label = if ($states.ContainsKey([int]$state)) { $states[[int]$state] } else { "0x{0:X}" -f [int]$state }
        Write-Host "$($endpoint.PSChildName) state=$label name='$friendly' device='$device'"
    }
    Write-Host '::endgroup::'
}

Write-Host '::group::AudioEndpoint device nodes'
Get-PnpDevice -Class AudioEndpoint -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Host "$($_.Status) $($_.FriendlyName) $($_.InstanceId)" }
Write-Host '::endgroup::'

Write-Host '::group::Audio services'
Get-Service AudioEndpointBuilder, Audiosrv | ForEach-Object { Write-Host "$($_.Name): $($_.Status) ($($_.StartType))" }
Write-Host '::endgroup::'

Write-Host '::group::Recent audio events'
$since = (Get-Date).AddMinutes(-30)
Get-WinEvent -FilterHashtable @{ LogName = 'System'; StartTime = $since } -ErrorAction SilentlyContinue |
    Where-Object { $_.ProviderName -match 'Audio|Kernel-PnP|bioatmos|Service Control Manager' } |
    Select-Object -First 40 |
    ForEach-Object { Write-Host "$($_.TimeCreated.ToString('HH:mm:ss')) $($_.ProviderName) $($_.Id): $($_.Message -replace '\s+', ' ')" }
foreach ($log in 'Microsoft-Windows-Audio/Operational', 'Microsoft-Windows-Audio/PlaybackManager') {
    Get-WinEvent -LogName $log -MaxEvents 20 -ErrorAction SilentlyContinue |
        ForEach-Object { Write-Host "$($_.TimeCreated.ToString('HH:mm:ss')) $log $($_.Id): $($_.Message -replace '\s+', ' ')" }
}
Write-Host '::endgroup::'
