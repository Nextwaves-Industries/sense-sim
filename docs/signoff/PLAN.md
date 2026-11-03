# Getting Sense Sim's simulation to sign-off level

Written 2026-11-02, after a four-part audit and a round of fixes. The audit reports are beside this file: `audit-rtls.md`, `audit-fdtd.md`, `audit-mom.md`, `audit-outcomes-plant.md`. Finding IDs below (such as RTLS-B1) refer to them. Line numbers in those reports describe the code as it was before the fixes.

## Where it stands

**Not at sign-off level.** Nobody should commit to a contract, a deployment or a product on these numbers yet. The audit found 31 blockers and 53 major defects. Nearly all are now fixed or flagged, but no amount of fixing code gets past these three:

1. **Nothing has been compared with real hardware.** Every test compares the code with theory or with itself. A simulation can pass all of them and still be wrong about a real building.
2. **Results can't be traced.** The code is now committed (main at 711d58d), but nothing stamps a result with the commit, solver versions or inputs behind it. A sign-off record must name them.
3. **Ranges don't include model error.** "About 90 % of runs" means runs of the model with assumed inputs. In a test with an unmodelled error added, a range built that way held only about 76 % of the time, even at 200 runs. So a range can't yet promise to contain a measurement.

**Fine to use today:** comparing layouts or designs, pre-tuning an antenna before building it, finding coverage holes, and deciding what to measure.

**Not yet:** quoting a performance number to a customer, skipping a site survey, skipping a chamber or VNA check, or relying on a "Calibrated" label as proof.

## Last gate run

Run on 2026-11-02 at 13:16 with `tools/simulation_gate.sh`, on commit 0358f9d plus 116 uncommitted files. **It has not passed in full.**

| Stage | Result |
|---|---|
| Core crates (`antenna_core`, `proof`) | 511 passed, 0 failed, 10 ignored |
| App (`--bin SenseSim`) | 550 passed, 0 failed, 26 ignored |
| Solver benchmarks | 11 of 13 passed, then stopped at 14:22 when all cargo processes were killed on the owner's order, which isn't a test failure |

The two unfinished: `fdtd_scene_end_to_end` passed its first phase (line of sight 0.3 dB, behind the wall 7.6 dB, last sample 51.4 ns against 49.7 ns needed) and was stopped during its second, and `fdtd_wall_s21_end_to_end` didn't start on this build. An earlier build of that test read 6.5 dB.

Other files have changed since. When builds are allowed again, run the whole gate on one tree.

Figures from that run:

| Benchmark | Result |
|---|---|
| Strip dipole, FDTD | 74.6+j4.3 Ω, 2.135 dBi, power balance 1.0038 |
| Monopole on ground, FDTD | 38.3+j7.3 Ω, 5.151 dBi, power balance 1.0035 |
| Patch on FR-4, FDTD | 2.441 GHz, -27.5 dB, 5.74 dBi directivity, 2.33 dBi gain, 45.6 % efficient |
| FDTD against MoM, strip dipole | 2.98 % apart at default meshes, 1.70 % at finer ones |
| Two ports, reciprocity | 1.92 % worst |

### Waiting for builds to be allowed

Written down but not applied, because they can't be compiled yet and an installer build is reading this tree:

1. The Positioning antenna tab still stops at -40 dB (`end_criteria: 1e-4` in `src/rtls/fdtd/antenna.rs`). Move it to -50 dB like Antenna Studio, then re-run `fdtd_dipole_end_to_end` and `fdtd_patch_efficiency_end_to_end`.
2. Remove `RtlsState::error_stats` from `src/rtls/mod.rs`. Nothing calls it now, and its all-zero result for "no data" is the defect from RTLS-B1.
3. Run the app and look at the changed screens: nobody in this audit has reviewed them screen by screen.

## What the gate is

A result may only be used for what its level allows. The levels build on each other.

| Level | Name | What it proves | How it's checked |
|---|---|---|---|
| L0 | Estimate | Nothing beyond the arithmetic | None |
| L1 | Code verified | The solver gets known answers right | `tools/simulation_gate.sh` passes on the exact commit |
| L2 | Solution verified | This result's numerical error is known and stated | A mesh and stop-level study ran for this result, and no solver warning is open |
| L3 | Model validated | The model matches measurements for this kind of problem | The validation campaign below passes, with its error fed back into the ranges |
| L4 | Signed off | This specific site or product meets its target | A calibration from a real walk or a real VNA sweep, then an acceptance test on a random sample of points |

Sign-off means L4. Each level needs a record: commit, solver versions, inputs, seed, mesh and stop level.

### Where each area is today

Levels below are what the code can reach. Strictly, no result can claim one until Phase 1 stamps it with the commit that made it.

| Area | Level reached | Why it stops there |
|---|---|---|
| Wire and surface MoM | L1, L2 on request | The mesh study is optional. Delta-gap feed never fully converges |
| Antenna Studio FDTD | L1, L2 on request | Same. FDTD and MoM differ by about 1.3 % on a strip dipole, from their feeds, at the finest meshes tried |
| Whole-scene FDTD | L1 up to ×2 scaling | Above ×2 it's view-only, which covers whole-scene UWB on real sites. No scaled benchmark exists |
| Through-object FDTD | L1 | Fine for one material, one band |
| RTLS planner and simulator | Below L1 | The two agree with each other, but neither is checked against an outside answer |
| Predictions and recipes | L0 to L1 | Labels are honest now. No model-error term except the assumed one on tag and tunnel |
| Plant | Below L1 | Hand calculations only. Pump curves are invented defaults, now labelled. No torque or power |
| 17 analysis types (`crates/sim-core`) | L1 where re-run | Structural and Elmer benchmarks are in the gate. OpenFOAM and LBM need `--full`. Its code wasn't audited |

## What this round fixed

Each fix has a regression test unless the audit report says otherwise. The biggest changes, as measured:

| What | Before | After |
|---|---|---|
| Planner against simulator, typical walls (median / p95 / worst ratio) | 2.04 / 4.39 / 5.47 | 1.00 / 1.35 / 1.73 |
| Same, concrete partitions | 1.25 / 3.17 / 5.16 | 1.02 / 1.20 / 1.52 |
| Map cells green while the sim fails, typical walls | 24.7 % | 0 % |
| "90 %" range coverage at 16 runs | About 81 %, labelled 90 % | Lowest and highest of 16, labelled about 88 %, measured 88 % |
| Lossless monopole power balance (FDTD) | 0.957, a real bug in the ground-plane row | 1.004 |
| Strip dipole power balance (FDTD) | 1.013 | 1.004, at the cost of a larger near-field box |
| Whole-scene stop | -30 dB, up to 3.55 dB wrong in a closed room | -60 dB, 1.2 to 2.2 times slower |
| Tank through an open valve at 2 m head | About 21 l/s | About 7.6 l/s, matching Torricelli with losses |

Also closed: a number reading as perfect when there was no data; practice data reaching "Calibrated"; calibrations leaking across scenes and bands; targets judged against what was reachable, not what was asked for; a failed solve printing as zero; "Likely" on a handful of runs; verdicts claiming more than their evidence; an open pipe end creating liquid.

The tests caught the audit overstating twice. FDTD-B1 predicted a 5 to 8 times slowdown (measured 1.2 to 2.2). FDTD-M7 predicted a 13 to 19 dB wall reading (measured 27 dB, slab estimate 30 dB). Treat the audits as leads and not as facts where no test has run.

### Results that moved, to check before trusting

- **`uwb_warehouse`, the stored layout, no longer holds its target:** one parked tag is 8.7 m off, probably a mirror solution from hearing only 3 anchors. Re-deploying gets to 24 anchors, up from 17, and reads Good. So the plan's anchor count rose by 40 % on a model assumption nobody has measured.
- **A UWB link through walls now has less range:** 39.7 m in the open, 30 m through wood, 19.9 m through a typical wall, 10 m through 10 cm concrete.
- **RFID wall loss:** 20 cm of concrete fell from 32 to 20 dB, within the published 7.5 to 22.5 dB.
- **Slower:** the FDTD patch took 114 s before and 159 s after in today's two gate runs, under different machine loads, so treat that as rough. The fix agent saw 200 to 280 s under heavier load. Whole-scene FDTD is 1.2 to 2.2 times slower, measured in cell-steps. Yield and mesh studies need more runs.
- **Tool JSON changed:** `chance_of_target` is gone from tag and shelf, `yield` is now `yield_about`, `extra_series_loss_ohm` is now `extra_series_resistance_ohm`, `zones_fully_covered` is now `zones_meeting_coverage`, and values with no data are null. Anything reading these needs updating.

## Still open

### Assumptions with no measurement behind them

These were chosen during the fixes because the model needed a value. Each one needs a measurement in Phase 4.

| Value | Where | What it's for |
|---|---|---|
| `HELD_VARIANCE` 0.5 | `src/rtls/radio.rs` | Share of ranging error that stays while nothing moves |
| `LINK_HOLD_M` 0.3 m | `src/rtls/radio.rs` | How far an end moves before a link's held state changes |
| RFID `wall_loss_db` 5 | `assets/devices/generic.json` | Judgement inside a wide published range |
| UWB "weak" -90 dBm | `src/rtls/rf.rs` | 5 dB above the sensitivity |
| `MAX_USABLE_SCALE` 2 | `src/rtls/fdtd/scene.rs` | When a scaled scene can feed the floor map (3 dB per edge) |
| Conducted power limits | `src/rtls/region.rs` | Table beside `regions.json`; confirm against the regulator's text |
| Tag and tunnel model-error factors | `crates/antenna_core/src/recipes/` | Assumed sizes |

### Known gaps in the models

| Gap | Effect | Would take |
|---|---|---|
| Delta-gap feed has no mesh limit (MoM) | Resonance keeps creeping, about 1.7 MHz per doubling at 1 GHz | A finite-gap feed model |
| FDTD and MoM feeds differ | About 1.3 %, or 32 MHz at 2.45 GHz, a third of the ISM band | Same feed model in both, then measure which is right |
| No CFIE in MoM | Closed metal bodies can be wrong near cavity modes, with no warning | CFIE or a cavity-mode check |
| FDTD metal loss on non-flat shapes | Only warned | Surface-impedance cells |
| Scaled whole-scene FDTD | Shadows about 10·log10(scale) dB per edge too optimistic | Ray tracing at the true band for large halls, or a scaled benchmark |
| Planner in sparse plans | Under-predicts: p95 ratio 1.8 to 2.5, up to 16.7 % of cells green but failing with 4 anchors through wood | Model link combinations and mirror solutions |
| 3-anchor mirror solutions | A tag can settle metres off with a good-looking fix | Ambiguity check in the solver, then count them as failures |
| No clock drift or air-time model | Many tags at high rates look fine | Clock and collision model (`clock_ppm` is still unread) |
| Multipath is a statistical stand-in | Real first-path errors aren't modelled | A channel model checked against measured impulse responses |
| Circular polarisation in site FDTD | Portal patterns are approximate | Polarisation in the source model |
| Stress covers one set of tag paths and up to 4 anchors | Most of the floor unsampled | Seeded floor sampling, more run time |
| Field yield is a quadratic with no cross terms | ±15 points | Check solves or cross terms |
| Drift with estimated tag positions | Moves under-reported | Reference tags or a joint fit |
| Plant has no torque or power; catalog NPSH is missing | Can't show an undersized motor | Catalog fields and a drive model |
| Solver warnings don't reach recipe claims | A run cut short now fails the job, but other warnings only reach the log | Pass warnings through to the claim |

### Not reviewed

- **Recipes added while the audit ran:** `src/outcomes/{acceptance,anchors,battery,compare,erces,esl,proximity,route}.rs`, `crates/antenna_core/src/recipes/{tunnel,medical,sar,gate}.rs`, `src/antenna/reconfig.rs` and `crates/antenna_core/src/reconfig.rs`. The session that wrote the recipes checked them against the rules and reports fixing every known gap, including a list from this audit. No independent review, and those last fixes aren't in the gate run above.
- **SAR:** `fdtd_sar_reference_dipole` and `fdtd_sar_reference_dipole_2450` are left out of the gate because, per the session that wrote them, their targets are "from memory". SAR is a safety figure, so it needs a published reference (IEEE 1528 or IEC/IEEE 62209 validation dipole values, for example) and shouldn't be relied on until then.
- **`crates/sim-core`:** benchmarks partly re-run, code not read. The "Verified" badges mean L1 at best: the benchmarks test the solver, not a user's model.
- **The UI:** changed text and buttons were compiled and only spot-checked, not reviewed screen by screen.
- **Regulatory rows** in `crates/knowledge/SIMULATOR_FINDINGS.md`: from reproduced copies, not gazettes.

## The plan

Effort figures are rough, for one engineer. Phase 4 depends on hardware and site access more than on effort.

### Phase 1: make results traceable (1 to 2 weeks)

Nothing later counts until this is done.

1. **Commit the work.** Done: main at 711d58d. It landed as large commits without review, so a code review is still worth doing.
2. **Stamp every result** with commit, solver version, inputs hash, seed, mesh and stop level, on claims, on reports and in the ledger.
3. **Run the gate in CI.** A nightly runner with openEMS, CalculiX and Elmer installed, plus the quick gate on every push. `tools/simulation_gate.sh` is the entry point.
4. **Keep golden results.** Store the benchmark figures, and fail on an unexplained move even inside the tolerance.
5. **Pull the remaining rule gaps out of the new recipes,** and review them independently.

*Done when:* a report can name the commit that made it, and CI fails on a broken benchmark.

### Phase 2: state the numerical error on every result (3 to 4 weeks)

1. **Make the mesh study part of the result:** every number used for a decision carries its numerical uncertainty, from the study and not assumed.
2. **Pass solver warnings to claims,** so anything open blocks L2.
3. **Add a finite-gap feed model** to both antenna solvers, and bring FDTD and MoM within 0.5 % on the reference dipole.
4. **Add a scaled benchmark** for whole-scene FDTD, or replace scaling with a ray tracer at the true band for large halls. Decide after measuring the shadow error against a real room in Phase 4.

*Done when:* no decision-grade number lacks a stated numerical error.

### Phase 3: close the model gaps that change decisions (4 to 8 weeks, alongside Phase 4)

Do these in the order measurements say they matter, not the table's order. Likely first: 3-anchor ambiguity, sparse-plan under-prediction, and a clock and air-time model. Add CFIE if a metal enclosure becomes a real use.

*Done when:* each remaining gap is fixed or its effect is measured and stated.

### Phase 4: validate against hardware (8 to 12 weeks, the real gate)

This is what turns a simulator into something to sign off on. It can't be done in software. Start small: each step below is useful alone.

| Step | Measure | Needs | Proposed pass line |
|---|---|---|---|
| 1. Antennas | S11 and gain of 3 reference antennas: wire dipole, patch on a Rogers board, UWB monopole | VNA, chamber time | Resonance within 1 %, S11 at its dip within 3 dB, peak realised gain within 1 dB, efficiency within 10 points |
| 2. UWB ranging | Range error against angle, tag on a turntable | 2 UWB modules, turntable | Bias against angle within 5 cm of `design_uwb_bias` |
| 3. Materials | Loss through real walls at 920 MHz, 2.44 GHz and 6.5 GHz: concrete, block, drywall, glass, a full and an empty rack | 2 antennas, VNA or radios | Within 3 dB of the model's per-wall value |
| 4. RFID shelf | Read rate on a real shelf, 3 product types | Reader, tags, shelf | Read rate within 10 points |
| 5. UWB site | Full system at 3 sites: open, racked warehouse, walled office. At least 59 randomly drawn points at each (`src/outcomes/acceptance.rs` plans them) | System, site access | Predicted p95 within 30 % of measured; the plan's anchor count holds up |
| 6. Plant | Pump curve and flow on a real or published rig | Rig or published data | Flow and head within 5 % |
| 7. Coverage | Across at least 20 scored claims, does a range hold what it says? | Steps 1 to 6 | A "90 %" range holds 80 to 97 % of measurements |

The pass lines are proposals, not standards: they need agreeing with whoever will rely on the numbers.

Then use what's measured:

- Replace the unmeasured assumptions above.
- Add a model-error term per recipe, sized from the measured error, so a range can honestly promise to contain a measurement.
- Record each result in the ledger as Measured, with its source.

*Done when:* the lines pass for a stated scope, and that scope is what the product claims. A model validated on warehouses is validated on warehouses.

### Phase 5: release the gate (about 2 weeks)

1. **Sign-off report:** refuses to generate unless the result is at L4. It names the commit, evidence, assumptions, measured points and uncertainty, and says what it doesn't cover.
2. **Outside review** of the validation by someone who didn't build it.
3. **Regulatory limits** confirmed against official texts, and the **GPL solver licences** confirmed before a commercial release (`docs/simulation.md` already says this).
4. **Say what's validated in the app,** per recipe, including where it isn't.

## Defaults taken, and what only the owner can do

These stand unless the owner says otherwise.

1. **First measurement:** UWB range against angle on a turntable, then a real RFID shelf. They're cheap, and they test the antenna and site models together.
2. **Pass lines:** the ones proposed in Phase 4, fixed before measuring so results can't move the goalposts.
3. **The `uwb_warehouse` sample:** left as it is, a known failure, until a measurement says whether 24 anchors is right. Changing it now would hide the question. The README still says 13 anchors.

Only the owner can do this: **provide hardware and site access** for Phase 4. Without it, the work stops at L2.
