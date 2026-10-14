# Starts, or stops and prints, an ETW session on the driver's KS request trace
# (driver/spatialsoftreceiver/kstrace.cpp, provider SpatialSoftReceiver.Driver.Ks): every create and
# KS IOCTL that reached PortCls through our dispatch, with the process that
# sent it, the property set, id, flags and pin, the client's data range or
# format, and the status PortCls returned. Run -Start before the device is
# installed and -Stop before anything else opens the filters, so the trace is
# the endpoint builder's own conversation with them. Diagnosis only; never
# fails; CI only.
param(
    [switch] $Start,
    [switch] $Stop,
    # Names the session and its files, so one job can keep several traces.
    [string] $Label = 'install'
)
$ErrorActionPreference = 'Continue'
$Provider = '{a63636ab-03aa-4a58-9dcd-86c273eb95fd}'
$Session = "SpatialSoftReceiverKs-$Label"
$etl = Join-Path $env:RUNNER_TEMP "spatialsoftreceiver-ks-$Label.etl"

if ($Start) {
    Remove-Item -LiteralPath $etl -ErrorAction SilentlyContinue
    & logman.exe start $Session -p $Provider 0xffffffffffffffff 0xff -o $etl -ets -bs 1024 -nb 16 128 2>&1 | Write-Host
    exit 0
}
if (-not $Stop) { Write-Host 'ks-trace.ps1: pass -Start or -Stop'; exit 0 }

& logman.exe stop $Session -ets 2>&1 | Write-Host
if (-not (Test-Path -LiteralPath $etl)) { Write-Host "no trace file at $etl"; exit 0 }
$xml = Join-Path $env:RUNNER_TEMP "spatialsoftreceiver-ks-$Label.xml"
& tracerpt.exe $etl -o $xml -of XML -y 2>&1 | Select-Object -Last 3 | Write-Host

# Process names, with the services each svchost carries.
$names = @{}
foreach ($p in Get-Process -ErrorAction SilentlyContinue) { $names[[uint32]$p.Id] = $p.ProcessName }
foreach ($s in Get-CimInstance Win32_Service -ErrorAction SilentlyContinue | Where-Object ProcessId) {
    $id = [uint32]$s.ProcessId
    if ($names[$id] -eq 'svchost') { $names[$id] = "svchost($($s.Name))" }
    elseif ($names[$id] -like 'svchost(*') { $names[$id] = $names[$id] -replace '\)$', ",$($s.Name))" }
}

$sets = @{
    '8c134960-51ad-11cf-878a-94f801c10000' = 'Pin'; '1464eda5-6a8f-11d1-9aa7-00a0c9223196' = 'General'
    '720d4ac0-7533-11d0-a5d6-28db04c10000' = 'Topology'; '4509f757-2d46-4637-8e62-ce7db944f57b' = 'Jack'
    '4f67b528-30c9-40de-b2fb-859ddd1f3470' = 'AudioSignalProcessing'; '45ffaaa0-6e1b-11d0-bcf2-444553540000' = 'Audio'
    'a855a48c-2f78-4729-9051-1968746b9eef' = 'RtAudio'; '1d58c920-ac9b-11cf-a5d6-28db04c10000' = 'Connection'
    '1fdd8ee1-9cd3-11d0-82aa-0000f822fe8a' = 'Stream'; '3a2f82dc-886f-4baa-9eb4-082b9025c536' = 'AudioEngine'
    'c034fdb0-ff75-47c8-aad3-b57ae0e61bdb' = 'AudioModule'; 'bc85be5e-fe53-4ec0-a4f1-1b1b1fbbd5a5' = 'AudioPosture'
}
$pinIds = 'CINSTANCES', 'CTYPES', 'DATAFLOW', 'DATARANGES', 'DATAINTERSECTION', 'INTERFACES', 'MEDIUMS',
    'COMMUNICATION', 'GLOBALCINSTANCES', 'NECESSARYINSTANCES', 'PHYSICALCONNECTION', 'CATEGORY', 'NAME',
    'CONSTRAINEDDATARANGES', 'PROPOSEDATAFORMAT', 'PROPOSEDATAFORMAT2', 'MODEDATAFORMATS'
$ioctls = @{ 0x2f0003 = 'PROPERTY'; 0x2f0007 = 'METHOD'; 0x2f000b = 'ENABLE_EVENT'; 0x2f000f = 'DISABLE_EVENT'
    0x2f0013 = 'READ_STREAM'; 0x2f0017 = 'WRITE_STREAM'; 0x2f001b = 'RESET_STATE' }

function Bytes([string] $hex) {
    if (-not $hex) { return ,[byte[]]@() }
    $hex = $hex -replace '^0x', ''
    $out = New-Object byte[] ($hex.Length / 2)
    for ($i = 0; $i -lt $out.Length; $i++) { $out[$i] = [Convert]::ToByte($hex.Substring(2 * $i, 2), 16) }
    ,$out
}
function GuidAt([byte[]] $b, [int] $at) {
    if ($b.Length -lt $at + 16) { return '' }
    ([guid][byte[]]$b[$at..($at + 15)]).ToString()
}
function U32([byte[]] $b, [int] $at) { if ($b.Length -lt $at + 4) { return $null }; [BitConverter]::ToUInt32($b, $at) }
function Name([byte[]] $b) {
    $text = [Text.Encoding]::Unicode.GetString($b)
    $cut = $text.IndexOf([char]0); if ($cut -ge 0) { $text = $text.Substring(0, $cut) }
    $text
}
# A data range or format: size, subformat, specifier, and the audio fields.
function Range([byte[]] $b, [int] $at) {
    $size = U32 $b $at
    if ($null -eq $size -or $b.Length -lt $at + 64) { return '' }
    $sub = GuidAt $b ($at + 32); $spec = GuidAt $b ($at + 48)
    $tail = ''
    if ($b.Length -ge $at + 84 -and $size -eq 88) {
        $tail = " ch<=$(U32 $b ($at + 64)) bits=$(U32 $b ($at + 68))..$(U32 $b ($at + 72)) rate=$(U32 $b ($at + 76))..$(U32 $b ($at + 80))"
    } elseif ($b.Length -ge $at + 82 -and $size -ge 82) {
        $tag = [BitConverter]::ToUInt16($b, $at + 64); $ch = [BitConverter]::ToUInt16($b, $at + 66)
        $tail = " tag=0x$('{0:x4}' -f $tag) ch=$ch rate=$(U32 $b ($at + 68)) align=$([BitConverter]::ToUInt16($b, $at + 76)) bits=$([BitConverter]::ToUInt16($b, $at + 78))"
    }
    $flags = U32 $b ($at + 4)
    "size=$size flags=0x$('{0:x}' -f $flags) sub=$sub spec=$spec$tail"
}

$doc = [xml](Get-Content -Raw -LiteralPath $xml)
$files = @{}
$lines = New-Object System.Collections.Generic.List[string]
$raw = 0
foreach ($event in $doc.Events.Event) {
  try {
    $task = $event.System.Task; if (-not $task) { $task = $event.RenderingInfo.Task }
    $data = @{}
    foreach ($d in $event.EventData.Data) { $data[$d.Name] = $d.'#text' }
    if (-not $data.ContainsKey('Process')) { continue }
    $time = ([datetime]$event.System.TimeCreated.SystemTime).ToString('HH:mm:ss.fff')
    $process = [uint32]$data.Process
    $who = if ($names.ContainsKey($process)) { "$($names[$process])[$process]" } else { "pid $process" }
    $status = $data.Status
    if ($data.ContainsKey('StackLeft')) {
        # KsEnter, written before PortCls: an entry with no line after it is
        # a request that never returned.
        $target = $files[$data.File]; if (-not $target) { $target = "file $($data.File)" }
        $lines.Add("$time $who ENTER major $($data.Major) code $($data.Code) irql $($data.Irql) stack left $($data.StackLeft) @ $target")
        continue
    }
    if ($data.ContainsKey('Name')) {
        $name = Bytes $data.Name
        $text = Name $name
        $file = $data.File
        # A pin create's name is KSSTRING_Pin, a backslash, then binary.
        $pinName = $text.IndexOf('{146F1A80-4791-11D0-A5D6-28DB04C10000}', [StringComparison]::OrdinalIgnoreCase)
        if ($pinName -ge 0) {
            $parent = $files[$data.Related]
            # KSPIN_CONNECT after the 38-character GUID and its backslash:
            # Interface, Medium, PinId, PinToHandle, Priority; then the data
            # format.
            $connect = 2 * ($pinName + 39)
            $pin = U32 $name ($connect + 48)
            $files[$file] = "$parent/pin$pin"
            $lines.Add("$time $who CREATE pin $pin of $parent ($(Range $name ($connect + 72))) -> $status")
        } else {
            $files[$file] = $text.TrimStart('\')
            $lines.Add("$time $who CREATE $text -> $status")
        }
        continue
    }
    $code = [Convert]::ToUInt32(($data.Code -replace '^0x', ''), 16)
    $target = $files[$data.File]; if (-not $target) { $target = "file $($data.File)" }
    $request = Bytes $data.Input
    $what = if ($ioctls.ContainsKey([int]$code)) { $ioctls[[int]$code] } else { '0x{0:x}' -f $code }
    if ($code -eq 0x2f0003 -or $code -eq 0x2f0007 -or $code -eq 0x2f000b) {
        $set = GuidAt $request 0; $id = U32 $request 16; $flags = U32 $request 20
        $setName = if ($sets.ContainsKey($set)) { $sets[$set] } else { $set }
        $idName = if ($setName -eq 'Pin' -and $id -lt $pinIds.Count) { $pinIds[$id] } else { "$id" }
        $verb = @(); if ($flags -band 1) { $verb += 'GET' }; if ($flags -band 2) { $verb += 'SET' }
        if ($flags -band 0x200) { $verb += 'BASICSUPPORT' }; if ($flags -band 0x1000) { $verb += 'TOPOLOGY' }
        $extra = ''
        if ([int]$data.InputLength -ge 32) { $extra += " pin=$(U32 $request 24)" }
        if ($flags -band 0x1000 -and [int]$data.InputLength -ge 28) { $extra += " node=$(U32 $request 24)" }
        if ($setName -eq 'Pin' -and $id -eq 4 -and $request.Length -ge 40) {
            $extra += " items=$(U32 $request 36) first:[$(Range $request 40)]"
        }
        if ($data.Value) { $extra += " value:[$(Range (Bytes $data.Value) 0)]" }
        $lines.Add("$time $who $what $setName.$idName $($verb -join '|')$extra out=$($data.OutputLength) -> $status @ $target")
    } else {
        $lines.Add("$time $who $what in=$($data.InputLength) out=$($data.OutputLength) -> $status @ $target")
    }
  } catch {
    if ($raw++ -lt 3) { Write-Host "undecoded event: $($_.Exception.Message) $($event.OuterXml)" }
  }
}
Write-Host "::group::KS requests to our filters, $Label ($($lines.Count) events)"
$lines | ForEach-Object { Write-Host $_ }
Write-Host '::endgroup::'
exit 0
