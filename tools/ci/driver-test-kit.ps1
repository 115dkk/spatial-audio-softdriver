#Requires -RunAsAdministrator
# Builds the signed x64 driver test kit used by CI and test-signing PCs.
param(
    [Parameter(Mandatory)] [string] $Package,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $OutDir,
    [ValidateSet('x64')] [string] $Arch = 'x64'
)
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'kit-tools.ps1')

function Invoke-Checked([string] $FilePath, [string[]] $Arguments) {
    Write-Host "> $FilePath $($Arguments -join ' ')"
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path -Leaf $FilePath) failed ($LASTEXITCODE)" }
}

function Add-CertificateToStore($Certificate, [string] $Name) {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($Name, 'LocalMachine')
    try { $store.Open('ReadWrite'); $store.Add($Certificate) } finally { $store.Close() }
}

function Remove-CertificateFromStore([string] $Thumbprint, [string] $Name) {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store($Name, 'LocalMachine')
    try {
        $store.Open('ReadWrite')
        foreach ($item in @($store.Certificates.Find('FindByThumbprint', $Thumbprint, $false))) {
            $store.Remove($item)
        }
    } finally { $store.Close() }
}

$signtool = Find-KitTool 'signtool.exe' 'x64'
$inf2cat = Find-KitTool 'Inf2Cat.exe' 'x86'
$packagePath = (Resolve-Path $Package).Path
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$kitName = "bio-atmos-driver-test-$Version-$Arch"
$kit = Join-Path $OutDir $kitName
Remove-Item -Recurse -Force $kit -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $kit | Out-Null
Copy-Item (Join-Path $packagePath 'bioatmos.sys'), (Join-Path $packagePath 'bioatmos.inf') $kit

$cert = $null
$subject = $null
$notAfter = $null
$thumbprint = $null
try {
    $cert = New-SelfSignedCertificate -Type CodeSigningCert `
        -Subject "CN=BIO Atmos test signing $Version" `
        -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddYears(10)
    $subject = $cert.Subject
    $notAfter = $cert.NotAfter
    $thumbprint = $cert.Thumbprint
    Export-Certificate -Cert $cert -FilePath (Join-Path $kit 'bioatmos-test.cer') -Type CERT | Out-Null
    foreach ($store in 'Root', 'TrustedPublisher') { Add-CertificateToStore $cert $store }

    $sign = @('sign', '/fd', 'sha256', '/sm', '/s', 'My', '/sha1', $thumbprint)
    Invoke-Checked $signtool ($sign + (Join-Path $kit 'bioatmos.sys'))
    Invoke-Checked $inf2cat @("/driver:$kit", '/os:10_VB_X64,ServerFE_X64', '/uselocaltime')
    Invoke-Checked $signtool ($sign + (Join-Path $kit 'bioatmos.cat'))
    Invoke-Checked $signtool @('verify', '/pa', '/v', (Join-Path $kit 'bioatmos.sys'))
    Invoke-Checked $signtool @('verify', '/pa', '/v', '/c', (Join-Path $kit 'bioatmos.cat'), (Join-Path $kit 'bioatmos.sys'))
} finally {
    if ($thumbprint) {
        foreach ($store in 'Root', 'TrustedPublisher') { Remove-CertificateFromStore $thumbprint $store }
        # -DeleteKey removes the private key with the certificate: nothing can
        # sign with this certificate once the kit is built.
        Remove-Item "Cert:\LocalMachine\My\$thumbprint" -DeleteKey -Force -ErrorAction SilentlyContinue
        if (Test-Path "Cert:\LocalMachine\My\$thumbprint") { throw "the signing certificate $thumbprint is still in LocalMachine\My" }
    }
}

foreach ($name in 'install.ps1', 'uninstall.ps1', 'collect.ps1') {
    Copy-Item (Join-Path $repo "tools\testkit\$name") $kit
}
foreach ($name in 'RootDevice.cs', 'driver_flow_check.py', 'ring_record.py', 'requirements.txt') {
    Copy-Item (Join-Path $repo "tools\ci\$name") $kit
}
$commit = $env:GITHUB_SHA
if (-not $commit) {
    $commit = (& git -C $repo rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'git rev-parse HEAD failed' }
}

# The guide becomes the kit's README. Its links are relative to docs/testing/
# in the repository; inside the kit they point at the same commit on GitHub.
$readme = Join-Path $repo 'docs\testing\test-signed-pc.md'
if (-not (Test-Path $readme)) { throw "the testing guide $readme is missing" }
$server = if ($env:GITHUB_SERVER_URL) { $env:GITHUB_SERVER_URL } else { 'https://github.com' }
$repository = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { '115dkk/spatial-audio-softdriver' }
$base = "$server/$repository/blob/$commit"
$text = [IO.File]::ReadAllText($readme, [Text.Encoding]::UTF8)
$text = $text.Replace('](../../', "]($base/").Replace('](../', "]($base/docs/")
[IO.File]::WriteAllText((Join-Path $kit 'README.md'), $text, (New-Object Text.UTF8Encoding($false)))
$hashes = [ordered]@{}
foreach ($name in 'bioatmos.sys', 'bioatmos.cat', 'bioatmos.inf', 'bioatmos-test.cer') {
    $hashes[$name] = (Get-FileHash -Algorithm SHA256 (Join-Path $kit $name)).Hash.ToLowerInvariant()
}
$manifest = [ordered]@{
    version = $Version
    arch = $Arch
    commit = $commit
    certificate = [ordered]@{
        subject = $subject
        thumbprint = $thumbprint
        not_after = $notAfter.ToUniversalTime().ToString('o')
    }
    sha256 = $hashes
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 (Join-Path $kit 'kit.json')

$zip = Join-Path $OutDir "$kitName.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $kit -DestinationPath $zip -CompressionLevel Optimal
$zip = (Resolve-Path $zip).Path
if ($env:GITHUB_OUTPUT) {
    Add-Content -Path $env:GITHUB_OUTPUT -Value "zip=$zip"
    Add-Content -Path $env:GITHUB_OUTPUT -Value "thumbprint=$thumbprint"
}
Write-Host "test kit: $zip"
