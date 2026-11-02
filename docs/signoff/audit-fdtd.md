# FDTD / openEMS audit: can a customer sign off on this?

**Not yet.** Antenna Studio's solver core and post-processing are sound, so it is usable for an antenna with an explicit convergence study and a higher stop level. The whole-scene and through-object results should not be used for site sign-off. They carry the errors below, and nothing automated checks them.

**Evidence:**
- I ran no solver and no build.
- I recomputed numbers from old openEMS run folders in `%LOCALAPPDATA%\SenseSim\openems-fdtd\runs`, dated Oct 30-31. Their settings match today's code.
- Upstream statements are from openEMS `master`. The `v0.0.37` tag URL I tried returned 404, so I only confirmed the message text in the installed v0.37.0 `openEMS.dll`.

**Out of chain:** `openems_local.rs`, `io/openems.rs` and `tools/openems_rtls` are OpenEMS Edge, the energy-management system. I read their headers and grepped them for FDTD terms and found none, so they carry no solver results.

## BLOCKERS

**B1. Whole-scene runs stop after about one room crossing, not six. CONFIRMED (the extra loss on late multipath is not measured).**
- **Where:** `src/rtls/fdtd/scene.rs:305` has `end_criteria: 1e-3, max_timesteps: steps.max(5000)`; line 158 says "six crossings of the room (reverberation)".
- **Why:** energy falls to -30 dB once most of the wave has left the domain, so the step limit is never reached.

| Run | Planned window | Actual | Share |
|---|---|---|---|
| `1793375601_scene/tx0` | 274.4 ns | 37.31 ns | 13.6 % |
| `1793375943_scene/tx1` | 274.4 ns | 35.44 ns | 12.9 % |
| `1793432736_scene/tx0` | 274.4 ns | 45.65 ns | 16.6 % |
| `1793425136_scene/tx0` | 431.7 ns | 88.56 ns | 20.5 % |

- **Effect:**
  - In `1793432736`, the far corner is 10.08 m away. The stop falls 1.59 ns after the pulse peak passes it, so even the direct path reads -2.6 dB (assuming openEMS's Gaussian envelope, s = 3.48 ns).
  - Paths longer than about 10 m are lost entirely, and those carry most of the power in shadow zones.
  - The floor map and the material fit inherit this. Nothing reports it.
- **Fix:** use `end_criteria: 1e-6` so the planned window runs. Avoid 0, because as I recall openEMS treats 0 as unset. Also fail or warn if the last probe time is below pulse length + room diagonal/c.

**B2. Frequency scaling makes shadows optimistic by about 9-12 dB per edge at UWB. Path CONFIRMED; dB is a knife-edge estimate.**
- **Where:** `scene.rs:141-151`. The caption at `src/rtls/fdtd/ui.rs:998` says "the average level hold".
- **Attainability:** reproducing `plan()` at Fast quality:

| Space | Band | Simulated at | Scale | Cell | Cells at real frequency |
|---|---|---|---|---|---|
| 40×28×6.15 m hall | UWB | 0.383 GHz | ×17.0 | 68 mm (0.68 cells per real λ) | 106 G (about 10.6 TB) |
| same hall | BLE | 0.386 GHz | ×6.3 | 67.5 mm | 5.6 G |
| same hall | RFID | 0.385 GHz | ×2.4 | 67.8 mm | 0.3 G |
| 8×6 m office | UWB | 1.407 GHz | ×4.6 | 18.5 mm | 2.3 G |

- **Silent?** No. There is a badge, tiles, a caption and a floor-map note.
- **Effect** (knife edge, d1 = d2 = 5 m):
  - UWB, h = 1.5 m: 28.7 dB real vs 16.9 dB simulated.
  - UWB, h = 0.5 m: 19.4 vs 10.3 dB.
  - BLE, h = 1.5 m: 24.5 vs 16.9 dB.
  - Wall loss does roughly hold: a 20 cm concrete wall gives 26.9 vs 30 dB at ×17.
- **Fix:** state the shadow error, and block "Use in the floor map" and calibration above a scale limit until a scaled benchmark exists.

**B3. A through-object calibration at one band is applied to every band. CONFIRMED.**
- **Where:**
  - `ui.rs:824`: `loss_scale: (l as f32 / (6.0 * res.layout.target_weight))`
  - `src/rtls/radio.rs:127`: one process-wide table, not keyed by band
  - `radio.rs:376` and `:414`: the UWB first-path gate and link budget
- **Effect:**
  - Run `1793361366` measures 6.47 dB through its concrete wall at 915 MHz (-24.75 vs -31.22 dB S21).
  - Toggling calibration makes the UWB floor map use 6.47 dB, where P.2040 gives about 30 dB.
  - `wall_eq` becomes 6.47/6 = 1.08, which is at or below `max_walls` of 2, so a 20 cm wall stops blocking the UWB first path.
  - For RFID the "Floor map assumes" column uses 6 dB where the code uses 8 (`wall_loss_db`), giving 8.6 dB after calibrating to 6.47.
  - The caption says "for all technologies", but that disclosure does not fix the number.
- **Fix:** key `MaterialCal` by band, and use the technology's real `wall_db`.

**B4. FDTD ignores materials the user or a BIM file stated. CONFIRMED.**
- **Where:** `src/rtls/fdtd/mod.rs:763` calls `obstruction_of_asset(&o.asset)` only. `src/rtls/sim.rs:95` checks `radio::object_material(&o.name)` first.
- **Effect:**
  - A wall stated as thin (weight 0.15) still solves as concrete: about 14 dB at 2.44 GHz through 20 cm, against under 1 dB.
  - `fit_scene` then compares FDTD on the guess with the ray model on the stated material.
  - `rack_fill` is not used either, so racks stay solid PEC.
- **Fix:** one line, the same lookup as `sim.rs:95`.

**B5. The Positioning panel's FDTD Antenna tab feeds lossless directivity to the planner as realised gain. CONFIRMED. Worse than the findings row.**
- **Where:**
  - `src/rtls/fdtd/antenna.rs:180`: `kappa: 0.0`
  - `ui.rs:590`: `d: p.d...`
  - `src/rtls/planning.rs:191`: `v.max(-30.0) + self.mismatch_db`
- **Effect:** by the README's own figures (5.75 dBi, 44 % efficient), real gain is 2.2 dBi. Every device and library save is 3.6 dB high. Studio's `planner_antenna` correctly uses `Quantity::Gain`.
- **Fix:** set kappa from tanδ and apply efficiency, or disable "Use on all" for dielectric patches.

**B6. WiFi at 5.18 GHz is simulated at 2.44 GHz. CONFIRMED.**
- **Where:** `mod.rs:795-796` has `Tech::Ble | Tech::Wifi => antenna::Band::Ble`; `assets/devices/generic.json:147` has `wifi-ap-rtt` at 5180; `planning.rs:297` adds `fspl_db(1.0, node.freq_mhz)`.
- **Effect:** +6.54 dB everywhere, plus concrete at 66 instead of 119 dB/m. That is about 17 dB optimistic through 20 cm. The label reads "BLE · 2.44 GHz".
- **Fix:** derive the band from `prof.freq_mhz`, or refuse the run.

## MAJOR

**M1. The truncation warning is dead code in Studio and absent elsewhere. CONFIRMED.**
- **Where:** `src/antenna/run.rs:158-161` scans stdout for `"number of timesteps"` and `"before"`.
- **Why:** upstream prints `cerr << "RunFDTD: Warning: Max. number of timesteps was reached before..."`, so `truncated` stays false. `mod.rs:475` (RTLS, region, scene) has no check at all. There is no energy check after the run. `src/outcomes/hardware.rs:347,370` also drops `warnings`.
- **Effect:** a cut run looks clean. The stderr text only reaches a collapsed log.
- **Fix:** scan stderr and join that thread before returning. Compare the last energy reading with the target in all three paths.

**M2. NaN or a dead port becomes a green "Matched". Mapping CONFIRMED; reachability SUSPECTED.**
- **Where:**
  - `crates/antenna_core/src/lib.rs:43`: `20.0 * v.abs().max(1e-15).log10()`. NaN gives -300 dB.
  - `src/rtls/fdtd/post.rs:160`: same pattern, -240 dB. `post.rs:67` turns 0/0 into 0.
  - `crates/antenna_core/src/farfield.rs:166`: NaN accepted power gives 100 %.
  - `post.rs:85`: `if let (Some(Ok(a)), Some(Ok(b)))` skips unparsable lines. MSVC's `-nan(ind)` would be dropped, leaving a finite pre-divergence series.
  - `scene.rs:459`: NaN power becomes "unknown".
- **Solver side:** openEMS has no NaN check. With NaN energy the loop exits and takes the "end-criteria reached" branch.
- **Related:** `crates/antenna_core/src/csx.rs:88,93` can merge a port gap smaller than 0.45 of the finest cell, which would give a dead port.
- **Fix:** bail on non-finite probe samples and on near-zero incident wave. Protect port lines from merging. Check dumps for NaN.
- **Sound part:** `scene.rs:573` already rejects NaN accepted power on the scene path.

**M3. Studio's default stop is -30 dB, not -40 dB. CONFIRMED. The findings row is stale and understates this.**
- **Where:** `crates/antenna_core/src/project.rs:115`: `end_db: 30.0`.
- **Effect:**
  - Remaining amplitude is 3.2 %, so S11 at -10 dB is ±0.8 dB and at -20 dB is about ±3 dB.
  - The README's -34 dB patch figure (0.02) sits inside the error (0.032).
  - Power is ±6 %, or ±0.27 dB, so the 1.05 efficiency guard cannot catch it.
- **Fix:** default to -50 dB and add a single-port note like the one at `analysis.rs:339`.

**M4. The scene mesh ignores εr. CONFIRMED.**
- **Where:** `scene.rs:141` has `C0 / (f * 1.15) / n`. `src/rtls/fdtd/region.rs:123` caps at `.min(10.0)`.
- **Effect:**
  - Concrete gets 4.37 / 5.24 / 6.55 cells per λ (Fast / Normal / Fine), with about 5.8 % phase error at Fast.
  - Glass gets 3.98 at Fast.
  - Liquid (εr 72) gets 1.18 / 1.41 / 1.77, below the grid's 2-cell limit.
  - Region mode gives liquid 3.7 at Fast.
  - The label still says "Fast (λ/10)".
- **Fix:** show cells per λ in the worst material and warn below 10.

**M5. Voxelisation changes or removes thin objects. CONFIRMED.**
- **Where:** `region.rs:317-319` uses `chunks_exact(2)` with cell-centre rounding. `region.rs:287` lists every object regardless.
- **Effect (68 mm cell):**
  - A 10 cm wall becomes 1 or 2 voxels depending on grid offset, a 7.9 dB swing.
  - 12.5 mm drywall vanishes 82 % of the time, otherwise it is 5× too thick.
  - 6 mm glass vanishes 91 % of the time.
  - Single-sided non-metal meshes vanish. Overlapping parts leave holes through the parity rule. The Materials table still lists them.
- **Fix:** report per-object voxel thickness, and warn on zero or a >25 % mismatch.

**M6. Site sources use one peak-gain offset at all angles. CONFIRMED; magnitude SUSPECTED.**
- **Where:** `mod.rs:846` and `scene.rs:35`.
- **Effect:** a 12 dBi portal is modelled as a 7 dBi dipole plus reflector, +5 dB everywhere. Off-axis and back-lobe power comes out about 8-10 dB high. Circular polarisation is not modelled.
- **Fix:** apply the device pattern ratio per direction, or mask the map outside the beam.

**M7. Through-object results can measure the patch's edges, not the wall. SUSPECTED.**
- **Where:** `region.rs:134-150`, which shrinks the region silently.
- **Arithmetic (UWB, Fast, 20 cm concrete):**
  - Cell 1.76 mm, 180 cells through, 6 M budget, leaves a square of about 0.32 m.
  - Each edge diffracts at about -25 dB (v = 4.06). Four edges sum to -13 to -19 dB.
  - The reading is about 13-19 dB against about 30 dB true, and that number can then be applied as a calibration.
- **Fix:** carry voxels through the PML so there is no edge, and warn when the region was shrunk.

**M8. Mesh grading is unbounded despite the 1.4 claim. CONFIRMED.**
- **Where:** `src/rtls/fdtd/csx.rs:212`: `if gap <= max_cell * 1.0001 { out.push(b); continue; }`. The Studio mesher has the same bypass at `crates/antenna_core/src/csx.rs:130`.
- **Data:** run `1793361366` has adjacent cells at 31:1 (min 0.40 mm, max 12.45 mm). Run `1793372227` has 21:1.
- **Effect:** time step 2.69 ps instead of about 24 ps, so about 9× more steps. The RTLS mesher has no minimum-cell warning. At its 12 µm merge limit, the 400 k step cap would be shorter than the pulse.
- **Fix:** merge lines closer than a quarter cell, as Studio's `tol` does, or snap dipoles to the voxel grid as `source()` does. Add a test for the gap ≤ cell case.

**M9. The scene height is capped at 6 m. Clipping CONFIRMED; off-grid behaviour SUSPECTED.**
- **Where:** `scene.rs:130`: `.clamp(2.0, 6.0)`.
- **Effect:** anything above 6 m is clipped silently. A transmitter above 6 m has its port off the mesh, while the planner can place anchors up to the ceiling. Tall sites are affected.
- **Fix:** refuse the run or raise the cap.

**M10. PML clearance is short at the low edge of wideband templates. CONFIRMED; S11 effect not measured.**
- **Where:** `crates/antenna_core/src/csx.rs:252-253`.
- **Effect:** structure-to-PML clearance is 0.142 λ_lo (`uwb_monopole`), 0.150 (`discone`), 0.164 (`vivaldi`). The RTLS patch is 0.157. That is inside the reactive near field (0.159 λ), at the edge that defines their bandwidth.
- **Fix:** size the pad from `C0/f_lo/4`.

**M11. Yield counts crashed solves as failed samples. CONFIRMED.**
- **Where:** `src/antenna/mod.rs:478`: `.ok().and_then(...).unwrap_or(0.0)`.
- **Effect:** a 30 % crash rate reads as a 70 % yield at best.
- **Fix:** count failures and abort or report them.

**M12. The test suite is neither automated nor tight. CONFIRMED.**

There are no CI files in the repo. The README says the figures come from `cargo test -p antenna_core`, but the FDTD rows need `--bin SenseSim --features antenna-studio -- --ignored` and an openEMS install.

| Test | What it asserts |
|---|---|
| Studio dipole | D ±0.35 dB, efficiency ±12 %, any resonance ±12 % |
| Studio monopole | D ±0.6 dB, efficiency ±20 % |
| Studio patch | any resonance ±6 % (±147 MHz, wider than the 83.5 MHz ISM band), D 4-9 dBi, efficiency 0.1-0.8 |
| FDTD vs MoM | resonance 6 %, D 0.5 dB |
| RTLS dipole | ±8 %, Fast only |
| Wall S21 | loss 2-40 dB |
| Scene | LOS ±3 dB, wall >5 dB, and it asserts the run is unscaled |
| `fdtd_checks` in `hardware.rs` | one sign test, two with no asserts |

- README's 74+j4 Ω, -34 dB and 44 % are only printed. The horn row appears only in the print-only `antenna_fdtd_template_audit`.
- Not covered: every scaled scene (so all whole-scene UWB and BLE on hall-sized sites), metal and racks, the RTLS patch, RTLS Normal and Fine, truncation and NaN paths, thin walls.
- **Fix:** add scheduled CI with an openEMS install. Tighten to about ±1-2 % resonance, ±0.2 dB, ±3 %. Add one scaled benchmark.

**M13. Convergence checking is opt-in and S11-only. CONFIRMED.**
- **Where:** `src/antenna/mod.rs:410-435` starts at 12 cells per λ regardless of the user's setting. `crates/antenna_core/src/report.rs:120` omits density, stop level and convergence status.
- **Effect:** a normal run shows no convergence state, and a report cannot be audited. Gain is displayed per pass but not tested.
- **Fix:** store these on `SimResult` and print them in the report.

**M14. Non-flat "Copper" stays lossless in FDTD. CONFIRMED.**
- **Where:** `crates/antenna_core/src/csx.rs:602-605` partitions on `is_flat`, and wires, cylinders and solids go to `<Metal>`. The MoM solvers apply surface impedance (`mom\wire.rs:510`). Sheet thickness is fixed at 35 µm (`csx.rs:18`).
- **Effect:** a small stainless helix (about 4 Ω loss against about 4 Ω radiation) reads 100 % efficient instead of about 50 %.
- **Fix:** warn, or model surface loss on those shapes.

## MINOR (all CONFIRMED by reading)
- `post.rs:131` and `run.rs:397`: `.abs()` hides negative accepted power.
- `project.rs:455`: Drive mode skips cross-terms. With S11 = 0.1 and S21 = 0.3, gain is off by 0.7 dB. Realised gain is unaffected.
- `run.rs:398`: FDTD far fields carry the pulse's phase, because the scale is `1/√p_inc` and not `1/a`. Exported `.ffd` and `.ffe` phases are off by -2πf·t0.
- `farfield.rs:435`: `.ffe` header says "Gain" over Directivity columns.
- RTLS dipole S11 is referenced to 73 Ω with no label (`antenna.rs:144`). Z = 74+j4 reads -35 dB, but -14 dB at 50 Ω.
- `material.rs:108`: Water tanδ 0.05 is labelled "1-6 GHz". Debye gives 0.045 at 915 MHz, 0.119 at 2.45 GHz, 0.289 at 6 GHz.
- `region.rs:340`: the triangle sampling clamp of 400 leaves holes in large single-sided metal sheets (14 m edge: 35 mm spacing on a 20.5 mm cell).
- `scene.rs:458` averages zero-field voxels inside metal into nearby points.
- `hardware.rs:203`: claims "Metal only" even on the FDTD path.
- The UI says "ITU-R P.2040 materials" for Liquid, Thin and Generic, which are not in the table.

## SIMULATOR_FINDINGS Antenna rows

| Row | Status |
|---|---|
| `material.rs:94-95`, `:98`, `:92` / `:32` | Holds, same lines |
| `fdtd/antenna.rs:132` (FR-4 4.3) | Holds |
| `fdtd/antenna.rs:180` (κ = 0) | Holds, worse (B5) |
| `fdtd/antenna.rs:152` (-40 dB) | Holds |
| `Setup::for_band` (-40 dB) | **Stale, now -30 dB** (`project.rs:115`) |
| PML 0.383 λ0 | Holds for RTLS dipole and monopole. RTLS patch is 0.314 λ0. Studio moved to `csx.rs:252-253`; low-edge is worst on wideband templates (M10) |
| Probe at L/6, `templates.rs:642` | **Stale for Studio**, now `yf = 0.25*L` at `:644`. Still L/6 at `fdtd/antenna.rs:137` |
| `Redge`, slot Z0 rows | Not re-verified |
| `fdtd/csx.rs:189-190` (no thirds rule) | Holds for RTLS only. **Stale for Studio**, which implements it and tests it |

## Checked and found sound
- **Time alignment:** the current probe's own time column carries the half-step (run data: t0 = 27.43 ps for I, 0 for U), and each series is DFT'd on its own times.
- **S-parameters:** wave formulas and impedance from S are correct. The DFT needs no window once fields have decayed, and the 2·dt scaling is consistent.
- **Near to far field:** surface currents, Balanis signs, -jk/4π, phase sign, trapezoid weights and ground images are correct. E and H share planes on mesh lines. README's 1.013 dipole efficiency is consistent with this.
- **Quantities:** directivity, gain and realised gain use the right power references. Power and field dB are correct. RHCP/LHCP and Ludwig-3 are correct. The p_rad pole cap and horizon halving are correct.
- **Efficiency:** not clamped; above 105 % it shows a warning. Between 100 and 105 % it is shown without a flag.
- **Excitation:** the pulse covers the sweep in all modes, with -20 dB at the edges.
- **Studio mesh:** √εr in dielectrics, at least 3 cells through thin ones (1.6 mm FR-4 gets h/3), thirds rule on sheets.
- **NF2FF placement:** λ/4 from the structure and 4 cells from the PML.
- **Materials:** P.2040 coefficients for concrete, wood and glass are as I recall the table (not re-checked against the document), and evaluated at the real band. κ = ωε₀εr·tanδ is exact at the design frequency, as documented.
- **Failure handling that works:** a non-zero exit status, missing probes, a scene with no accepted power, and region overflow all bail with an error.
- **Scene physics that holds:** the Friis scaling is correct, and multi-port runs enforce a -50 dB stop with a UI caveat.
- **Region S21 stability:** in my recomputation from run probes, S21 moved at most 0.09 dB between the last stop times. The 915 MHz wall result of 6.47 dB is close to my slab estimate of about 7.6 dB.
