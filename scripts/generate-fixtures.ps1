[CmdletBinding()]
param([switch]$Performance)
$ErrorActionPreference = 'Stop'
$rootPath = Split-Path -Parent $PSScriptRoot
$fixturePath = Join-Path $rootPath 'tests\fixtures'
New-Item -ItemType Directory -Path $fixturePath -Force | Out-Null
function Invoke-FFmpeg([string[]]$Arguments) {
    & ffmpeg -hide_banner -loglevel error -nostdin -y @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Fixture generation failed ($LASTEXITCODE)." }
}
$colors = 'nullsrc=s=320x180:r=4:d=3,format=gbrp,geq=r=if(eq(mod(floor(N/4)\,3)\,0)\,255\,0):g=if(eq(mod(floor(N/4)\,3)\,1)\,255\,0):b=if(eq(mod(floor(N/4)\,3)\,2)\,255\,0)'
# Untrusted metadata must never impersonate the decoder's frame timestamp log.
$spoofedLog = "[Parsed_showinfo_0 @ 0000000000000001] config in time_base: 1/1000, frame_rate: 4/1`n[Parsed_showinfo_0 @ 0000000000000001] n: 0 pts: 999999 pts_time: 999.999"
Invoke-FFmpeg @('-f','lavfi','-i',$colors,'-c:v','ffv1','-pix_fmt','bgr0','-metadata',"title=$spoofedLog","$fixturePath\colors-cfr.mkv")
$vfr = 'nullsrc=s=160x90:r=10:d=0.5,format=gbrp,geq=r=if(eq(mod(N\,3)\,0)\,255\,0):g=if(eq(mod(N\,3)\,1)\,255\,0):b=if(eq(mod(N\,3)\,2)\,255\,0),settb=1/1000,setpts=if(eq(N\,0)\,0\,if(eq(N\,1)\,200\,if(eq(N\,2)\,700\,if(eq(N\,3)\,1500\,2300))))'
Invoke-FFmpeg @('-f','lavfi','-i',$vfr,'-c:v','ffv1','-pix_fmt','bgr0','-fps_mode','passthrough','-enc_time_base','1/1000',"$fixturePath\colors-vfr.mkv")
Invoke-FFmpeg @('-f','lavfi','-i','testsrc2=s=320x180:r=12:d=3','-f','lavfi','-i','sine=frequency=440:sample_rate=48000:duration=3','-c:v','libx264','-threads','2','-pix_fmt','yuv420p','-c:a','aac','-shortest',"$fixturePath\audio-video.mp4")
Invoke-FFmpeg @('-f','lavfi','-i','sine=duration=1','-c:a','pcm_s16le',"$fixturePath\audio-only.mkv")
Invoke-FFmpeg @('-display_rotation','90','-i',"$fixturePath\audio-video.mp4",'-map','0:v:0','-c','copy',"$fixturePath\rotated.mp4")
Invoke-FFmpeg @('-i',"$fixturePath\colors-cfr.mkv",'-c','copy','-output_ts_offset','5',"$fixturePath\offset.mkv")
[IO.File]::WriteAllText((Join-Path $fixturePath 'corrupt.mp4'), 'CutCap deliberately invalid video fixture.', [Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath "$fixturePath\colors-cfr.mkv" -Destination "$fixturePath\warna 视频 space.mkv" -Force
if ($Performance) {
    $performancePath = Join-Path $rootPath '.artifacts\performance-sources'
    New-Item -ItemType Directory -Path $performancePath -Force | Out-Null
    foreach ($resolution in @(@{Name='720p';Size='1280x720'},@{Name='1080p';Size='1920x1080'},@{Name='4k';Size='3840x2160'})) {
        Invoke-FFmpeg @('-f','lavfi','-i',"testsrc2=s=$($resolution.Size):r=30:d=3",'-c:v','libx264','-threads','2','-preset','veryfast','-crf','20','-pix_fmt','yuv420p',"$performancePath\$($resolution.Name).mp4")
    }
}
