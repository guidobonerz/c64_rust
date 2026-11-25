# The sizes of all examples that have a C port: rust64 --release against
# oscar64 -Os, -O2 and -O3 (see README.md). The cycles of the programs
# that can be timed are measured by drawing/run.ps1, effects.ps1 and
# math.ps1.
#
#   pwsh bench/examples.ps1
#
# Tools: OSCAR64 (default: oscar64 on the PATH), RUST64_CARGO_C64.

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$repo = Resolve-Path "$here\.."
$oscar = if ($env:OSCAR64) { $env:OSCAR64 } else { 'oscar64' }
$cargoC64 = if ($env:RUST64_CARGO_C64) { $env:RUST64_CARGO_C64 } else {
    @("$repo\target\debug\cargo-c64.exe", "$repo\target\release\cargo-c64.exe") |
        Where-Object { (Test-Path $_) -and (Test-Path (Join-Path (Split-Path $_) 'rustc_codegen_rust64.dll')) } |
        Select-Object -First 1
}

# the example and its C source
$programs = [ordered]@{
    border     = 'examples\oscar64\border.c'
    hello      = 'examples\oscar64\hello.c'
    joystick   = 'examples\oscar64\joystick.c'
    ui         = 'examples\oscar64\ui.c'
    music      = 'examples\oscar64\music.c'
    sprites    = 'examples\oscar64\sprites.c'
    rasterbars = 'rasterbars\oscar64\rasterbars.c'
    scroller   = 'scroller\oscar64\scroller.c'
    drawing    = 'drawing\oscar64\drawing.c'
    diagonal   = 'diagonal\oscar64\diagonal.c'
    mandelbrot = 'math\oscar64\mandelbrot.c'
    julia      = 'math\oscar64\julia.c'
    pi         = 'math\oscar64\pi.c'
    dir        = 'examples\oscar64\dir.c'
    sd2iec     = 'examples\oscar64\sd2iec.c'
}

"{0,-12} {1,7} {2,7} {3,7} {4,7}   {5}" -f 'example', 'rust64', '-Os', '-O2', '-O3', 'rust64 against -Os'
$total = @(0, 0, 0, 0)
foreach ($name in $programs.Keys) {
    Push-Location "$repo\examples\$name"
    & $cargoC64 build --release 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "cargo c64 build failed for $name" }
    Pop-Location
    $sizes = @((Get-Item "$repo\examples\$name\target\mos-c64\release\$name.prg").Length)
    $source = "$here\$($programs[$name])"
    $out = Join-Path (Split-Path $source) 'out'
    New-Item -ItemType Directory -Force $out | Out-Null
    foreach ($opt in '-Os', '-O2', '-O3') {
        $prg = "$out\$name$opt.prg"
        & $oscar $opt "-o=$prg" $source 2>&1 | Out-Null
        $sizes += (Get-Item $prg).Length
    }
    for ($i = 0; $i -lt 4; $i++) { $total[$i] += $sizes[$i] }
    $pct = [math]::Round(($sizes[0] - $sizes[1]) * 100 / $sizes[1])
    $word = if ($pct -lt 0) { "$(-$pct)% smaller" } elseif ($pct -gt 0) { "$pct% larger" } else { 'the same' }
    "{0,-12} {1,7} {2,7} {3,7} {4,7}   {5}" -f $name, $sizes[0], $sizes[1], $sizes[2], $sizes[3], $word
}
$pct = [math]::Round(($total[0] - $total[1]) * 100 / $total[1])
"{0,-12} {1,7} {2,7} {3,7} {4,7}   {5}" -f 'total', $total[0], $total[1], $total[2], $total[3], "$pct% larger"
