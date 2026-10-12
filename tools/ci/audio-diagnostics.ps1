# Prints what the Windows audio stack made of an installed audio driver: its
# KS device interfaces, the MMDevices render and capture endpoints with their
# states, the audio services and their recent events, and the answers its KS
# filters give to the questions the endpoint builder asks (tools/ci/KsProbe.cs)
# (ADR 0027). Never fails; CI only.
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
# Interface keys hold '#' and '?': read them with -LiteralPath, never as a
# wildcard pattern.
$needle = ($InstanceId -replace '\\', '#')
foreach ($name in $categories.Keys) {
    $root = "HKLM:\SYSTEM\CurrentControlSet\Control\DeviceClasses\$($categories[$name])"
    foreach ($interface in Get-ChildItem -LiteralPath $root -ErrorAction SilentlyContinue) {
        if ($needle -and $interface.PSChildName -notlike "*$needle*") { continue }
        foreach ($reference in Get-ChildItem -LiteralPath $interface.PSPath -ErrorAction SilentlyContinue) {
            if ($reference.PSChildName -notlike '#*') { continue }
            $control = Join-Path $reference.PSPath 'Control'
            $linked = (Get-ItemProperty -LiteralPath $control -ErrorAction SilentlyContinue).Linked
            Write-Host "$name $($reference.PSChildName) linked=$linked"
        }
    }
}
if ($InstanceId) {
    & pnputil.exe /enum-devices /instanceid $InstanceId /interfaces /stack 2>&1 | Write-Host
    $problem = (Get-PnpDeviceProperty -InstanceId $InstanceId -KeyName DEVPKEY_Device_ProblemCode -ErrorAction SilentlyContinue).Data
    Write-Host "device $InstanceId problem code now: $problem"
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
        if ($InstanceId -and $device -like "*$InstanceId") {
            # Everything the endpoint builder stored for the device's endpoints:
            # which properties it got to tells how far it went, and an active
            # endpoint of another driver shows what a finished one carries.
            foreach ($property in $properties.PSObject.Properties | Where-Object { $_.Name -like '{*' }) {
                $value = $property.Value
                $text = if ($value -is [byte[]]) {
                    # Whole blob in hex, then any UTF-16 text in it (interface paths).
                    $hex = ($value | ForEach-Object { $_.ToString('x2') }) -join ''
                    $utf16 = ([Text.Encoding]::Unicode.GetString($value) -replace '[^\x20-\x7e]+', ' ').Trim()
                    "$hex | $utf16"
                } else { "$value" }
                Write-Host "    $($property.Name) = $text"
            }
        }
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
foreach ($log in 'Microsoft-Windows-Audio/Operational', 'Microsoft-Windows-Kernel-PnP/Configuration') {
    Get-WinEvent -LogName $log -MaxEvents 40 -ErrorAction SilentlyContinue |
        Where-Object { $_.TimeCreated -ge $since } | Sort-Object TimeCreated |
        ForEach-Object {
            $values = ($_.Properties | ForEach-Object { "$($_.Value)" }) -join ' | '
            Write-Host "$($_.TimeCreated.ToString('HH:mm:ss.fff')) $log $($_.Id): $($_.Message -replace '\s+', ' ') [$values]"
        }
}
Write-Host '::endgroup::'

if ($InstanceId) {
    # Pins, data ranges, intersections, physical connections and jacks, then
    # one pin created and closed. A KS request that never completes would hang
    # the step, so each part runs in its own job with a time limit.
    $source = Join-Path $PSScriptRoot 'KsProbe.cs'
    $fragment = ($InstanceId -replace '\\', '#').ToLowerInvariant()
    foreach ($method in 'Run', 'TryCreatePin') {
        Write-Host "::group::KS filters: KsProbe.$method"
        $job = Start-Job -ArgumentList $source, $fragment, $method -ScriptBlock {
            param($source, $fragment, $method)
            Add-Type -TypeDefinition (Get-Content -Raw $source)
            [KsProbe]::$method($fragment)
        }
        if (Wait-Job $job -Timeout 120) { Receive-Job $job -ErrorAction Continue | ForEach-Object { Write-Host $_ } }
        else { Write-Host "KsProbe.$method did not finish in 120 s" }
        Remove-Job $job -Force
        Write-Host '::endgroup::'
    }
}
