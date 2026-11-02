# Audit of Sense Sim's prediction and evidence layer and plant simulation

**Answer to the customer: no, these numbers can't support a deployment sign-off yet.** A number can reach "Calibrated to measurements" without a real delay measurement. The "90 %" ranges aren't 90 %, and several passes mean little.

I read the code only and ran nothing, so all arithmetic is by hand. CONFIRMED means I traced it in code, not that I reproduced it at runtime. Line numbers may move, so the code is quoted.

## BLOCKERS (all CONFIRMED)

**B1. Practice data, or no delay data at all, can reach Calibrated through label parsing.**
`src/outcomes/survey.rs:276-278`, `:136`
`let had_delay = MaterialCal::applied(*mat).is_some_and(|c| c.label.contains(DELAY_FITTED));`
`label.starts_with(FIELD) && label.contains(DELAY_FITTED) && label.ends_with(...)`
- **Path 1:** fit a practice walk with `apply=true`, which gives the label "Practice walk delay…". Then fit a real RSSI-only walk. `had_delay` is true and `bias.unwrap_or(cb)` keeps the synthetic delay, so the new label is "Field survey delay · … · scene". `evidence_now` then returns Calibrated.
- **Path 2:** `contains("delay")` searches the whole label, including the path. A loss-only field walk from `delay_walk.csv` passes, and so does a scene with "delay" in its name.
- **Test gap:** `a_practice_walk_never_counts_as_a_calibration` only tests a single walk.
- **Fix:** replace string parsing with typed fields on `MaterialCal` (`delay_origin: None|Practice|Field`, `scene_id`). Clear the origin when a practice fit touches it, and never inherit a delay across origins.

**B2. "Field" is the default, and the synthetic flag is easily lost.**
`survey.rs:34-35`, `:61`; `src/outcomes/tools.rs:199-201`
`#[serde(default, ...)] pub synthetic: bool` and `Reading { ..., synthetic: false }`
- Every CSV row is treated as real.
- The tool's inline `readings` format omits `synthetic` in its own error text, so an agent re-passing a practice walk produces a Field fit.
- Converting a practice walk to CSV also strips the flag.
- **Fix:** record a hash of each practice file and refuse those files in `site_survey_fit`. Require a declared origin before a fit can count as Field.

**B3. Five readings at one spot through one anchor give Calibrated.**
`survey.rs:244`, `:262`, `:271`; `crates/proof/src/calibrate.rs:236`; `src/outcomes/ui.rs:159`
`if links.len() < 5 { bail!("... take at least 10") }` and `p.learned >= 0.5` and `json!({ "path": path, "apply": true })`
- **Arithmetic for concrete** (weight 2, bias 0.35, spread 0.25):
  - Sensitivity is J = 2·0.6 = 1.2, and reading variance is σ² = 0.15² + 0.5² = 0.2725.
  - Five readings give an information matrix of [[30.42, 22.02], [22.02, 29.46]] for scale and anchor delay.
  - The posterior variance of the scale is 0.0716, so `learned` = 1 − 0.0716/0.25 = 0.71. The material is applied.
  - Hold-out size is 1.
- **Repeats inflate it:** repeated readings are treated as independent, so one stationary burst pushes `learned` higher.
- **Prior loophole:** `learned` is relative to the prior. `prior_sigma=5` gives `learned` = 0.99 from a single reading.
- **Missing gates:** there is no goodness-of-fit test, and the fit is never checked against the actual scatter. `converged` and `rms_after` are dropped from `SurveyFit`, and a value clamped at 0 or 6 still counts. The UI always applies.
- **Fix:** require a minimum number of distinct spots and anchors per material. Use an absolute posterior σ threshold, and require a passing residual check and a better held-out score before Field status.

**B4. The held-out score is leaky, and it scores a model that is never deployed.**
`survey.rs:250`, `:258`, `:271`
`if i % 5 == 4 { b.push(l.clone()) }` and `rms_by_kind(&test, &slots, &cal_train.values())`
- **Leakage:** the split follows file order, not spot. Other anchors at the same spot, and repeats of the same link, stay in training. The module header promises trust "for the points nobody measured", but no spot is ever withheld.
- **Aliasing:** with 5 anchors, the hold-out is all one anchor.
- **Wrong model:** the "after" score uses fitted anchor delays, level offsets, the path-loss exponent and scales that were learned too weakly to apply. `apply` writes only material scales with `learned >= 0.5`. The tool says it "uses what was learned in every prediction".
- **Effect:** with a fitted exponent of 2.8 against a built-in 2.2, the applied model is off by 10·0.6·log10(20) = 7.8 dB at 20 m. The reported score doesn't show this.
- **No gate:** `apply` runs even when the held-out error gets worse.
- **Fix:** hold out whole spots, score exactly what gets applied, report the sample size and an interval, and refuse to apply on a worse score.

**B5. Calibrations aren't bound to a scene, and claims misstate what the materials are.**
`src/outcomes/stress.rs:208`; `src/rtls/radio.rs:127`; `src/tabs.rs:139`, `:268`; `src/ui/topbar.rs:115`
`_ => "Materials are built-in typical values: run a survey walk to replace them".to_string()`
- **Numbers leak across tabs:** the scales are process-wide. Switching between loaded tabs doesn't clear them, because `begin_load` only runs for lazy tabs. So scene B's numbers use scene A's fit while reading "Simulated" and "built-in".
- **Practice walks shift numbers too:** a practice `truth` of `{"concrete":0.2}` lowers the error and can flip the verdict, with the same false text on the claim. FDTD calibrations are also not disclosed. The README admits the tab sharing, but the claim itself doesn't.
- **The label can transfer between scenes:** it matches on the scene title, which is a file stem or blank and is editable. Two `warehouse.json` files in different folders match, and so do two blank tabs.
- **Edits don't invalidate it:** changing the geometry after the walk leaves the fit in place.
- **Fix:** store a scene ID plus the geometry signature (`scene_signature` in `src/rtls/sim.rs:218` already exists). Build the assumption text from the actual `MaterialCal` state, and keep calibrations per tab.

**B6. Calibrated gains trust from a partial fit, so the weakest input doesn't set the label.**
`stress.rs:117`; `radio.rs:368`, `:376`; `crates/proof/src/claim.rs:9-10`
`if !present.is_empty() && present.iter().all(fitted) { Evidence::Calibrated }`
- The comment says "a claim is only as strong as its weakest input", but nothing I read combines evidence levels.
- Only delay is required. Link gating uses `wall_eq`, which is uncalibrated. Range noise, dropouts, assumed rack fill and assumed crowds are all still in the number.
- For metal and liquid, `self.blocker.is_none()` kills the link whatever the fit says. The "metal delay" fit changes nothing, yet it is required for the label.
- `MaterialCal` has no σ, so the posterior is discarded. The ±30 % log-normal factor (σ 0.3) stays the same before and after calibration, so "predict again" never narrows the range.
- **Fix:** compute the label as the minimum across inputs, name what was actually fitted, and feed the posterior σ into the factor.

**B7. The `site_stress` p95 only counts tags that have a fix, so most tags can be lost and it still passes.**
`stress.rs:76-80`, `:199`; `sim.rs:744`
`((uwb_tracks > 0 && p95 > 0.0).then_some(p95), located, uwb_tracks, reads)` and `r.0.map_or(f64::NAN, |x| x as f64)`
- `located` is ignored in the verdict. If 9 of 10 tags are lost and one keeps a 20 cm p95, that combination passes a 30 cm target.
- Only combinations with no fix at all count as misses.
- **Fix:** treat missing fixes as infinite error, or make the metric require all tags located.

**B8. The stated "90 %" and "100 %" are not supported at the sample sizes used.**
`crates/proof/src/lib.rs:35`; `crates/proof/src/robust.rs:165`; `claim.rs:116`; `stress.rs:132`, `:193`
`1.96 * (p * (1.0 - p) / n as f64).sqrt()` and `Some(p) if p >= 0.9 => Verdict::Likely` and `samples: o.samples.max(4)`
The code computes interpolated (type 7) sample quantiles. For a new independent draw, coverage is about (r_hi − r_lo)/(n+1):

| n | Used by | Expected coverage of the "90 %" range |
|---|---|---|
| 4 | stress minimum | 54 % |
| 12 | `site_material_ranges` default | 76 % |
| 16 | `site_stress` default | 79 % |
| 200 | tag recipe | 89 % |
| 2000 | field yield | 90 % |

- Latin hypercube sampling helps only in the best case, about 84 % for one monotone factor at n=16.
- Below n=19, even the sample minimum to maximum can't reach 90 %, because (n−1)/(n+1) < 0.9.
- 16/16 passes shows "100 %" with a margin of exactly 0. The 95 % Clopper-Pearson lower bound is 79 %. For 15/16 ("Likely") it is 70 %, and for 4/4 it is 40 %.
- `pass_margin` and n never reach the claim. There is no t or other small-sample correction anywhere.
- **Fix:** store n and the margin on the claim. Use Wilson or Clopper-Pearson intervals. Base "Likely" on the lower bound, which needs at least 29/29 passes to reach 0.9. Show the minimum and maximum at small n instead of a "90 %" range.

**B9. The range covers only assumed input spread, but the ledger promises coverage of real measurements.**
`claim.rs:226`, `:235`; `robust.rs:158-159`
`"{hit} of {total} measurements landed inside the predicted range (a 90 % range should catch about 9 in 10)."`
- No recipe has a model-error or measurement-noise factor, so even a large n doesn't make the range a prediction interval.
- Tagging is a clear example: its own test accepts ±8 % resonance error, while the only tag tolerance factor is 1 %.
- `hit_rate` pools repeated checks on one claim as if independent and shows no interval. A 9/10 result has a 95 % interval of 55–99.7 %, and "1 of 1" is shown in bold.
- **Fix:** relabel it "spread across assumed inputs" until a model-error term exists. Count per claim, show an interval, and hide the 9-in-10 line at low n.

**B10. "Likely to meet the target" ignores the evidence behind it.**
`claim.rs:116`; `src/outcomes/hardware.rs:138`; `ui.rs:57`
`if t.met_by(p.median) { Verdict::Likely } else { Verdict::Unlikely }`
- `design_tag` is described as "for ranking only", yet it can show a green "Likely to meet the target (100 %)".
- A single solve with no range also gets Likely, although the enum says "across nearly the whole range".
- **Fix:** cap verdicts by evidence, for example "Worth testing" for an Estimate and "Meets in this one solve" for a point value.

**B11. `healthy: true` can come from the simulator checked against itself, with no label.**
`tools.rs:128`, `:299`, `:305`
`None => live_samples(...)` and `json!({ "healthy": r.healthy, ... })`
- With no path, the ranges come from the same model they are compared against. It can only catch scene edits, and the solver already knows the true anchor positions.
- The output has no source or evidence field, and no false-alarm rate is stated anywhere.
- **Fix:** add a `source` field. Don't output `healthy` for simulated input, and call that mode a drill.

**B12. A missing tolerance gives 100 % yield.**
`hardware.rs:506`; `crates/antenna_core/src/recipes/fit.rs:182`
`v["sigma_m"].as_f64().unwrap_or(0.0)`
- `{"name":"L","sigma":1e-4}` or `"sigma_m":"0.1mm"` becomes `Dist::Fixed`, so the dimension doesn't vary. If the nominal design meets the target, the result is yield 1.0 and "Likely".
- **Fix:** bail when the value is missing or not a number.

**B13. Plant: an open pipe end creates liquid and still reports `ok: true`.**
`src/plant/sim.rs:837`, `:937`, `:955`, `:1543`, `:1454`
`n.fixed = Some(n.elev);` and `if node.open && inflow > 1e-6` and `"ok": alarms.is_empty() && flow.leaks.is_empty()`
- A riser open above the tank surface fills the tank from nothing, because head at the open end exceeds the tank's and only outward flow counts as a leak.
- The same happens to a pump whose suction pipe ends in open air. `reaches` explicitly accepts "an open end".
- `mass_balance_error_pct` shows the problem but isn't part of `ok`. It is also blind to one case: when a tank empties, `*spilled += *v` books the excess as a negative spill.
- **Fix:** let an open end only discharge, with no inflow unless it is a declared source. Include the mass-balance error in `ok`.

**B14. Plant: there are no entry or exit losses and no velocity head.**
`sim.rs:786`, `:837`
- Tanks and open ends are bare fixed heads, and there is no test against Torricelli's result.
- **Example:** a DN50 tank nozzle to an open valve with 2 m of head has K = 0.217 (0.15 + 0.067 friction).
  - The code gets v = 13.4 m/s, or 21 l/s.
  - Adding entry 0.5 and exit 1.0 gives K = 1.717, v = 4.8 m/s, or 7.6 l/s.
  - That is 2.8× too high.
- A 1 m pipe between tanks is about 2.2× high. Long runs are barely affected.
- **Fix:** add K = 0.5 and K = 1.0 at those boundaries.

## MAJOR (CONFIRMED)

**M1. Evidence labels are missing outside the ledger.** `hardware.rs:199`, `:266`, `:304`, `:409`, `:489`, `:529`; `tools.rs:218`
- `design_job` results for shelf, UWB bias, AoA, integrate, VNA fit and yield carry no label. Neither do survey fits, drift or any plant output.
- A search of `src` for phrases such as "validated" or "real hardware" found no such caveat. The "nothing validated on hardware" statement appears only in the README.
- Fix: add `evidence` to every tool result, and put the disclaimer in the UI and in `to_markdown`.

**M2. UWB bias overstates its coverage and hides failures.** `crates/antenna_core/src/recipes/uwb_bias.rs:333-334`; `hardware.rs:203`, `:215`
`metric: "95 % of directions within"`, `return BiasMetrics::default();`, `if *ok { *v as f32 } else { 0.0 }`
- Directions in nulls are excluded. At 50 % usable, the claim really covers 47.5 % of directions. Usable share isn't on the claim.
- No usable directions gives 0 cm and "Likely".
- Nulls are saved to the simulator as zero error, which is the most optimistic value possible.
- "After calibration" means the optimal centre over the region, not a bench direction, so the real error can be up to about 2× higher.
- The assumption text "Metal only" is hard-coded even on the FDTD path.

**M3. Failed numbers print as zero.** `hardware.rs:122`
`if v.is_finite() { ... } else { 0.0 }`
- NaN or infinity becomes 0.0 in all recipe JSON, so a failed value reads as 0 cm, 0 dB or 0 % yield.
- Fix: return null. I did not trace a live trigger for this one.

**M4. Plastic typed with an unknown name is solved as metal.** `crates/antenna_core/src/mom/mod.rs:124`; `crates/antenna_core/src/mom/surface.rs:257`; `crates/antenna_core/src/project.rs:226`
`if let Some(m) = material::by_name(&s.material) && !m.is_conductor()`
- Any name not in the library passes as a perfect conductor, and an unknown substrate silently becomes Air.
- Fix: bail on unknown names.

**M5. Integration compares two different solvers.** `hardware.rs:398-400`, `:411`
- With a plastic shell, `before` is MoM and `after` is FDTD, so solver differences are reported as enclosure detuning.
- The verdict uses mismatch at band centre only, with a hard-coded 1 dB target.
- Fix: use the same solver for both sides, and judge the worst point in the band.

**M6. The AoA table is tested on the data that built it.** `crates/antenna_core/src/recipes/aoa.rs:141`, `:234`; `hardware.rs:288`, `:312`
- Truth and table use the same fields at the same grid points. Its "error" is only the assumed perturbation.
- It uses one random array draw (seed 1) and a single frequency. Across BLE channels (±1.6 %), the error at 60° is tan60·0.016 = 1.6°, while the test asserts under 0.5° with no perturbation.
- The figure is then recommended as the simulator's `aoa_sigma_deg`.
- Fix: test off-grid, across frequency, against a perturbed array, and across many draws.

**M7. Tagging runs well outside its fit, with no warning.** `crates/antenna_core/src/recipes/tagging.rs:122`, `:167`, `:238-247`
`(d / 0.002).powf(-0.79)`
- **Fit quality:** about 12 constants against 8 solved rows, with no independent check. Tolerances are ±8 % resonance, R within 40 % of max(R, 25 Ω), and ±1 dB. A resonance error of 8 % moves X by about 32 Ω at Q=10, which is about 25 % of range.
- **Extrapolation:** the solves cover gaps of 2–20 mm and an 81 Ω strip. Recommendations use 0.4–46 mm and 6–25 Ω tags.
- **Absolute range is optimistic:** the free-space wet inlay computes to 24.0 m, against the code's own "10–20 m" comment. Polarisation loss is missing, and adding 3 dB gives 17 m.
- Fix: warn outside the fitted range, add a model-error factor, and don't quote pass probabilities.

**M8. Coexistence leaves key effects out of the number.** `crates/antenna_core/src/recipes/coexist.rs:120`, `:123`, `:178`, `:185`
- Harmonics are only a note. RFID to UWB channel 5 at 40 dB isolation gives 0.8 dB desense and a range share of 0.92. A −60 dBc 7th harmonic would really give about 11 dB desense and a share of 0.31.
- Touchstone isolation uses the nearest sample however far away it is, so a 2–3 GHz sweep answers for 6.5 GHz.
- The 3 dB-per-dB blocking penalty and the 12 dB/octave selectivity have no source.
- `_ => 20.0` is a hidden fallback.
- The figures are typical-only, and the label correctly stays Estimate.

**M9. The VNA fit is ill-posed.** `fit.rs:36-37`, `:71-82`; `hardware.rs:491`
- It uses magnitude only, and a given |Γ| has two possible R values on either side of Z0. With sim R=30 Ω at −12 dB, the fit can't separate 0 from +53 Ω. R is also bounded ≥ 0.
- `extra_series_loss_ohm` can equally be radiation resistance, so it isn't necessarily loss.
- The σ of 1 dB on correlated VNA points shrinks with point count, with no new information.
- Z is held flat outside the sweep, and the job solves only the measured band, so the top is clamped for any scale above 1.
- Fix: use complex S11, or report the fit as ambiguous.

**M10. Field yield's precision is misleading.** `fit.rs:139-140`, `:292`
- It uses a quadratic per dimension with no cross terms, through ±1σ.
- Its own test accepts 40–70 % against a true 53 %.
- `pass_margin` reflects only the 2000 draws on that surrogate, not the surrogate's error.
- The label says Simulated, though only 2k+1 points were solved.

**M11. `site_stress` covers less than it claims, and what it omits doesn't reach the ledger.** `stress.rs:72-74`, `:187-191`, `:214`; `sim.rs:969`
- The dead anchor, 20 people per 100 m² and materials 1.5× worse are not in the verdict. The worst scenarios are absent from the ledger claim.
- At most 4 anchors are tried.
- It uses one noise seed, 10 s, and tag paths from the same start, so most of the floor is unsampled.
- At n=16, a rank correlation needs r² of about 0.25 to be significant, yet "most uncertain because of" lists factors unqualified.
- Fix: put the scenario results on the claim and vary the seed.

**M12. Drift detection has blind spots and untested conditions.** `src/outcomes/drift.rs:74`, `:171`, `:219`; `tools.rs:125`
- Noise is fixed at 0.12 m. A device with σ=0.35 m reads healthy, because 0.35 < the 0.36 limit.
- A dead zone exists: delay-shift evidence fails above 0.24 m RMS and "unexplained" only starts above 0.36 m. A 15 cm delay shift at σ=0.30 gives RMS 0.335 and no finding.
- Samples are assumed independent. For a static tag with a fixed multipath bias, effective n is about 1, so P(|x| > 0.15) at σ=0.12 is about 21 %, not the near-zero implied by n=20.
- Ranges the solver rejected are filtered out before the check by `l.used`.
- The README's file format omits `expected_excess_m`, so any wall on a healthy site reads as drift.

**M13. The ledger can't serve as an audit record.** `claim.rs:208-209`; `ui.rs:147-148`; `claim.rs:135`
`c.checks = std::mem::take(&mut self.claims[i].checks);`
- Re-running a VNA fit on the same file path with an unchanged prediction silently drops the new measurement, despite the promise that "a measurement is never lost".
- "Remove" deletes misses from the record.
- Checks carry no timestamp or source.
- `Factor.basis` ("Reports show it"), n, failures and seed aren't stored on claims.

**M14. Material ranges overstate thick BIM walls.** `src/outcomes/spec.rs:43`, `:190`
- `scale_for` assumes `m.weight()`, but BIM weights scale with thickness. A 30 cm concrete wall at "30 dB" costs 90 dB, which biases "Decide first".

**M15. Plant geometry falls back to wrong values.** `sim.rs:815-816`; `src/plant/ports.rs:378-381`
`.unwrap_or((0.5, 0.5))`
- Any catalog or detected pipe or hose counts as 0.5 m with K = 0.5, so 30 m of pipe loses 60× too little friction.
- The largest concentric rim is kept as the port size. A flange outside diameter can become the bore: 165 mm against 53 mm cuts friction by about (0.149/0.053)^5, or about 170×.
- Object scale is ignored for pipe length and bore.

**M16. The plant report shows the wrong valve state.** `sim.rs:1508`
`let open = valve_open(o, &HashMap::new());`
- Valves without a lever always report 100 % open with the open-valve K, even when shut or throttled.

**M17. Plant numbers rest on unlabelled invented curves.** `sim.rs:683-684`, `:690`
- The default pump (8 m shut-off, 6 m/s run-out) and `0.8 + 3(q/qmax)²` NPSH are not datasheet values. The NPSH curve has no catalog override, yet they drive pass and fail alarms.
- There is no torque, power or energy anywhere. A motor can't stall or be undersized, and no kW value exists.
- Tests are hand calculations and self-consistency only, so nothing here is validated.

## MINOR (CONFIRMED)

- `to_markdown` says "Nothing has been measured yet" above "Measured:" lines when only point claims have checks, and the UI does the same (`claim.rs:236`).
- `calibrate` marks a stall as `converged = true` (`calibrate.rs:209-210`).
- Plant `newton` returns silently after 80 iterations.
- `Fluid::of` silently turns unknown media into water.
- The surge bar conversion assumes water density (`sim.rs:930`). Surge also scales with 1/dt, so it is 5× lower at dt=0.5.
- `step` discards simulated time when more than 5 substeps are needed.
- `seconds` up to 300 is allowed, but errors keep only the last 30 s.
- `worst_coupling_db` is hard-coded to −inf and coexist's `duty` is unused.
- `Swing.share` calls a squared rank correlation a variance share.
- Ledger saves ignore write errors and are not atomic, and `key_of` is 24 bits.
- Seeds are fixed in code but not recorded on claims.
- `implied_er` assumes the field is entirely in the substrate.
- Tee branches have K = 0.4.
- Pumps can't pass forward flow when stopped.

## SUSPECTED (not fully traced)

- **Shelf mesh:** the default of 8 edges/λ gives about 4 edges along the patch, with h = 0.025λ. S11 and field may be unconverged, and there is no mesh-uncertainty factor (`hardware.rs:245`). Tags in the lowest ~4 cm and top 20 % are not sampled, so read rate may be optimistic.
- **UWB frequency sampling:** 5 frequencies means 187 MHz steps. A Q≈30 antenna has about 216 MHz bandwidth, so group-delay ripple is unresolved (`hardware.rs:171`).
- **Drift on a real feed:** `Sample.tag` is documented as the tag's estimated position, but the tests use true positions. With 4 anchors, the solver absorbs about 50 % of a move in 2D and about 75 % in 3D, so moves would be under-reported and smeared across anchors.
- **Yield finite differences:** steps of 1σ on a remeshed geometry may pick up mesh jumps.
- **Sticky plant NaN:** a NaN would persist through `state.q` and `volume` and still give `ok: true`. A catalog `max_flow_lps` of 0 is a possible trigger.
- **Small tanks at dt=0.5:** the explicit level update combined with inertia is near the stability limit (ωdt ≈ 2).

## Checked and sound

**Evidence**
- `Evidence::Measured` is never assigned.
- `Claim::check` upgrades nothing.
- Calibrated has exactly one source, `evidence_now`.
- A single practice walk read back from its file stays labelled Practice, and any synthetic reading taints the whole fit.
- `Evidence::Estimate` stays on tagging, coexist, shelf and material ranges.
- The UI, ledger JSON and markdown all show the label on each claim.

**Statistics**
- The RNG (xoshiro256**, splitmix64, Box-Muller, Fisher-Yates) and Acklam's quantile coefficients match the published versions.
- The Latin hypercube construction is correct, and the quantile function is correct as a point estimate.
- The Levenberg-Marquardt/MAP update matches the conjugate result, and the Huber cost and weights are consistent.
- No MAD estimator exists, so its consistency constant doesn't apply.
- Hold-out parameter values don't leak: the train fit never sees test values.
- Greedy D-optimal planning in `plan.rs` is correct.

**UWB math**
- The sign matches the MoM far field, `exp(+jk r̂·r')`.
- The factor of 2 is correct. Two-way averaging leaves c·τ per antenna, and `sim.rs:491` adds each end once.
- Units are correct.

**Recipe arithmetic**
- The desense formula and the 5.87 dB figure, the kTB noise floor and the range-share formula are correct.
- The Friis forward link and shelf harvest calculation are correct.
- AoA per-port SNR scaling and the `implied_er` algebra are correct.

**Plant**
- Darcy-Weisbach, Swamee-Jain, the exact laminar form and the 2000–4000 blend are correct.
- Pump affinity laws and the rpm, l/s and belt-ratio conversions are correct. G is 9.81.
- Implicit momentum with the Joukowsky cap is stable.
- Tank-to-tank mass balance holds on the closed-loop tests, apart from B13.
- MoM correctly rejects known dielectrics.
- Drift's Gauss-Newton move fit is correct when tag positions are true.
- `plant_simulate`'s 1200 s clamp is disclosed in `simulated_s`.
