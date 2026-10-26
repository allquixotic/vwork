param([Parameter(Mandatory=$true)][string]$Binary, [Parameter(Mandatory=$true)][string]$Evidence)
$ErrorActionPreference = 'Stop'
$signature = Get-AuthenticodeSignature -LiteralPath $Binary
if ([string]::IsNullOrWhiteSpace($env:VWORK_WINDOWS_PUBLISHER) -or
    $signature.Status -ne 'Valid' -or
    $signature.SignerCertificate.Subject -ne $env:VWORK_WINDOWS_PUBLISHER -or
    $null -eq $signature.TimeStamperCertificate) {
    throw "Missing, invalid, untimestamped or unexpected signature: $($signature.Status)"
}
$record = @{
    status = 'Valid'
    publisher = $signature.SignerCertificate.Subject
    certificate_sha1 = $signature.SignerCertificate.Thumbprint
    timestamp_publisher = $signature.TimeStamperCertificate.Subject
    binary_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Binary).Hash.ToLowerInvariant()
    source_commit = $env:GITHUB_SHA
    workflow_run = $env:GITHUB_RUN_ID
}
$record | ConvertTo-Json | Set-Content -Encoding utf8 $Evidence
Write-Host "Verified timestamped Authenticode signature: $($record.publisher)"
