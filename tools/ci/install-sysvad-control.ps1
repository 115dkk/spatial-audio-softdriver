# Test-signs and installs a SysVAD TabletAudioSample build (the sample our
# driver is reduced from, ADR 0027) on a GitHub-hosted runner, as a control for
# the endpoint builder: if SysVAD's render endpoints turn active where ours stay
# NOTPRESENT, the difference is in our driver; if they do not, it is the runner.
# Its INF loses the keyword detector DLL and the DRM signature attributes, and
# its OS floor drops from build 22621 to 19041; nothing else changes. Writes the
# device's instance id to the step output `instance`. Diagnosis only; CI only;
# the certificate is made here and thrown away.
param(
    # Folder searched for TabletAudioSample.sys and ComponentizedAudioSample.inf.
    [Parameter(Mandatory)] [string] $Build
)
$ErrorActionPreference = 'Stop'

$HardwareId = 'Root\sysvad_ComponentizedAudioSample'

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

$sys = Get-ChildItem -Path $Build -Recurse -Filter 'TabletAudioSample.sys' | Select-Object -First 1
$inf = Get-ChildItem -Path $Build -Recurse -Filter 'ComponentizedAudioSample.inf' | Select-Object -First 1
if (-not $sys -or -not $inf) { throw "TabletAudioSample.sys or ComponentizedAudioSample.inf not found under $Build" }
Write-Host "SysVAD build: $($sys.FullName), $($inf.FullName)"

$work = Join-Path $env:RUNNER_TEMP 'sysvad-package'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $work | Out-Null
Copy-Item $sys.FullName (Join-Path $work 'tabletaudiosample.sys')
$section = ''
$lines = foreach ($line in Get-Content -LiteralPath $inf.FullName) {
    if ($line -match '^\s*\[(.+)\]') { $section = $Matches[1] }
    if ($section -like 'SignatureAttributes*') { continue }
    if ($line -match '(?i)keyworddetectorcontosoadapter\.dll') { continue }
    # The sample's models need Windows 11 22H2 (build 22621); the runner is
    # Server 2022 (20348). Our INF's floor, 19041, lets it match.
    $line -replace '(?i),\s*KEYWORDDETECTORCONTOSOADAPTER\.(CopyList|AddReg)', '' -replace '\.10\.0\.\.\.22621', '.10.0...19041'
}
$lines | Set-Content -LiteralPath (Join-Path $work 'sysvad.inf') -Encoding unicode

$signtool = Find-KitTool 'signtool.exe' 'x64'
$inf2cat = Find-KitTool 'Inf2Cat.exe' 'x86'
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=BIO Atmos CI SysVAD control' `
    -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddDays(2)
foreach ($store in 'Root', 'TrustedPublisher') {
    $s = New-Object System.Security.Cryptography.X509Certificates.X509Store($store, 'LocalMachine')
    $s.Open('ReadWrite'); $s.Add($cert); $s.Close()
}
$sign = @('sign', '/fd', 'sha256', '/sm', '/s', 'My', '/sha1', $cert.Thumbprint)
Invoke-Checked $signtool ($sign + (Join-Path $work 'tabletaudiosample.sys'))
Invoke-Checked $inf2cat @("/driver:$work", '/os:10_VB_X64,ServerFE_X64', '/uselocaltime')
Get-ChildItem -Path $work -Filter '*.cat' | ForEach-Object { Invoke-Checked $signtool ($sign + $_.FullName) }
Invoke-Checked pnputil.exe @('/add-driver', (Join-Path $work 'sysvad.inf'))

Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'RootDevice.cs'))
$reboot = [RootDevice]::Install((Join-Path $work 'sysvad.inf'), $HardwareId)
Write-Host "SysVAD installed (reboot requested: $reboot)"

$deadline = (Get-Date).AddSeconds(60)
do {
    $device = Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains $HardwareId } | Select-Object -First 1
    if ($device -and $device.Status -eq 'OK') { break }
    Start-Sleep -Seconds 1
} while ((Get-Date) -lt $deadline)
if (-not $device) { throw "no device with hardware id $HardwareId was created" }
$problem = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName DEVPKEY_Device_ProblemCode).Data
Write-Host "SysVAD device $($device.InstanceId): status $($device.Status), problem code $problem"
"instance=$($device.InstanceId)" | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8
