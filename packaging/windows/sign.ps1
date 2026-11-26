# Sign Windows executables with Authenticode (SHA-256, RFC 3161 timestamp).
#   pwsh packaging/windows/sign.ps1 <file.exe> [<file.exe> ...]
# Uses the code-signing certificate in WINDOWS_CERT_PFX_BASE64 (a .pfx with its private key,
# base64-encoded) and WINDOWS_CERT_PASSWORD. Without them it signs nothing and says so, so
# builds without the secrets stay unsigned. WINDOWS_TEST_SIGNING=true signs with a throwaway
# self-signed certificate instead, to try the signing steps on CI without a real one.
param([Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)][string[]]$Files)
$ErrorActionPreference = 'Stop'

$pfx = Join-Path ([IO.Path]::GetTempPath()) 'only3d-signing.pfx'
$password = $env:WINDOWS_CERT_PASSWORD
if ($env:WINDOWS_CERT_PFX_BASE64) {
    [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64))
} elseif ($env:WINDOWS_TEST_SIGNING -eq 'true') {
    $password = [guid]::NewGuid().ToString()
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=only3d test signing' `
        -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddDays(2)
    $secure = ConvertTo-SecureString -String $password -Force -AsPlainText
    Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $secure | Out-Null
    Write-Host 'Signing with a throwaway self-signed certificate (WINDOWS_TEST_SIGNING)'
} else {
    Write-Host "WINDOWS_CERT_PFX_BASE64 not set: leaving $($Files -join ', ') unsigned"
    exit 0
}

# the newest signtool of the Windows SDKs installed on the runner
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
    Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
if (-not $signtool) { throw 'signtool.exe not found (Windows SDK)' }
$timestamp = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }

try {
    & $signtool.FullName sign /fd SHA256 /f $pfx /p $password /tr $timestamp /td SHA256 /d only3d $Files
    if ($LASTEXITCODE -ne 0) { throw "signtool sign failed ($LASTEXITCODE)" }
    if ($env:WINDOWS_CERT_PFX_BASE64) {
        # a real certificate chains to a trusted root; the throwaway one doesn't, so it's only listed
        & $signtool.FullName verify /pa /v $Files
        if ($LASTEXITCODE -ne 0) { throw "signtool verify failed ($LASTEXITCODE)" }
    } else {
        $sigs = Get-AuthenticodeSignature $Files
        $sigs | Format-Table -AutoSize Path, Status, StatusMessage
        if ($sigs | Where-Object { $_.Status -eq 'NotSigned' }) { throw 'a file has no signature' }
    }
} finally {
    Remove-Item -Force $pfx -ErrorAction SilentlyContinue
}
