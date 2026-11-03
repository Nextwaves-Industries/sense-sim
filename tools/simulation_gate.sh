#!/usr/bin/env bash
# The simulation gate: every unit test, then the solver benchmarks with real pass/fail limits.
# Exits 0 only if all of it passed. See docs/signoff/PLAN.md for what passing does and doesn't mean.
#
#   tools/simulation_gate.sh           unit tests and solver benchmarks, over an hour on an idle machine
#   tools/simulation_gate.sh --quick   unit tests only, no solvers
#   tools/simulation_gate.sh --full    also the OpenFOAM and LBM benchmarks, which take much longer
#
# Needs openEMS (Setup page in Antenna Studio), and CalculiX, Gmsh and Elmer (Simulation workbench)
# for the benchmarks. Build output goes to CARGO_TARGET_DIR if set. The report goes to
# output/gate/<time>/report.md. Check nothing else is building first: this machine is shared.
set -u
cd "$(dirname "$0")/.."
[ -f ./env.sh ] && source ./env.sh

quick=0
full=0
case "${1:-}" in
  --quick) quick=1 ;;
  --full) full=1 ;;
esac

out="output/gate/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
report="$out/report.md"
failed=0
stopped=0

# Benchmarks that assert against a known answer. The other ignored tests print studies for a
# person to read, or take hours: leave them out, or a pass says nothing about them.
benchmarks=(
  antenna::run::tests::antenna_fdtd_dipole_matches_theory
  antenna::run::tests::antenna_fdtd_monopole_on_ground
  antenna::run::tests::antenna_fdtd_patch_on_fr4
  antenna::run::tests::antenna_fdtd_and_mom_agree
  antenna::run::tests::antenna_fdtd_gate_agrees_with_mom
  antenna::run::tests::antenna_fdtd_two_ports_are_reciprocal
  antenna::run::tests::antenna_fdtd_step_limit_is_reported
  antenna::run::tests::antenna_mom_templates_are_quiet
  outcomes::hardware::fdtd_checks::fdtd_plastic_enclosure_detunes_a_dipole
  rtls::fdtd::tests::fdtd_dipole_end_to_end
  rtls::fdtd::tests::fdtd_patch_efficiency_end_to_end
  rtls::fdtd::tests::fdtd_wall_s21_end_to_end
  rtls::fdtd::tests::fdtd_scene_end_to_end
)

{
  echo "# Simulation gate, $(date '+%Y-%m-%d %H:%M')"
  echo
  echo "- Commit: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- Uncommitted files: $(git status --short 2>/dev/null | wc -l)"
  echo "- Toolchain: $(cargo --version 2>/dev/null)"
  echo
  echo "| Stage | Result | Passed | Failed | Ignored |"
  echo "|---|---|---|---|---|"
} > "$report"

# Runs a stage, keeps its log, and adds the summed counts to the report.
stage() {
  local name="$1"; shift
  local log="$out/$name.log"
  if [ "$stopped" -eq 1 ]; then
    echo "| $name | not run | | | |" >> "$report"
    return
  fi
  echo "== $name"
  "$@" > "$log" 2>&1
  local code=$?
  local counts
  counts=$(grep -E '^test result:' "$log" | awk '{for (i = 1; i <= NF; i++) { if ($i == "passed;") p += $(i-1); if ($i == "failed;") f += $(i-1); if ($i == "ignored;") g += $(i-1) }} END { printf "%d %d %d", p, f, g }')
  read -r passed fails ignored <<< "$counts"
  local result=PASS
  # A stage that ran nothing has passed nothing.
  if [ "$code" -ne 0 ] || [ "${passed:-0}" -eq 0 ]; then
    result=FAIL
    failed=1
    # No summary line: it didn't build, or was stopped from outside, so no verdict on the tests.
    # Don't go on to the next stage: if someone killed this run, they meant all of it.
    if ! grep -q '^test result:' "$log"; then
      result="FAIL (no summary: didn't build, or was stopped)"
      stopped=1
    fi
    grep -E '^(error|test .*FAILED)|panicked at' "$log" | cut -c1-200 | head -20
  fi
  echo "| $name | $result | ${passed:-0} | ${fails:-0} | ${ignored:-0} |" >> "$report"
  echo "   $result: ${passed:-0} passed, ${fails:-0} failed, ${ignored:-0} ignored"
}

stage core cargo test --no-fail-fast -p antenna_core -p proof -p sim-core
stage app cargo test --no-fail-fast --bin SenseSim
if [ "$quick" -eq 0 ]; then
  stage structural_elmer cargo test --release --no-fail-fast -p sim-core --test structural --test elmer -- --ignored --nocapture --test-threads=1
  stage benchmarks cargo test --no-fail-fast --bin SenseSim -- --ignored --exact --nocapture --test-threads=1 "${benchmarks[@]}"
  # The figures a person reads next to the verdict.
  grep -E 'FDTD:|FDTD |deficit vs Friis|resonance |S21 with|two ports:' "$out/benchmarks.log" 2>/dev/null | cut -c1-220 > "$out/figures.txt"
fi
if [ "$full" -eq 1 ]; then
  stage foam_lbm cargo test --release --no-fail-fast -p sim-core --test foam --test lbm -- --ignored --nocapture --test-threads=1
fi

{
  echo
  if [ "$failed" -eq 0 ]; then
    echo "**Gate: PASS.** The code matches its known answers. This says nothing about real hardware."
  else
    echo "**Gate: FAIL.**"
  fi
  [ "$quick" -eq 1 ] && echo && echo "Quick run: the solver benchmarks did not run, so this can't count as a full pass."
  [ "$quick" -eq 0 ] && [ "$full" -eq 0 ] && echo && echo "The OpenFOAM and LBM benchmarks did not run (--full), so this says nothing about those analyses."
} >> "$report"

echo "Report: $report"
exit "$failed"
