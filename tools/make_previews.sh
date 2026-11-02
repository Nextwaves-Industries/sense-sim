#!/usr/bin/env bash
# Renders the start-screen picture of every sample scene with the real GPU
# renderer: loads the sample, frames it, hides the UI and saves the 3D view
# as a 960 x 540 PNG in assets/samples/previews/. Rebuild afterwards (the
# pictures are compiled in). Runs one sample at a time: parallel windows
# come out black.
#
#   tools/make_previews.sh [exe] [first] [last]
set -u
cd "$(dirname "$0")/.."
exe=${1:-target/release/SenseSim.exe}
first=${2:-1}
last=${3:-9}
slugs=(uwb_warehouse rfid_warehouse hospital_ward auto_workshop office canteen pump_station engine_drive conveyor_line)
for n in $(seq "$first" "$last"); do
    out="assets/samples/previews/${slugs[$((n - 1))]}.png"
    for try in 1 2 3; do
        rm -f "$out.tmp.png"
        "$exe" --sample "$n" --preview --screenshot "$out.tmp.png" >/dev/null 2>&1
        # A black frame (GPU not ready) compresses to almost nothing.
        if [ -f "$out.tmp.png" ] && [ "$(stat -c %s "$out.tmp.png")" -gt 20000 ]; then
            mv "$out.tmp.png" "$out"
            echo "$n ${slugs[$((n - 1))]}: ok ($(stat -c %s "$out") bytes)"
            break
        fi
        echo "$n ${slugs[$((n - 1))]}: retry $try"
    done
    rm -f "$out.tmp.png"
done
