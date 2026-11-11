# Size and speed of the rasterbars, scroller and diagonal examples: rust64
# against oscar64. Builds both, runs the timed variants in mimic-headless
# and prints the program sizes and the measured cycles (see README.md).
#
#   pwsh bench/effects.ps1
#
# Tools: as for bench/drawing/run.ps1 (OSCAR64, RUST64_CARGO_C64,
# RUST64_MIMIC_HEADLESS, EMU_ROM_DIR).

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
$frames = 256

function Measure-Program($timed, $png) {
    & $mimic --machine c64 --autostart --frames 1500 --prg $timed --snapshot $png 2>&1 | Out-Null
    $text = node "$here\drawing\screen.js" $chargen.FullName $png
    $values = @{}
    foreach ($m in ($text | Select-String '^\s*([A-Z]+) (\d+)\s*$')) {
        $values[$m.Matches[0].Groups[1].Value] = [long]$m.Matches[0].Groups[2].Value
    }
    $values
}

foreach ($name in 'rasterbars', 'scroller', 'diagonal') {
    $dir = "$here\$name"
    $out = "$dir\oscar64\out"
    New-Item -ItemType Directory -Force $out | Out-Null
    $programs = [ordered]@{}

    foreach ($d in "$repo\examples\$name", "$dir\rust64-timed") {
        Push-Location $d
        & $cargoC64 build --release 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "cargo c64 build failed in $d" }
        Pop-Location
    }
    $programs['rust64 release'] = @(
        "$repo\examples\$name\target\mos-c64\release\$name.prg",
        "$dir\rust64-timed\target\mos-c64\release\$name-timed.prg")

    foreach ($opt in '-Os', '-O2', '-O3') {
        $plain = "$out\$name$opt.prg"
        $timed = "$out\$name-timed$opt.prg"
        & $oscar $opt "-o=$plain" "$dir\oscar64\$name.c" 2>&1 | Out-Null
        & $oscar $opt -dTIMED "-o=$timed" "$dir\oscar64\$name.c" 2>&1 | Out-Null
        $programs["oscar64 $opt"] = @($plain, $timed)
    }

    ""
    "== $name"
    "{0,-16} {1,6} {2,10} {3,10} {4}" -f 'build', 'bytes', 'avg/frame', 'max/frame', 'other'
    foreach ($build in $programs.Keys) {
        $plain, $timed = $programs[$build]
        $v = Measure-Program $timed "$out\$($build -replace '[^a-zA-Z0-9]+', '-').png"
        $other = @()
        if ($v.ContainsKey('INIT')) { $other += "start $($v['INIT'])" }
        if ($v.ContainsKey('WIRQ')) {
            # the interrupts' share of the fixed loop, as cycles per frame
            $perFrame = [math]::Round(($v['WIRQ'] - $v['WSEI']) * 19656 / $v['WIRQ'])
            $other += "interrupts $perFrame/frame (loop $($v['WSEI']) -> $($v['WIRQ']))"
        }
        "{0,-16} {1,6} {2,10:F0} {3,10} {4}" -f $build, (Get-Item $plain).Length, ($v['SUM'] / $frames), $v['MAX'], ($other -join ', ')
    }
}
