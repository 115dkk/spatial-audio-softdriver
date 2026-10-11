$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Driver installation is restricted to a disposable GitHub-hosted runner.'
}
$evidence = Join-Path $env:GITHUB_WORKSPACE 'out/runner'
Start-Transcript -Path "$evidence/install.txt"
try {
    bcdedit /enum '{current}'
    foreach ($service in 'AudioEndpointBuilder', 'Audiosrv') {
        Set-Service $service -StartupType Automatic
        Start-Service $service
    }
    $cable = Join-Path $env:RUNNER_TEMP 'vbcable'
    $archive = "$cable.zip"
    Invoke-WebRequest 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip' -OutFile $archive
    $hash = (Get-FileHash $archive -Algorithm SHA256).Hash
    if ($hash -ne 'B950E39F01AF1D04EA623C8F6D8EB9B6EA5C477C637295FABF20631C85116BFB') { throw 'VB-CABLE hash mismatch' }
    Expand-Archive $archive $cable
    $installer = Start-Process "$cable/VBCABLE_Setup_x64.exe" -ArgumentList '-i','-h' -PassThru -WindowStyle Hidden
    if (!$installer.WaitForExit(120000)) { $installer.Kill(); throw 'VB-CABLE installer timed out' }
    "VB-CABLE installer exit: $($installer.ExitCode)"
    pnputil /scan-devices
    Restart-Service Audiosrv -Force
    Get-CimInstance Win32_PnPEntity | Where-Object Name -Match 'CABLE' |
        Select-Object Name,Status,ConfigManagerErrorCode | Format-Table

    # SAL is an opaque, pinned build input (ADR 0005). Never print source,
    # patches, compiler snippets or package its implementation in artifacts.
    $sal = Join-Path $env:RUNNER_TEMP 'sal-blackbox'
    $revision = '5c689815680f3ea5b09fb35ea4dbe63a3dc5bfb7'
    git clone --quiet https://github.com/barralutz/spatial-audio-lab.git $sal
    if ($LASTEXITCODE) { throw 'SAL clone failed' }
    git -C $sal checkout --quiet $revision
    if ($LASTEXITCODE) { throw 'SAL checkout failed' }
    git -C $sal submodule update --init --recursive --depth 1 driver/windows-driver-samples
    if ($LASTEXITCODE) { throw 'SAL driver submodule checkout failed' }
    "SAL revision: $revision" | Set-Content "$evidence/sal-provenance.txt"
    Push-Location $sal
    try {
        # Commands published in upstream driver/README.md; logs remain private
        # because compiler diagnostics can contain implementation excerpts.
        $env:SAL_ERROR_FILE = "$evidence/sal-error.txt"
        & pwsh -NoProfile -NonInteractive -Command '$ErrorActionPreference="Stop"; try { & ./tools/Initialize-SpatialAudioLab.ps1 -Component Driver } catch { $_.Exception.Message | Set-Content $env:SAL_ERROR_FILE; exit 1 }' *> "$env:RUNNER_TEMP/sal-initialize.log"
        if ($LASTEXITCODE) { if (Test-Path $env:SAL_ERROR_FILE) { Get-Content $env:SAL_ERROR_FILE }; throw "SAL initialize failed (exit $LASTEXITCODE)" }
        $wdkHeader = "${env:ProgramFiles(x86)}/Windows Kits/10/Include/10.0.26100.0/km/ntddk.h"
        if (!(Test-Path $wdkHeader)) { throw 'The runner is missing the required 26100 WDK' }
        "Using the runner's installed WDK: $wdkHeader"
        # The upstream installer requires winget, which Server 2022 lacks.
        # This image already has SDK/WDK 26100; use that toolchain directly.
        $vswhere = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
        $msbuild = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -find 'MSBuild/**/Bin/amd64/MSBuild.exe' | Select-Object -First 1
        $solutionDir = "$sal/driver/windows-driver-samples/audio/sysvad/"
        $outputDir = "${solutionDir}x64/Release/"
        foreach ($project in 'EndpointsCommon/EndpointsCommon.vcxproj','Package/package.vcxproj') {
            & $msbuild "driver/windows-driver-samples/audio/sysvad/$project" /m:1 /t:Build /p:Configuration=Release /p:Platform=x64 /p:TargetPlatformVersion=10.0.26100.0 "/p:SolutionDir=$solutionDir" /p:SignMode=Off /v:quiet *> "$env:RUNNER_TEMP/sal-build.log"
            if ($LASTEXITCODE) {
                # Error identifiers only; never expose SAL lines or snippets.
                $codes = Select-String -Path "$env:RUNNER_TEMP/sal-build.log" -Pattern '(?:error|fatal error) ([A-Z]+\d+)' -AllMatches |
                    ForEach-Object { $_.Matches.Groups[1].Value } | Sort-Object -Unique
                # Missing include names are dependency metadata, not code.
                Select-String -Path "$env:RUNNER_TEMP/sal-build.log" -Pattern "fatal error C1083: ([^\r\n]+)" -AllMatches |
                    ForEach-Object { $_.Matches.Groups[1].Value } | Set-Content "$evidence/missing-includes.txt"
                Select-String -Path "$env:RUNNER_TEMP/sal-build.log" -Pattern '(?:SignTool Error:|error(?: [A-Z]+\d+)?:) [^\r\n]+' -AllMatches |
                    ForEach-Object { $_.Matches.Value } | Set-Content "$evidence/build-errors.txt"
                throw "SAL build failed: $project; codes=$($codes -join ','); exit=$LASTEXITCODE"
            }
        }
        # Generate an ephemeral test certificate in the disposable runner and
        # sign the catalogs explicitly: no Visual Studio user signing profile.
        $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=BIO SAL runner experiment' -CertStoreLocation Cert:/LocalMachine/My
        Export-Certificate -Cert $certificate -FilePath "${outputDir}package.cer" | Out-Null
        $signtool = "${env:ProgramFiles(x86)}/Windows Kits/10/bin/10.0.26100.0/x64/signtool.exe"
        $catalogs = @(Get-ChildItem $outputDir -Recurse -Filter *.cat)
        if (!$catalogs.Count) { throw 'SAL build produced no catalogs' }
        foreach ($catalog in $catalogs) {
            & $signtool sign /fd SHA256 /sm /s My /sha1 $certificate.Thumbprint $catalog.FullName
            if ($LASTEXITCODE) { throw "Test signing failed: $($catalog.Name)" }
        }
        Get-ChildItem $outputDir -Recurse -File -Include *.sys,*.cat,*.inf,*.cer,*.dll |
            Get-FileHash -Algorithm SHA256 | Select-Object Path,Hash | ConvertTo-Json > "$evidence/sal-package-hashes.json"
        & pwsh -NoProfile -NonInteractive -Command '$ErrorActionPreference="Stop"; try { & ./tools/Install-SysvadCapture.ps1 } catch { $_.Exception.Message | Set-Content $env:SAL_ERROR_FILE; exit 1 }' *> "$evidence/sal-install.txt"
        if ($LASTEXITCODE) { if (Test-Path $env:SAL_ERROR_FILE) { Get-Content $env:SAL_ERROR_FILE }; throw "SAL install failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
} catch {
    $_.Exception.Message | Set-Content "$evidence/driver-failure.txt"
    throw
} finally { Stop-Transcript }
