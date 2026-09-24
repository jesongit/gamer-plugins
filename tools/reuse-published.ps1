# Preserve immutable bytes for unchanged plugin versions after verifying every entry.
[CmdletBinding()]
param([string]$OutputDir = 'dist', [string]$SourceDir = '')
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$lock = Get-Content (Join-Path $repo 'release-reuse.lock.json') -Raw | ConvertFrom-Json
$registryPath = Join-Path $OutputDir 'registry.json'
$registry = Get-Content $registryPath -Raw | ConvertFrom-Json
Add-Type -AssemblyName System.IO.Compression.FileSystem
function Entry-Hashes([string]$Path) {
    $zip = [IO.Compression.ZipFile]::OpenRead([IO.Path]::GetFullPath($Path))
    try {
        $result = @{}
        foreach ($entry in $zip.Entries) {
            if ($result.ContainsKey($entry.FullName)) { throw 'Duplicate ZIP entry' }
            $stream = $entry.Open()
            $hash = [Security.Cryptography.SHA256]::Create()
            try { $result[$entry.FullName] = [BitConverter]::ToString($hash.ComputeHash($stream)) }
            finally { $hash.Dispose(); $stream.Dispose() }
        }
        return $result
    } finally { $zip.Dispose() }
}
foreach ($p in $lock.plugins) {
    if ($p.id -notmatch '^gamer-(keymap|video)$' -or $p.version -notmatch '^[0-9A-Za-z.-]+$') { throw 'Invalid reuse identity' }
    $name = "$($p.id)-$($p.version).gplugin"
    if ($p.download_url -notmatch '^https://github.com/jesongit/gamer-plugins/releases/download/[^/]+/[^/]+$' -or -not $p.download_url.EndsWith("/$name")) { throw 'Invalid reuse URL' }
    $entry = @($registry.plugins | Where-Object { $_.id -eq $p.id -and $_.version -eq $p.version })
    if ($entry.Count -ne 1) { throw "Update reuse lock when changing $($p.id) version" }
    $download = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString() + '.gplugin')
    try {
        if ($SourceDir) { Copy-Item -LiteralPath (Join-Path $SourceDir $name) -Destination $download }
        else { Invoke-WebRequest -Uri $p.download_url -OutFile $download }
        if ((Get-Item $download).Length -ne $p.size -or (Get-FileHash $download -Algorithm SHA256).Hash.ToLowerInvariant() -ne $p.sha256) { throw "Published hash mismatch: $name" }
        $built = Join-Path $OutputDir "plugins/$name"
        $before = Entry-Hashes $download
        $after = Entry-Hashes $built
        if ($before.Count -ne $after.Count) { throw "Changed contents require a new version: $name" }
        foreach ($key in $before.Keys) {
            if ($before[$key] -ne $after[$key]) { throw "Changed entry requires a new version: $name / $key" }
        }
        Copy-Item -LiteralPath $download -Destination $built -Force
        $entry[0].sha256 = $p.sha256
        $entry[0].size = $p.size
        Write-Host "PASS: reused immutable published bytes after entry comparison: $name"
    } finally { if (Test-Path $download) { Remove-Item -LiteralPath $download } }
}
[IO.File]::WriteAllText([IO.Path]::GetFullPath($registryPath), ($registry | ConvertTo-Json -Depth 30) + "`n", [Text.UTF8Encoding]::new($false))
