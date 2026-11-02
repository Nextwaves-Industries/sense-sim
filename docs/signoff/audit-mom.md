# Sense Sim MoM and antenna analysis: sign-off audit

**Verdict: not good enough to sign off a real antenna design.** The EFIE, RWG and network core is correct, and I found no formula bug in it. It is usable for pre-tuning thin-wire and metal antennas, where the default mesh puts a thin dipole's resonance about 0.4 % high. The blockers are missing guards and validation, not the core formulation:
- Nothing warns on an invalid mesh or wire radius.
- Convergence is optional, and the study passes too early.
- Efficiency is unqualified.
- Some array and high-Q numbers can be wrong by many dB.

**How the numbers were produced.** No files were written and no cargo was run.
- I ported `wire.rs` and `surface.rs` to in-memory Node. The ports reproduce the README: 82.27+j40.82, −16.57−j31.10, and −4.85 %, −7.85 %, −1.74 %.
- The references are my own exact-kernel Galerkin and Hallén solvers.
- I did not consult King-Middleton tables and did not trust them from memory.
- The Rust tests were not run. I took the assertions from the source and relied on the port's match to the README figures.

## Your red-flag questions

**Half-wave dipole (a = 0.001 λ).** The test is `assert!((70.0..95.0).contains(&z.re) && (30.0..60.0).contains(&z.im), "{z}");` (`crates\antenna_core\src\mom\wire.rs:619`), run at 20 segments/λ.

| Seg/λ | Δ/a | wire.rs port | Exact-kernel Galerkin | Hallén, exact kernel |
|---|---|---|---|---|
| 20 | 50 | 82.27+j40.82 | 82.21+j40.66 | 83.17+j42.57 |
| 40 | 25 | 84.63+j44.10 | 84.55+j43.91 | 84.78+j44.27 |
| 80 | 12.5 | 85.68+j45.60 | 85.57+j45.36 | 85.62+j45.42 |
| 160 | 6.25 | 86.40+j46.54 | 86.21+j46.22 | 86.22+j46.23 |
| 320 | 3.1 | 87.13+j47.21 | 86.75+j46.78 | 86.75+j46.78 |
| 640 | 1.56 | 88.35+j47.47 | 87.26+j47.07 | 87.26+j47.07 |
| 1280 | 0.78 | n/a | 87.75+j47.13 | 87.75+j47.13 |

- **Correct value:** about 87–88 + j47 Ω. R still creeps about 0.5 Ω per halving because of the delta gap.
- **README figure:** about 5 Ω low in R and 6 Ω low in X, which is mesh error.
- **Is the window wide enough to hide errors? Yes.**
  - dX/d(L/λ) is 1919 Ω, so the 30 Ω X window is 3.1 % of L, about ±1.6 % in frequency.
  - That is ±38 MHz at 2.45 GHz, against an 83.5 MHz ISM band.
  - Passing corners give S11 (73 Ω) from −8.2 to −13.2 dB.
- **`t02`** compares only |Z|: `(b.norm() - a.norm()).abs() / a.norm()` (`wire.rs:629`). It reads 3.9 % and passes while X moves 8 %.

**Mutual impedance.** This is not a 33 % solver error. The reference is wrong for this radius.
- −12.5−j29.9 is the a→0 sinusoidal-current result.
- At a = 10⁻⁴ λ the converged value is **−16.85−j31.36 Ω**. Exact and reduced kernels agree to 0.01 Ω.
- As a shrinks (320 seg/λ), Z21 approaches the reference:

| a/λ | Z21 (Ω) |
|---|---|
| 10⁻⁶ | −14.75−j30.73 |
| 10⁻⁸ | −14.03−j30.48 |
| 10⁻¹⁰ | −13.66−j30.35 |
| 1/Ω extrapolation | about −12.2−j29.9 |

- `assert!(err < 0.25, "{z21}")` (`wire.rs:698`) accepts anything within 8.1 Ω of the wrong target.
- At a = 10⁻³ λ the same check gives 25.2 % at 80 seg/λ (−20.41−j31.97), so it would fail there.

**Sphere.** The test accepts ±10 % at both densities, only at ka = 1 and 2 (`crates\antenna_core\src\mom\surface.rs:525-526`). Figures outside the tested range are much worse (see M2).

**Mesh warning and convergence requirement: neither exists.** See B1 and B2. Power balance of 1.00001 is not accuracy evidence either: it holds at 20 seg/λ, where Z is 8 Ω off.

## BLOCKERS (all CONFIRMED)

**B1. Thin-wire kernel collapses with no guard.**
`crates/antenna_core/src/mom/wire.rs:405` `let rr = (dot(d, d) + radius2).sqrt();`
The only radius check is `if !(*r > 0.0)` (`wire.rs:194`). The reduced kernel fails when Δ approaches a. Default mesh (24 seg/λ at f_hi, L = 0.5 λ):

| a/λ | Δ/a | Code | Exact kernel, same mesh |
|---|---|---|---|
| 0.01 | 3.1 | 106.5+j30.6 | 102.3+j30.5 |
| 0.02 | 1.56 | 115.5−j19.7 | 109.3+j3.8 |
| 0.03 | 1.04 | 52.5−j59.8 | 100.3−j23.1 |
| 0.04 | 0.78 | 9.9−j33.3 | 82.1−j39.6 |
| 0.05 | 0.63 | 1.6−j13.8 | 63.8−j45.9 |

- A 2 mm radius rod at 5.8 GHz is a/λ = 0.039, so this is a realistic case.
- Refining makes it worse: a = 0.01 λ at the UI maximum of 100 seg/λ gives 125.2−j11.1, against exact 111.1+j26.0.
- At Δ/a = 0.25, Z is 0.000−j0.139 and the pivot ratio is 6e-2, far above the 4e-14 floor, so LU raises nothing.
- *Fix:* in `WireModel::build`, warn below Δ/a = 4 and refuse below 2. Warn above a/λ of about 0.01. Include bridge and split segments in the check.

**B2. MoM results carry no warnings, and convergence is never required.**
`src/antenna/run.rs:490` `warnings: Vec::new(),`
- The UI allows 6 seg/λ and 4 edges/λ silently (`src\antenna\ui\solve.rs:156`, `:164`).
- At those minimums the dipole resonance is 2.4 % high (59 MHz at 2.45 GHz) and sphere RCS is −41 to −43 %.
- The HTML report (`crates\antenna_core\src\report.rs:120`) omits mesh density, metal and convergence state.
- *Fix:* add `Model::warnings()` for density, thin-wire limits and `MeshQuality`, which is currently used only in tests. Print density, metal, Z0 and "not converged" in the report.

**B3. The convergence study passes too early.**
`src/antenna/mod.rs:443` `if delta < limit {`, with densities `1.35f64.powi(i)` from a fixed base of 12 (`:410`, `:422`).
- For first-order error, the remainder is about ΔS/(1.35−1) = 2.9·ΔS.
- Simulated on the default dipole: pass 2 gives ΔS = 0.0144, so the page says "Converged at density 16". True remaining error is 0.0424.
- S11(f0) reads −19.19 dB against −23.22 dB at 200 seg/λ.
- That density is coarser than the project default of 24, and it is what "Show the finest result" then displays.
- Only S11 of port 1 is checked (`:435`), not gain or S21.
- *Fix:* start at or above the project density and use a ratio of 2 or more, or report ΔS/(r^p−1). Require two consecutive passes and include all S entries and peak gain.

**B4. "Radiation eff. 100.0 %" is shown unqualified on PEC.**
`crates/antenna_core/src/project.rs:195` `metal: "PEC".into(),`
`src/antenna/ui/results.rs:271`
`crates/antenna_core/src/report.rs:143`
- On PEC this number is only a quadrature check, and gain simply equals directivity.
- The only caveat is on the Materials page (`src\antenna\ui\design.rs:448`). The report never names the metal.
- Effect: Rr = 1 Ω with RL = 0.5 Ω is really 67 % (−1.8 dB), and it reports 100 %.
- *Fix:* label it "power balance (PEC)", and show the metal on the results page and in the report.

**B5. "Array gain" is wrong on two counts.**
- `crates/antenna_core/src/array.rs:264-265` `out.p_accepted *= power; out.p_incident *= power;`
- `src/antenna/ui/analysis.rs:131` shows it as "array gain".
- `src/antenna/run.rs:437` sets the grid to `far_step`, default 3°. The UI allows 256×256 elements.

*(a) The grid misses off-grid beams (λ/2 spacing, steered 31°).*

| N | Peak on 3° grid vs true peak |
|---|---|
| 32 | −0.84 dB |
| 64 | −3.60 dB |
| 128 | −24.4 dB |

*(b) Gain is always N·G_el, regardless of spacing (8 z-dipoles, shown 11.18 dBi).*

| Spacing | True directivity | Shown minus directivity |
|---|---|---|
| 0.05 λ | 3.05 dBi | +8.1 dB |
| 0.1 λ | 5.30 dBi | +5.9 dB |
| 0.25 λ | 9.45 dBi | +1.7 dB |
| 0.5 λ | 12.37 dBi | −1.2 dB |

Gain above directivity is physically impossible, which is worse than the README's "ignores coupling".
*Fix:* take the peak from `summarize`'s refined (u, v) times the interpolated element pattern. Resample to a step at or below HPBW/5. Set gain to D·η_element, and flag any spacing below λ/2.

**B6. High-Q S11 comes from under-sampled interpolation, silently.**
`src/antenna/run.rs:444` and `:455`, with `mom_samples: 41`. The doc at `crates/antenna_core/src/sparam.rs:411-412` asks for about 5 samples across the band, but nothing checks it. Series-RLC model, 0.7–1.3 f0:

| Q | Worst error | Interpolated minimum (true match about −66 dB) | −10 dB band |
|---|---|---|---|
| 60 | 5.9 dB | −29.9 dB | |
| 100 | 13.4 dB | −16.5 dB | |
| 200 | 21.7 dB | −6.6 dB | Vanishes (true 0.33 %) |

- `src/outcomes/hardware.rs:523` (outside the stated scope) uses only 5 samples.
- *Fix:* refine around each S11 minimum until a held-out sample agrees, or warn when the step exceeds 2/(5Q).

## MAJOR (CONFIRMED)

**M1. The README table and tests are not validation.**
- `wire.rs:619`, `:629` and `:694-698`, as above.
- `crates/antenna_core/src/mom/wire.rs:676` tolerates 10 % where the README says 0.9 %.
- `crates/antenna_core/src/mom/surface.rs:557` tolerates 25 %.
- `crates/antenna_core/src/mom/template_tests.rs:54` allows PEC efficiency from 50 % to 150 %.
- *Fix:* pin 86.2+j46.2 at 160 seg/λ within 1 Ω, compare complex Z, use −16.85−j31.36 within 0.5 Ω, and limit PEC efficiency to ±3 %. Relabel the README column.

**M2. No curvature rule in meshing, so sphere error is much worse outside the tested range.**
`crates/antenna_core/src/mom/mesher.rs:368` `while longest(n) > self.h && n < 512`.

| ka (12 edges/λ) | Mesh | RCS error |
|---|---|---|
| 0.1, 0.25 | 20-face icosahedron, 39.5 % volume deficit | −63 % (−4.4 dB) |
| 0.5 | 80 faces | −23 % |
| 0.75 | 80 faces | −20 % |

*Fix:* add a sag or h/R limit alongside the wavelength limit.

**M3. Delta-gap feed: mesh-dependent, and the gap is ignored or harmful depending on geometry.**
- The `g` row (`crates\antenna_core\src\templates.rs:111`) has no effect on a continuous wire, because the port snaps to a node (`wire.rs:232-233`).
- If the wires stop at the gap, a bridge is added. g = 0.04 gives 84.0+j42.5, g = 0.0005 gives 103.3+j14.8, and g = 0.0002 gives 26.9−j46.4, at a = 0.001.
- Even with the exact kernel, a = 0.01 moves from 89.2+j28.6 to 117.6+j15.2 as Δ shrinks. A thick wire has no single delta-gap answer.
- *Fix:* warn when g is below 2a, state in the report that no feed parasitics are modelled, and mark `g` as unused by wire MoM.

**M4. Accepted power is wrong for combined drive.**
`crates/antenna_core/src/project.rs:455` `out.p_accepted += ff.p_accepted * w.norm_sqr();`
- The correct form is ‖a‖²−‖Sa‖². With S11 = 0.2, S21 = 0.3 and in-phase drive, true power is 1.5 W against 1.92 W estimated.
- Gain is then 1.07 dB low and a lossless model shows 78 %. Antiphase shows 103 %.
- *Fix:* compute it from S.

**M5. Yield statistics.**
- Failures become 0 dB: `...band_worst(...)).unwrap_or(0.0)` (`src\antenna\mod.rs:478`). The solve error is discarded and the value enters the mean, σ and sensitivities, despite the NaN contract at `crates/antenna_core/src/optimize.rs:698`.
- The verdict is `o.fraction >= 0.9` (`src\antenna\ui\solve.rs:640`). With the default n = 30, 27/30 shows green while the Wilson interval is 74–97 %.
- Tolerances are relative Gaussian only (`mod.rs:467`), though etching error is absolute. Each sample remeshes, so mesh noise enters the spread.
- *Fix:* return NaN and count failures, judge on the lower bound, and add absolute tolerances.

**M6. Cross-plane beam width is a φ span, not an angle.**
`crates/antenna_core/src/farfield.rs:357-360`. The true width is about Δφ·sinθ0, so it reads 2× at θ0 = 30°.
*Fix:* use 2·asin(sinθ0·sin(Δφ/2)).

**M7. Sheet loss is up to 2× high.**
`crates/antenna_core/src/mom/surface.rs:368-374` applies Zs to the net sheet current. A free-standing sheet shares current across two faces, so Zeff = Zs/2. The test at `surface.rs:679-685` assumes Rs/w and allows a ratio of 0.8–3.0, which hides this. Rr = 1 Ω with true RL = 0.5 Ω reads 50 % instead of 67 %. Microstrip is closer to one-sided.
*Fix:* apply Zs/2 to zero-thickness free-standing sheets and document the patch case.

**M8. Negative R silently reports 100 % efficiency.**
- `crates/antenna_core/src/mom/mod.rs:218` `(1.0 - s.norm_sqr()).max(0.0)`
- `crates/antenna_core/src/farfield.rs:166` `... else { 1.0 }`
- `:209-214` falls back so gain equals directivity.
- `crates/antenna_core/src/sparam.rs:117` gives VSWR 1e6.
- `:124` with `db10`'s `v.abs()` turns |S|² = 1.01 into 20 dB mismatch loss.
- The warning at `src/antenna/ui/results.rs:286` is one-sided and blames a "near-field box" that MoM does not have.
- *Fix:* add a passivity check on Re(Z) and warn when |1−η| exceeds 2 % on PEC.

**M9. No conditioning check.**
`crates/antenna_core/src/linalg.rs:55` `let floor = (n as f64 * f64::EPSILON).powi(2) * biggest;`. Junk passes this floor (see B1), and there is no rcond or residual check.
*Fix:* add an rcond estimate and a residual warning, which would also flag interior resonances at run time.

**M10. Matching tiles.**
`src/antenna/ui/analysis.rs:304` computes "power to antenna" from `pick`, while `:306` beside it shows the snapped parts, always green. A snapped S11 of −10 dB omits 0.46 dB. Standard parts carry only constant Q, with no ESL, SRF or pads, and the page does not say so.
*Fix:* compute from `snapped` and state the limit.

## MINOR (CONFIRMED)

- **Array random errors:** one draw with seed 1 and no seed control (`src\antenna\ui\analysis.rs:117-121`). It is not a statistic.
- **Unknown material becomes PEC:** `is_none_or` (`crates\antenna_core\src\geometry.rs:487`) and `wire.rs:206`. Project load does not validate names.
- **Wire geometry:**
  - Mid-leg wire crossings are not joined (`wire.rs:116-118`).
  - A port up to one Δ off the wire is accepted (`wire.rs:250`).
- **Far-field grid:** a non-divisor step such as 7° runs θ to 182° and φ to 357°, and the φ = 90° cut reads 91° (`crates\antenna_core\src\farfield.rs:115-116`).
- **`.ffe` export:** `#Result Type: Gain` above directivity columns (`farfield.rs:435-436`).
- **Touchstone:**
  - Interpolated data is not marked.
  - A non-increasing 2-port frequency truncates silently (`sparam.rs:385-387`).
- **Diversity gain** uses ECC²: 8.66 dB against 7.07 dB at ECC = 0.5 if ECC = |ρ|² (`sparam.rs:670-671`). Literature usage varies.
- **Optimiser:**
  - `Outcome` has no converged or budget flag.
  - F/B and side-lobe goals count as met when `None` (`project.rs:511`, `:513`).
  - `far_index` has no frequency limit (`project.rs:431`).
  - Results are not re-verified on a finer mesh.
- **Mesh staircase:** `ceil(part/delta)` steps X by about 1 Ω (20→24 seg/λ moves 40.82 to 42.02), which makes the objective discontinuous.
- **Report plots** clamp Z to ±2000 Ω and VSWR to 20 (`report.rs:177-181`).
- **Not implemented:** passivity checks, causality checks, mixed-mode S and Touchstone 2.

## SUSPECTED (not fully traced)

- **Default surface mesh on strips:** my RWG port on a *structured* strip mesh, not the real mesher, gives Z(f0) = 66.8−j12.3 at 12 edges/λ against 69.7−j7.3 at 80. Resonance is 1.0221 against 1.0134 f0, about +21 MHz at 2.45 GHz.
- **Sliver triangles** on strips narrower than h are never reported.
- **Low-frequency breakdown:** a dipole's R is stable to kL ≈ 6e-6, so it is not a problem there. Loops were not run. By scaling, I expect R loss below ka of about 1e-3, and there is no loop-star basis.
- **Junctions:**
  - No acute-bend or overlap check.
  - The `0.5(a²+b²)` radius rule at stepped-radius junctions (`wire.rs:483`) is untested.
- **Thin parallel plates:** the near rule is verified only to gap/edge = 0.25.

## Checked and sound

- **EFIE and bases:** signs, triangle and RWG normalisation, N−1 junction bases, ground images, delta-gap RHS and current, and Z = Y⁻¹.
- **Singular integrals:** the wire closed form with grading and Wilton on triangles. Reduced and exact kernels agree within 0.4 Ω at Δ/a ≥ 3 on thin wires. Z21 recovers the induced-EMF limit.
- **Dielectrics:** refused by MoM with a clear message, with no hidden approximation. Zs is the correct good-conductor form, and the wire form Zs/(2πa) is correct.
- **Far field:**
  - θ/φ conventions, RHCP sense and Ludwig-3.
  - Gain, directivity and realized gain definitions.
  - F/B, and P_rad including pole caps and the ground ring.
- **Network code:**
  - from_z, z_at, y_at and renormalisation.
  - Touchstone RI/MA/DB, unit scaling, N11 N21 N12 N22 order, n-port layout and R.
  - Smith chart orientation, group delay, Y-B Q, TARC and active S.
- **Matching:** L, π, T, stub and quarter-wave formulas (each checked against the chain model), series-R finite Q, `efficiency_db` and Bode-Fano.
- **Arrays:** steering sign, AF, grating lobes, Dolph and Taylor tapers, taper efficiency and the directivity integral.
- **Numerics:**
  - LU pivoting, blocking, AVX kernels and NaN rejection.
  - Matrix fill and vertex welding.
  - Nelder-Mead, PSO, DE and the surrogate.
  - Wilson interval, RNG and fixed seeds (7 for yield, 1 for the optimiser).
