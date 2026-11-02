#!/usr/bin/env bash
# The simulation gate: every unit test, then the solver benchmarks with real pass/fail limits.
# Exits 0 only if all of it passed. See docs/signoff/PLAN.md for what passing does and doesn't mean.
#
#   tools/simulation_gate.sh           everything, about an hour on an idle machine
#   tools/simulation_gate.sh --quick   unit tests only, no solvers
#
# Needs openEMS installed (Setup page in Antenna Studio) for the benchmarks. Build output goes to
# CARGO_TARGET_DIR if set. The report goes to output/gate/<time>/report.md.
set -u
cd "$(dirname "$0")/.."
[ -f ./env.sh ] && source ./env.sh

quick=0
[ "${1:-}" = "--quick" ] && quick=1

out="output/gate/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
report="$out/report.md"
failed=0

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
    # No summary line: the run was stopped from outside or crashed, so no verdict on the tests.
    grep -q '^test result:' "$log" || result="FAIL (cut off, no summary)"
    grep -E '^(error|test .*FAILED)|panicked at' "$log" | cut -c1-200 | head -20
  fi
  echo "| $name | $result | ${passed:-0} | ${fails:-0} | ${ignored:-0} |" >> "$report"
  echo "   $result: ${passed:-0} passed, ${fails:-0} failed, ${ignored:-0} ignored"
}

stage core cargo test --no-fail-fast -p antenna_core -p proof
stage app cargo test --no-fail-fast --bin SenseSim
if [ "$quick" -eq 0 ]; then
  stage benchmarks cargo test --no-fail-fast --bin SenseSim -- --ignored --exact --nocapture --test-threads=1 "${benchmarks[@]}"
  # The figures a person reads next to the verdict.
  grep -E 'FDTD:|FDTD |deficit vs Friis|resonance |S21 with|two ports:' "$out/benchmarks.log" | cut -c1-220 > "$out/figures.txt"
fi

{
  echo
  if [ "$failed" -eq 0 ]; then
    echo "**Gate: PASS.** The code matches its known answers. This says nothing about real hardware."
  else
    echo "**Gate: FAIL.**"
  fi
  [ "$quick" -eq 1 ] && echo && echo "Quick run: the solver benchmarks did not run, so this can't count as a full pass."
} >> "$report"

echo "Report: $report"
exit "$failed"
