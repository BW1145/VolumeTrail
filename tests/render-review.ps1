param([switch]$SkipBuild)
$ErrorActionPreference='Stop'
$repo=Split-Path $PSScriptRoot -Parent
if (!$SkipBuild) {
    & (Join-Path $repo 'build.ps1') build -CargoArgs @('--locked','--features','ui-capture','--example','ui_capture')
    if ($LASTEXITCODE -ne 0) {throw 'Capture harness build failed'}
}
$exe=Join-Path $repo 'target\debug\examples\ui_capture.exe'
$output=Join-Path $repo 'evidence\ui'
New-Item -ItemType Directory -Force -Path $output | Out-Null
# Application-level synthetic state and offscreen native framebuffer rendering only.
# No native mouse/keyboard injection. Each backend sample uses its own newly created fixture.
$scenes=@(
    @('overview','1050','720','1.0','polish-overview'),
    @('preview','760','520','1.25','polish-form-125'),
    @('long-path','1050','900','1.0','polish-long-path'),
    @('preview-expanded','760','720','1.0','polish-candidates-expanded'),
    @('preview-confirmed','760','520','1.0','polish-confirmed'),
    @('preview-bottom','760','520','1.25','polish-bottom-125'),
    @('result-details','760','520','1.0','polish-result-details'),
    @('partial','760','520','1.0','polish-partial'),
    @('interrupted','760','520','1.0','polish-interrupted')
)
foreach ($scene in $scenes) {
    $image=Join-Path $output ($scene[4]+'.png')
    $process=Start-Process -FilePath $exe -ArgumentList @(('"'+$image+'"'),$scene[0],$scene[1],$scene[2],$scene[3]) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $output ($scene[4]+'.stderr.txt'))
    if (!$process.WaitForExit(20000)) {throw "Capture still running: $($process.Id)"}
    if ($process.ExitCode -ne 0) {throw "Capture failed: $($scene[0])"}
    Get-Item -LiteralPath $image | Select-Object Name,Length
}
