# Size and speed of the arithmetic examples: rust64 against oscar64.
# The Mandelbrot set and a Julia set (16-bit fixed point, 32-bit products),
# each on the text screen (PETSCII) and on the multicolour bitmap, and the
# first 100 digits of pi (32-bit multiplication and division).
#
# Builds both sides, runs the timed variants in mimic-headless and prints
# the program sizes, the measured cycles and a checksum of the result (the
# sum of all rounds, or of the groups of digits): it must be the same in
# every row of a program.
#
#   pwsh bench/math.ps1            everything; the bitmaps take a while
#   pwsh bench/math.ps1 -Quick     without the multicolour bitmaps
#
# Tools: as for bench/drawing/run.ps1 (OSCAR64, RUST64_CARGO_C64,
# RUST64_MIMIC_HEADLESS, EMU_ROM_DIR).
param([switch]$Quick)

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$repo = Resolve-Path "$here\.."
$oscar = if ($env:OSCAR64) { $env:OSCAR64 } else { 'oscar64' }
$cargoC64 = if ($env:RUST64_CARGO_C64) { $env:RUST64_CARGO_C64 } else {
    @("$repo\target\debug\cargo-c64.exe", "$repo\target\release\cargo-c64.exe") |
        Where-Object { (Test-Path $_) -and (Test-Path (Join-Path (Split-Path $_) 'rustc_codegen_rust64.dll')) } |
        Select-Object -First 1
}
$mimic = if ($env:RUST64_MIMIC_HEADLESS) { $env:RUST64_MIMIC_HEADLESS } else { "$repo\..\mimic\target\release\mimic-headless.exe" }
$roms = if ($env:EMU_ROM_DIR) { $env:EMU_ROM_DIR } else { "$HOME\.mimic\roms" }
$chargen = Get-ChildItem "$roms\c64\chargen*" | Select-Object -First 1
$screen = "$here\drawing\screen.js"

$out = "$here\math\oscar64\out"
$rustOut = "$here\math\rust64-out"
New-Item -ItemType Directory -Force $out, $rustOut | Out-Null

# name, the example, its features / defines, frames to run (50 per second)
$variants = @(
    @{ Name = 'pi'; Source = 'pi'; Features = @(); Defs = @(); Frames = 1500 }
    @{ Name = 'mandelbrot petscii'; Source = 'mandelbrot'; Features = @(); Defs = @(); Frames = 3000 }
    @{ Name = 'julia petscii'; Source = 'julia'; Features = @(); Defs = @(); Frames = 3000 }
)
if (-not $Quick) {
    $variants += @(
        @{ Name = 'mandelbrot multicolor'; Source = 'mandelbrot'; Features = @('multicolor'); Defs = @('-dMULTICOLOR'); Frames = 60000 }
        @{ Name = 'julia multicolor'; Source = 'julia'; Features = @('multicolor'); Defs = @('-dMULTICOLOR'); Frames = 70000 }
    )
}

# Build everything: (variant, build, program, timed program, frames).
$runs = foreach ($v in $variants) {
    $tag = $v.Name -replace ' ', '-'
    $dir = "$repo\examples\$($v.Source)"
    $built = foreach ($timed in $false, $true) {
        $features = $v.Features + $(if ($timed) { 'timed' })
        $args = if ($features) { @('--features', ($features -join ',')) } else { @() }
        Push-Location $dir
        & $cargoC64 build --release @args 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "cargo c64 build failed in $dir" }
        Pop-Location
        $copy = "$rustOut\$tag$(if ($timed) { '-timed' }).prg"
        Copy-Item "$dir\target\mos-c64\release\$($v.Source).prg" $copy -Force
        $copy
    }
    [pscustomobject]@{ Variant = $v.Name; Build = 'rust64 release'; Plain = $built[0]; Timed = $built[1]; Frames = $v.Frames }
    foreach ($opt in '-Os', '-O2', '-O3') {
        $plain = "$out\$tag$opt.prg"
        $timed = "$out\$tag-timed$opt.prg"
        $defs = $v.Defs
        & $oscar $opt @defs "-o=$plain" "$here\math\oscar64\$($v.Source).c" 2>&1 | Out-Null
        & $oscar $opt @defs -dTIMED "-o=$timed" "$here\math\oscar64\$($v.Source).c" 2>&1 | Out-Null
        [pscustomobject]@{ Variant = $v.Name; Build = "oscar64 $opt"; Plain = $plain; Timed = $timed; Frames = $v.Frames }
    }
}
# (the examples' target directories are left as a plain build makes them)
foreach ($source in $variants.Source | Select-Object -Unique) {
    Push-Location "$repo\examples\$source"
    & $cargoC64 build --release 2>&1 | Out-Null
    Pop-Location
}

# Run them, several at a time: the emulator is the slow part.
$results = $runs | ForEach-Object -ThrottleLimit 8 -Parallel {
    $png = [IO.Path]::ChangeExtension($_.Timed, '.png')
    & $using:mimic --machine c64 --autostart --frames $_.Frames --prg $_.Timed --snapshot $png 2>&1 | Out-Null
    $text = node $using:screen $using:chargen.FullName $png
    $values = @{}
    foreach ($m in ($text | Select-String '^\s*([A-Z]+) (\d+)\s*$')) {
        $values[$m.Matches[0].Groups[1].Value] = [long]$m.Matches[0].Groups[2].Value
    }
    [pscustomobject]@{
        Variant = $_.Variant; Build = $_.Build; Bytes = (Get-Item $_.Plain).Length
        Time = $values['TIME']; Sum = $values['SUM']
    }
}

foreach ($v in $variants) {
    ""
    "== $($v.Name)"
    "{0,-16} {1,6} {2,12} {3,10}" -f 'build', 'bytes', 'cycles', 'checksum'
    foreach ($build in 'rust64 release', 'oscar64 -Os', 'oscar64 -O2', 'oscar64 -O3') {
        $r = $results | Where-Object { $_.Variant -eq $v.Name -and $_.Build -eq $build }
        "{0,-16} {1,6} {2,12} {3,10}" -f $build, $r.Bytes, $r.Time, $r.Sum
    }
}
