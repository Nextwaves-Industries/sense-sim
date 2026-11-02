# RTLS simulation audit (read-only)

**Verdict: not good enough to sign off a deployment.** The core formulas are mostly right, and on open floors the planner matches the sim within 3%. The defects are in what gets scored and reported: "Good" verdicts, floor coverage and read counts can all be badly optimistic, and obstructed floors are under-predicted by 2 to 5×.

**How this was checked**
- `src/...` paths are under ``.
- I did not run cargo or the app, so UI text is traced from code, not observed.
- Numbers marked "port" come from a Node port of the code's arithmetic, in `C:\Users\DeanOS\AppData\Local\Temp\rtls_audit\`.
- The port scene is 30×20 m with 6 perimeter anchors at 2.6 m, a tag at 1.2 m, and 200 to 300 draws per cell or seed.
- The port approximates the scene geometry, so treat its numbers as indicative.

## BLOCKER (all confirmed)

**B1. No data reads as a perfect result, and the verdict can print the target instead of the result.**
- **Code:**
  - `src/rtls/mod.rs:332` `if e.is_empty() { return (0.0, 0.0, 0.0, 0.0);`
  - `deploy.rs:1036` `report.uwb_cep95_m <= accept`
  - `ui.rs:2015` `format!("Good: 95% of positions within {}", dist(err95.min(r.target_m)))`
- **Why it's wrong:**
  - 0.0 means "no samples" but passes `<= accept`.
  - RFID never enters `st.errors` (only `sim.rs:744` pushes there).
  - The sim check is UWB-only (`deploy.rs:1071`), and RSSI plans have no accuracy gate (`deploy.rs:730`).
- **Effect:**
  - An RFID-only plan that meets its target shows "Good: 95% of positions within 0 cm".
  - A BLE plan with a real 3 m CEP95 shows "within 30 cm", which is 10× wrong.
  - UWB with zero fixes passes the sim check.
- **Fix:** make `error_stats` return `Option` plus the sample count, print the real err95, and use a per-technology target. `stress.rs:80` and `battery.rs:134` already guard against this with `p95 > 0`.

**B2. `targets_met` is judged against what was reachable, not what was requested.**
- **Code:**
  - `placement.rs:484` `self.target_cov = t.map(|t| t.min(reach.coverage * 0.98));`
  - `deploy.rs:442` `(true, true) => r.chosen.len() < b.chosen.len() ...`
  - `tools.rs:542` `"zones_fully_covered"`, fed by `zones_met`
- **Arithmetic:** with 50% reach the 95% target becomes 49%, so 49% coverage counts as met.
- **Effect:**
  - The verdict says "Good" on half a floor.
  - A low-reach mounting tier beats a tier that covers 95% with one more anchor, because their targets differ and fewest devices wins.
  - A zone with 40% reach counts as "fully covered" at 39.2%.
  - The weak-zone advice appears only when targets are not met (`ui.rs:2048`).
- **Fix:** compare against the requested target, report `reachable_coverage` separately, and rank plans by coverage first when their targets differ.

**B3. Acceptance "95% of positions" is not a floor statistic.**
- **Code:** `sim.rs:744` `st.errors.push_back((now, err, tech));`, `deploy.rs:71` `test_s: 30.0`, `deploy.rs:1000` `speed: 1.3 + 0.2 * w`.
- **Why it's wrong:**
  - It pools Kalman-filtered fixes equally across all tags.
  - Parked tags average their independent noise down, and they count the same as moving ones.
  - Walkers cover only 39 m and 45 m of a corner-to-corner loop in 30 s, so most aisles are never visited.
  - Epochs with no fix are excluded.
  - The output has no sample count, availability, uncertainty or "simulated" qualifier in the UI verdict.
- **Effect (port):**

| Scene | Walkers only, CEP95 | With 10 parked tags, pooled CEP95 |
|---|---|---|
| 10 cm concrete partitions | 1.53 m | 0.345 m |
| Typical walls | 1.01 m | 0.59 m |
| Empty room | 0.107 m | 0.090 m |

  Adding parked tags improves the number without changing the system. Seed noise is small by comparison (p05 to p95 of 1.42 to 1.64 m), so the problem is sampling, not the RNG.
- **Fix:** score fixed grid points, take a per-spot p95, count no-fix as failure, separate moving and static tags, and print the sample count and availability.

**B4. An RFID "read" means read at least once in 30 s, with fading redrawn every 50 ms.**
- **Code:** `radio.rs:562` `let fading = rng.gauss(2.0);` (called each tick from `sim.rs:823`), `deploy.rs:1032` `t.reads == 0`, `sim.rs:1078` `(t.reads > 0) as u8`.
- **Why it's wrong:** a static tag's fade is not independent every tick, yet the "N of M shelf items read" note comes from this count.
- **Effect:**

| Forward margin | Read chance per round | P(ever read), 1 / 50 / 150 tags in field |
|---|---|---|
| -4 dB | 2.3% | 100% / 99% / 75% |
| -5 dB | 0.6% | 98% / 74% / 32% |
| -6 dB | 0.13% | 56% / 26% / 8% |

  - The planner's 90% criterion needs +2.6 dB, a gap of 6 to 8 dB, or about 2 to 2.5× in forward-limited distance.
  - The 12 m cap hides this on-axis in line of sight (5.69 dB margin at 12 m). It bites off-axis and through obstructions.
  - With few tags in the field the result also depends on tick rate: at -6 dB, 18% at 5 Hz, 56% at 20 Hz, 100% at 200 Hz.
- **Fix:** hold the fade per tag-reader pair until something moves (or per hop channel), and report per-round probability and read success within a stated time budget.

**B5. The planner under-predicts obstructed floors, so the Quality map shows green where the sim fails.**
- **Code:** `placement.rs:138` treats NLOS (non-line-of-sight) bias as zero-mean variance, and `:177` `p / var.max(1e-6)` turns it into information.
- **Why it's wrong:** a biased range that the first-path flag misses goes into the solver at full weight, and the CRLB (Cramér-Rao bound) never sees it. At `wall_eq` 1, `expected - fp` is about 5.3 dB ± 1.5 against a 4 dB threshold, so about 19% are missed.
- **Effect (port):**

| Scene | Sim RMS / planner RMS (median, p95, max) | Other results |
|---|---|---|
| Line of sight only | 1.03, 1.09, 1.15 | Quality map agrees 100% |
| 10 cm concrete partitions | 1.33, 3.29, 4.52 | Predicted p95 0.43 m; per-cell 95% radius 1.08 m |
| Typical walls | 1.99, 4.16, 5.27 | Map passes 93.3%, sim 70.5%; 22.8% of cells green but failing; predicted p95 0.276 m (target met), pooled sim 0.86 m |

  Cells with 2 LOS + 3 NLOS anchors have a median ratio of 2.9 and a max of 6.2.
- **Fix:** propagate bias through the solver's weights, `b = (HᵀWH)⁻¹HᵀW·b_link` with the flag-miss probability, then use `err² = crlb² + |b|²`. Add a per-cell planner-versus-sim regression test.

## MAJOR (all confirmed)

**M1. Metrics cover only the last 30 s of a run.**
- **Code:** `sim.rs:969` `*t < now - 30.0`. `rtls_run` allows 3600 s (`tools.rs:391`).
- **Effect:** a one-hour run reports 30 s of fixes. The live tab says "RMSE (30 s)" but the tool JSON does not.
- **Fix:** keep a whole-run accumulator and label the window.

**M2. The same seed does not reproduce a run.**
- **Code:** `mod.rs:319` `restart` resets `RtlsState` only, and `tools.rs:394` and `deploy.rs:1020` call it.
- **Why it's wrong:** `Transform` and `MotionState` persist, so moving tags start wherever they stopped. `stress.rs:53` restores them, so this was known there. The single shared RNG stream also means adding a device shifts all later draws.
- **Fix:** reuse `Start::restore` in `rtls_run` and `auto_deploy`.

**M3. Ground truth leaks into the estimator.**
- **Code:**
  - `sim.rs:511` and `:612` `Dim::Plane(truth.y as f64)`
  - `:720` `(raw - *truth).length() > 200.0`
  - `:643-644` AoA weights from the noise-free `mean` and the true `wall_eq`
  - `:994` `t.est.unwrap_or(t.truth)`
- **Effect:**
  - A fixed-plane solver with a 0.9 m height error gets 7 cm median and 17 cm p95 with no noise (port, 10 m anchor grid).
  - AoA is first-order: 0.5 m × tan 60° = 0.87 m, against a 0.75 m target.
  - `tag_height` and `heights` imply varying tags, yet the solver always knows the true height.
  - Wild fixes are dropped using truth and never counted.
- **Fix:** solve at a configured plane height, gate against anchor bounds, and take AoA weights from the noisy RSSI.

**M4. The RFID sim ignores per-device and FDTD antennas.**
- **Code:** `radio.rs:560` `let g_r = antenna_gain(reader.antenna_gain_dbi, reader.beam_deg, angle);`, while the planner uses `ants.gain(n.antenna...)` (`planning.rs:416`).
- **Effect (Vietnam, 6 dBi antenna):** tx is capped to 23.15 dBm for 6 dBi, but the sim applies 9 dBi. EIRP is 32.15 dBm against the 29.15 limit, so the forward link is +3 dB and the reverse +6 dB versus the map.
- **Fix:** pass the `ants.gain` result into `rfid_link`.

**M5. TDoA model errors.**
- **(a) Noise:** `radio.rs:450` and `placement.rs:158` use `range_sigma_m * 0.5`.
  - Arithmetic: TWR is `r = c(Tround - Treply)/2` with 4 timestamps, so σ_r = cσ_t, and one arrival has σ = 0.08 m, not 0.04 m.
  - Total arrival noise is 0.060 m coded versus 0.092 m correct.
  - Effect: TDoA beats TWR at the centre (port: 0.072 versus 0.086 m RMS), and the planner is 1.5× low.
- **(b) Map ignores TDoA:** `planning.rs:466` always uses the default TWR tag, while the optimiser checks (`:756`).
  - At 5 m outside a 10 m square: map 0.095 m, optimiser 0.248 m, corrected noise 0.378 m.
  - At 10 m outside: 0.136, 0.762, 1.161 m.
- **(c) Link rules:**
  - The sim ignores the planner's `unsynced` result (`sim.rs:565` always uses offset 0).
  - The blink uses the anchor's TX (`radio.rs:444`), so a tag's power override has no effect.
- **Fix:** use `range_sigma_m` for σ_toa, share the optimiser's tag selection with the map, drop unsynced anchors in the sim, and use the tag's TX for TDoA.

**M6. `rtls_coverage` reports RFID link margin in dB as metres.**
- **Code:** `planning.rs:435` `c.err = best;` then `tools.rs:435` `"p95_predicted_error_m": p95`. `quality_share` (`planning.rs:210`) also runs on it.
- **Effect:** a 20 dB margin appears as a 20 m error. `fix_share(10, 0.3)` = 0.0009, so `pass_fraction` is about 0 on a fully covered floor. The UI guards this; the assistant's tool output does not.
- **Fix:** return null for these fields on RFID and report margin under its own name.

**M7. The planner leaves the exponential's mean out of NLOS error.**
- **Code:** `placement.rs:138` `(path.bias_eq * dev.nlos_bias_m).powi(2) + (path.bias_eq * dev.nlos_sigma_m).powi(2)`, against `radio.rs:407`, which adds both.
- **Arithmetic:** for `bias_eq` 1, sim MSE = 0.60² + 0.25² = 0.4225 versus 0.185 coded, which is 1.51× in RMS.
- **Effect (port):** 3 NLOS anchors give 1.185 m planned versus 1.789 m simulated. With the fix the planner gives 1.786 m.
- **Fix:** use `(bias + sigma)² + sigma²`, where `bias` and `sigma` are `bias_eq` times `nlos_bias_m` and `nlos_sigma_m`.

**M8. AoA noise does not grow off boresight.**
- **Code:** `radio.rs:498` has no angle term, and the FOV allows ±70°.
- **Arithmetic:** array CRLB scales as 1/cos θ, so 3° becomes 8.8° at 70°, or 2.9×. On the floor plane that is 0.90 m predicted versus 2.62 m (locator 2 m above the tag).
- **Also:** `solver.rs:353` hard-codes `dop: 1.0`, and a single bearing has zero residual, so `quality` reads 1.0 for the least-checked fix.
- **Fix:** divide σ by `cos(angle).max(ε)` and derive AoA DOP from the bearing information.

**M9. The RFID map and optimiser use different criteria.**
- **Code:** `planning.rs:425` `margin >= 0.0` is a 50% read chance, while the optimiser needs `read_p` 0.9 (+2.58 dB). The comment at `planning.rs:303` says the map shows what the optimiser scores.
- **Effect:** the map's read zone is up to 1.35× wider in forward-limited distance.
- **Fix:** use `rfid_read_probability >= read_p`.

**M10. The BLE and WiFi maps are optimistic.**
- **Proximity:** `planning.rs:403` sets `c.err = PROXIMITY_ERR_M` (4 m), but the sim's fix sits at the node, so the true error reaches the 30 m range. Those cells also count as "has a fix".
- **FDTD fallback:** `planning.rs:318` uses `range_sigma_m` (default 0.1 m) for RSSI nodes, so one such link predicts about 10 cm.
- **Frequency:** `ble-beacon` has no `freq_mhz` in `generic.json`, so `planning.rs:297` uses 6489.6 MHz. That adds 8.5 dB to FDTD RSSI, or 2.43× heard radius.
- **Fix:** use horizontal distance as proximity error, use the RSSI sigma in the fallback, and add `freq_mhz: 2440`.

**M11. Ranging noise is independent per epoch, so the Kalman filter averages away errors that real static tags keep.**
- **Code:** `radio.rs:403` and `:407`. LOS links also have no multipath bias.
- **Effect (port):** a static tag's KF p95 is 0.083 m with independent noise versus 0.123 m with the same sigma frozen per link, which is 48% optimistic.
- **Fix:** add a per-link, per-location persistent term.

**M12. "Floor with a good position" means RMS ≤ 3× target, and the known 1-sigma p95 row is worse than stated.**
- **Code:** `ui.rs:2038`, `planning.rs:892` `err_cap: target_m * 3.0`, `placement.rs:692`, `:735-738`.
- **Arithmetic:**
  - The 0.9 m cap means a 95% radius of 0.9 × 1.731 = 1.56 m.
  - The target gate is 1-sigma as well: a spot at RMS 0.3 m has 63% within 0.3 m and a 95% radius of 0.52 m. Port factor is 1.6 to 1.9×.
  - The p95 covers only points that have a fix.
  - `deploy.rs:125` documents "≥3 anchors in line of sight", but NLOS links count.
- **Fix:** convert with 1.731 before comparing, count no-fix points, and relabel.

## MINOR (all confirmed)
- **`radio.rs:597`:** `0.368 * 2.72` equals 1.001, so the ALOHA comment describes nothing. Delete it or model it.
- **`radio.rs:565`:** reciprocal fading is counted once. Reverse-link σ is 2.24 versus 4.12 dB, which is small because the forward link limits.
- **`sim.rs:624`:** body shadow uses the tag's default 6489.6 MHz for BLE and WiFi, giving 25.9 versus 18.7 dB at 180° (pessimistic).
- **`radio.rs:436`:** UWB gain sits on top of the −41.3 dBm/MHz EIRP limit with no cap. The default 40 m `max_range_m` hides it at full power. It shows at reduced TX and in the signal map.
- **`rf.rs:193` and `:199`:** with nothing heard, avg and min show 0 dBm, and avg's tile is green (`:430`).
- **`region.rs:96`:** the conducted limit is not enforced, so `rfid-desktop` stays at 33 dBm under the US 1 W rule and the Japan 250 mW rule, despite the "legal values" claim at `tools.rs:42`.
- **`device.rs:108`, `:213`, `:293`, `antenna.rs:93`:** typos, bad JSON, unknown profile ids and missing patterns fall back silently to defaults.
- **`ui.rs:2032`:** "30 s" is hard-coded, and "found" is the last snapshot's instant.
- **`deploy.rs:707-708`:** coverage and p95 are not re-evaluated after feedback anchors or the extra readers at `:911`.
- **`sim.rs:730`:** the KF treats total 2D error as per-axis, so R is 2× too big and adds slight lag.
- **`placement.rs:711`, `deploy.rs:462`:** the floor index reads slightly low, and a "p95" from 5 samples is the 4th smallest (about p75).

## SUSPECTED (not fully traced)
- **`sim.rs:106`:** `objs.retain(|o| !o.parts.is_empty())` drops objects with no meshes yet. Only `autotest` checks loading, so a plan made during load may see open floor.
- **`radio.rs:94`, `:103`:** "box/crate/bin" count as 0.15 of a wall (0.9 dB) whatever they contain, and unknown objects count as one wall.
- **`sim.rs:529`:** the NLOS flag reads the exact loss the model added (about 91% detection, 0.4% false alarm). That is probably cleaner than real chips.
- **`stress.rs:132`, `crates/proof/src/claim.rs:116`:** "Likely" from 16 runs of 10 s. At 16/16 the Wilson lower bound is 80.6%, and `robust.rs:214` gives a margin of 0. It inherits B3's sampling.

## SIMULATOR_FINDINGS.md sample
- **Hold, line numbers often moved:**
  - exact lines: `generic.json` rows, `rf.rs:21`, `placement.rs:57` and `:60`
  - content unchanged: UWB range ignoring walls (now `radio.rs:376`, `:435`), 32 dB at 920 MHz and 24 dB at 6.5 GHz for 20 cm concrete, the BLE `tx_power` row, one 10 cm wall adding 1.2 m mean, the reader rate row (now `sim.rs:844`)
  - not modelled, still true: `clock_ppm` unread and offset 0 (now `sim.rs:565`), `current_ma` unread, no air-time model
- **Stale:**
  - "`rtls_add_device` sets yaw only": tilt and roll now exist (`tools.rs:295-296`), though the default still faces horizontal.
  - "No body-blocking": partly stale. People (`sim.rs:502`) and worn-tag shadow (`:493`) exist, but both are off by default and absent from the planner.
- **Worse than stated:**
  - The 1-sigma p95 is the actual pass gate (M12).
  - RFID fading sigma is small and also redrawn every tick (B4).
  - RSSI plans have no accuracy gate at all (B1).

## Checked and sound
- **Path loss and link budget:** FSPL (48.69 dB at 1 m, 6489.6 MHz) and the UWB budget (−14.32 dBm, 39.7 m on channel 5, 32.3 m on channel 9).
- **RFID:** at 5 m, forward −4.71 dBm, margin 13.29 dB, RSSI −45.42 dBm, forward-limited at 23.1 m. The phase formula and cos^n antenna pattern are also correct.
- **Ranging noise:** the TWR sigma combination, and the BLE distance sigma (0.419·d) with its wall-as-distance bias.
- **Geometry and probability:** bearing noise, `solve_aoa` normal equations, `normal_above`, heard and read probabilities against the sim's draws, `fix_share`, the people Poisson term, body-shadow geometry.
- **Solver:** Fisher algebra including the TDoA Schur complement, 3D cofactors and add/remove updates, plus LM, DOP, robust index mapping and Kalman F/Q/K/P.
- **RNG:** SplitMix64 constants, Box-Muller and the exponential draw are correct.
- **Region:** ERP-to-EIRP conversions (+2.15 dB).

## Not audited
`crates/proof` and `src/outcomes` were only skimmed. `src/outcomes/acceptance.rs` appeared during the audit and I did not review it.
