# Runs only on an ephemeral GitHub-hosted Windows VM. Never select a host disk.
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$evidence = Join-Path $repo 'evidence\ntfs-ci'
New-Item -ItemType Directory -Path $evidence -Force | Out-Null
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'This fixture may run only on an ephemeral GitHub-hosted runner.'
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    @{ status='not_run'; reason='Runner token is not administrator' } | ConvertTo-Json | Set-Content (Join-Path $evidence 'preflight.json')
    throw 'Isolated NTFS validation not run: administrator token unavailable.'
}
$tempBase = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\')
$fixture = [IO.Path]::GetFullPath((Join-Path $tempBase ('volumetrail-ntfs-' + [guid]::NewGuid().ToString('N'))))
if (!$fixture.StartsWith($tempBase + '\', [StringComparison]::OrdinalIgnoreCase) -or $fixture.Contains('"') -or $fixture.Contains("`n")) {
    throw 'Invalid isolated fixture path.'
}
New-Item -ItemType Directory -Path $fixture | Out-Null
$image = Join-Path $fixture 'fixture.vhd'
$token = [guid]::NewGuid().ToString('N')
$letter = @('R','S','T','U','V','W','X','Y','Z') | Where-Object { !(Test-Path -LiteralPath "${_}:\") } | Select-Object -First 1
if (!$letter) { throw 'No free fixture drive letter.' }
$root = "${letter}:\"
$createScript = Join-Path $fixture 'create.txt'
@"
create vdisk file="$image" maximum=256 type=expandable
select vdisk file="$image"
attach vdisk
convert mbr
create partition primary
format fs=ntfs quick label="VolumeTrailCI"
assign letter=$letter
exit
"@ | Set-Content -LiteralPath $createScript -Encoding ascii
try {
    & diskpart /s $createScript | Tee-Object -FilePath (Join-Path $evidence 'diskpart-create.txt')
    if ($LASTEXITCODE -ne 0) { throw 'VHD creation failed.' }
    $volume = Get-Volume -DriveLetter $letter
    $partition = Get-Partition -DriveLetter $letter
    $mountedImage = Get-DiskImage -DevicePath "\\.\PhysicalDrive$($partition.DiskNumber)"
    if ($volume.FileSystem -ne 'NTFS' -or $volume.FileSystemLabel -ne 'VolumeTrailCI' -or
        ![IO.Path]::GetFullPath($mountedImage.ImagePath).Equals($image,[StringComparison]::OrdinalIgnoreCase)) {
        throw 'Fixture drive is not the newly created NTFS VHD; refusing journal operations.'
    }
    $marker = Join-Path $root ('.volumetrail-ci-' + $token + '.marker')
    Set-Content -LiteralPath $marker -Value $token -Encoding ascii -NoNewline
    & fsutil usn createjournal m=8388608 a=1048576 "${letter}:"
    if ($LASTEXITCODE -ne 0) { throw 'Cannot create USN journal on isolated fixture.' }
    $data = Join-Path $fixture 'data'
    New-Item -ItemType Directory -Path $data | Out-Null
    $env:VT_NTFS_FIXTURE_DRIVE = $letter
    $env:VT_NTFS_FIXTURE_TOKEN = $token
    $env:VT_NTFS_FIXTURE_MARKER = $marker
    $env:VT_NTFS_FIXTURE_DATA = $data
    $env:VT_NTFS_FIXTURE_REPORT = Join-Path $evidence 'runtime.json'
    @{ status='ready'; administrator=$true; isolated_vhd=$true; drive=$letter; label=$volume.FileSystemLabel } |
        ConvertTo-Json | Set-Content (Join-Path $evidence 'preflight.json')
    & (Join-Path $repo 'build.ps1') test -CargoArgs @('--locked','--test','ntfs_runtime','--','--ignored','--nocapture')
    if ($LASTEXITCODE -ne 0) { throw 'Isolated NTFS runtime test failed.' }
} finally {
    # Detach only the literal VHD path this job created. No host disk/volume selection.
    if (Test-Path -LiteralPath $image) {
        $detachScript = Join-Path $fixture 'detach.txt'
        "select vdisk file=`"$image`"`ndetach vdisk`nexit" | Set-Content -LiteralPath $detachScript -Encoding ascii
        & diskpart /s $detachScript | Tee-Object -FilePath (Join-Path $evidence 'diskpart-detach.txt')
        $resolvedImage = [IO.Path]::GetFullPath($image)
        if ($resolvedImage.StartsWith($fixture + '\',[StringComparison]::OrdinalIgnoreCase) -and
            !(Get-DiskImage -ImagePath $resolvedImage).Attached) {
            Remove-Item -LiteralPath $resolvedImage -Force
        }
    }
    Remove-Item Env:VT_NTFS_FIXTURE_DRIVE,Env:VT_NTFS_FIXTURE_TOKEN,Env:VT_NTFS_FIXTURE_MARKER,Env:VT_NTFS_FIXTURE_DATA,Env:VT_NTFS_FIXTURE_REPORT -ErrorAction SilentlyContinue
}
