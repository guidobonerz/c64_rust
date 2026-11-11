# Size and speed of the drawing example: rust64 against oscar64.
# Builds both, runs the timed variants in mimic-headless and prints the
# program sizes and the cycles of every phase (see README.md).
#
#   pwsh bench/drawing/run.ps1
#
# Tools (override with the environment variables):
#   OSCAR64                 oscar64.exe            (default: on the PATH)
#   RUST64_CARGO_C64        cargo-c64.exe          (default: target/debug, then target/release)
#   RUST64_MIMIC_HEADLESS   mimic-headless.exe     (default: ../mimic/target/release)
#   EMU_ROM_DIR             the C64 ROMs           (default: ~/.mimic/roms)

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$repo = Resolve-Path "$here\..\.."
$oscar = if ($env:OSCAR64) { $env:OSCAR64 } else { 'oscar64' }
$cargoC64 = if ($env:RUST64_CARGO_C64) { $env:RUST64_CARGO_C64 } else {
    @("$repo\target\debug\cargo-c64.exe", "$repo\target\release\cargo-c64.exe") |
        Where-Object { (Test-Path $_) -and (Test-Path (Join-Path (Split-Path $_) 'rustc_codegen_rust64.dll')) } |
        Select-Object -First 1
}
$mimic = if ($env:RUST64_MIMIC_HEADLESS) { $env:RUST64_MIMIC_HEADLESS } else { "$repo\..\mimic\target\release\mimic-headless.exe" }
$roms = if ($env:EMU_ROM_DIR) { $env:EMU_ROM_DIR } else { "$HOME\.mimic\roms" }
$chargen = Get-ChildItem "$roms\c64\chargen*" | Select-Object -First 1

$out = "$here\oscar64\out"
$shots = "$here\shots"
New-Item -ItemType Directory -Force $out, $shots | Out-Null

$programs = [ordered]@{}   # name -> @(program, timed program)

# rust64: the example as it is and the timed copy, clipped (the default)
# and with the feature `gfx-unclipped`. The clipped build comes last, so
# that the examples' target directories are left as a plain build makes them.
$rustOut = "$here\rust64-out"
New-Item -ItemType Directory -Force $rustOut | Out-Null
foreach ($variant in 'unclip', 'clip') {
    $features = if ($variant -eq 'unclip') { @('--features', 'c64/gfx-unclipped') } else { @() }
    $built = foreach ($dir in "$repo\examples\drawing", "$here\rust64-timed") {
        Push-Location $dir
        & $cargoC64 build --release @features 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "cargo c64 build failed in $dir" }
        Pop-Location
        $prg = Get-ChildItem "$dir\target\mos-c64\release\*.prg" | Select-Object -First 1
        $copy = "$rustOut\$($prg.BaseName)-$variant.prg"
        Copy-Item $prg.FullName $copy -Force
        $copy
    }
    $programs["rust64 release $variant"] = @($built)
}
$order = @('rust64 release clip', 'rust64 release unclip')

# oscar64: clipped (what rust64's routines do) and unclipped, per level.
foreach ($variant in 'clip', 'unclip') {
    foreach ($opt in '-O1', '-Os', '-O2', '-O3') {
        $defs = if ($variant -eq 'unclip') { @('-dUNCLIPPED') } else { @() }
        $plain = "$out\drawing-$variant$opt.prg"
        $timed = "$out\drawing-$variant-timed$opt.prg"
        & $oscar $opt @defs "-o=$plain" "$here\oscar64\drawing.c" 2>&1 | Out-Null
        & $oscar $opt @defs -dTIMED "-o=$timed" "$here\oscar64\drawing.c" 2>&1 | Out-Null
        $programs["oscar64 $opt $variant"] = @($plain, $timed)
    }
}

"{0,-22} {1,6} {2,8} {3,8} {4,8} {5,8} {6,8} {7,8}" -f 'build', 'bytes', 'setup', 'lines', 'rect', 'circles', 'cells', 'total'
$order += $programs.Keys | Where-Object { $_ -notin $order }
foreach ($name in $order) {
    $plain, $timed = $programs[$name]
    $png = "$shots\$($name -replace '[^a-zA-Z0-9]+', '-').png"
    & $mimic --machine c64 --autostart --frames 900 --prg $timed --snapshot $png 2>&1 | Out-Null
    $text = node "$here\screen.js" $chargen.FullName $png
    $t = foreach ($phase in 'SETUP', 'LINES', 'RECT', 'CIRCLES', 'CELLS', 'TOTAL') {
        ($text | Select-String "$phase (\d+)").Matches[0].Groups[1].Value
    }
    "{0,-22} {1,6} {2,8} {3,8} {4,8} {5,8} {6,8} {7,8}" -f (@($name, (Get-Item $plain).Length) + $t)
}
