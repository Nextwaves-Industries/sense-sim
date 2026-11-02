---
title: Antenna tuning and troubleshooting, from symptom to cause to fix
domain: antenna
tags: troubleshooting, tuning, diagnose, debug, resonance too low, resonance too high, scale dimension, frequency shift, input resistance, reactance, inductive, capacitive, Smith chart, narrow bandwidth, low efficiency, low gain, pattern tilt, squint, beam squint, front to back, F/B, axial ratio, AR, S11 good but gain low, simulation versus measurement, discrepancy, retune, Q factor
aliases: anten sai tần số, cộng hưởng lệch, chỉnh chiều dài anten, trở kháng quá cao, trở kháng quá thấp, điện kháng dương, điện kháng âm, băng thông hẹp, hiệu suất thấp, búp sóng bị lệch, tỷ số trước sau kém, tỷ số trục kém, mô phỏng khác đo đạc, S11 tốt nhưng độ lợi thấp, why is my antenna off frequency, how much should I shorten it, my antenna does not match
verified: 2026-11-02
---
# Antenna tuning and troubleshooting, from symptom to cause to fix

## Symptom table for a quick antenna diagnosis

| Symptom | Most likely cause | First fix |
|---|---|---|
| Resonance too low | Too long, or higher permittivity than modelled | Shorten by the frequency ratio |
| Resonance too high | Too short, or lower permittivity | Lengthen by the frequency ratio |
| Deep dip, wrong R at resonance | Feed position | Move the feed |
| Resonant, offset reactance | Probe or feed inductance | Series capacitor or lumped match |
| Band too narrow | Thin or small antenna, high Q | Thicker substrate, different feed or type |
| Low gain, good S11 | Loss | Check efficiency and material |
| Beam off axis | Asymmetric ground, feed radiation, cable | Make the structure symmetric |
| Weak front to back | Small ground, reflector wrong | Larger ground, tune reflector |
| AR above 3 dB | Wrong mode split or feed position | Tune length difference, then feed |
| Measured differs from simulated | Dk, reference plane, cable, housing | Fit Dk first, then check setup |

Read in this order: a trustworthy simulation, then frequency, resistance, reactance, bandwidth, efficiency and pattern. Resistance is meaningful only near resonance.

## Resonance too low or too high: how much to scale

For a resonant antenna, frequency goes inversely with electrical length. TI's rule: if resonance is too low, shorten the antenna; if too high, lengthen it [1]. Scale by: Lnew = Lold * (fmeasured / ftarget).

Worked example: a dipole of 58 mm resonates at 2.30 GHz, target 2.45 GHz: 58 * 2.30/2.45 = 54.4 mm, about 6% shorter.

Worked example for a patch, where the fringing stays nearly fixed: an FR-4 patch of L = 28.81 mm with dL = 0.74 mm resonates at 2.38 GHz. The effective length 30.29 mm scales to 29.42 mm, so L becomes 27.94 mm, or 0.87 mm shorter. This agrees with the sensitivity of about 8 MHz per 0.1 mm.

| Antenna | Dimension that sets frequency |
|---|---|
| dipole, monopole | Length |
| patch | L, the resonant length [2] |
| ifa_pcb, pifa | Total path from open end to short [3] |

Scale, run and scale again. On a high-Q antenna, confirm mesh convergence before correcting a small shift.

## Input resistance too high or too low

Tune the resistance only after you are close to resonance. The mechanism depends on the type.

| Antenna | R too high | R too low |
|---|---|---|
| Probe or inset patch | Move feed towards centre [2] | Move towards the radiating edge |
| PIFA | Move feed towards the short [3] | Move away from the short [3] |
| Corner reflector | Move dipole closer to the apex [4] | Move further from the apex |
| Patch at edge | Widen patch: width lowers edge impedance [2] | Narrow patch |

Worked example of feed sensitivity, FR-4 2.45 GHz patch with L = 28.81 mm, Redge = 321 ohm, and R = Redge * cos^2(pi*y/L) [6]. R is 80 ohm at y = 9.62 mm from the edge, and 50 ohm at y = 10.69 mm. So moving 1.07 mm towards centre takes 80 to 50 ohm, about 25 to 30 ohm per mm.

A folded dipole multiplies R by about 4, and a sleeve or T-match raises it. A small loop has a very low radiation resistance, 0.003 to 0.5 ohm in the lambda/100 to lambda/30 radius range, and often less than its loss [5].

## Reactance sign and what it means

Reactance is the most informative number in a run, but its meaning depends on the resonance type.

| Type near resonance | Below resonance | Above resonance |
|---|---|---|
| Series type: dipole, monopole | X negative | X positive |
| Parallel type: patch at its feed point [2] | X positive | X negative |

Worked example: an exact half-wave dipole has X = +42.5 ohm and is too long, so shorten it. A patch with X positive at the design frequency is the opposite: the patch is resonating above that frequency, or there is a probe inductance on top.

To tell them apart, look at the trace against frequency. A slope of X upward through zero means series type, and downward means parallel type at resonance (rule of thumb). A positive offset across the whole resonance is feed inductance, such as a long probe [2], rather than a length error. Fix an offset with a series element: at 2.45 GHz, +30 ohm needs 2.17 pF in series, and -30 ohm needs 1.95 nH in series (worked example).

If S11 is poor with X near zero, resistance is wrong. If R is correct and X is not, fix length or add a series part.

## Narrow bandwidth: how to widen it

Bandwidth comes from Q. For a single-tuned antenna, the fractional bandwidth at VSWR 2 is about 0.707/Q (rule of thumb). Worked example: Q = 50 gives 1.4%, and Q = 10 gives 7%.

Moves, approximately in order of cost:

| Move | Effect | Cost |
|---|---|---|
| Increase patch height | Roughly proportional for a thin patch | Surface waves, probe inductance [2] |
| Use wider conductor or flare | Bowtie gets 33% against 8% for a thin wire [7] | Larger size |
| Aperture or proximity feed | About 10 to 13% [2, 8] | Multilayer |
| Stacked patch | 3 to 10% [2] | Multilayer |
| Double-tuned matching | Up to the Bode-Fano limit | Parts, loss |

If the size is fixed, check the Chu limit first; no tuning beats it. Relaxing the criterion from -10 dB to -6 dB widens bandwidth but costs 1.26 dB against 0.46 dB in mismatch loss at the band edge.

## Low efficiency: causes and how large each can be

Radiation efficiency is Rrad / (Rrad + Rloss), so an antenna with low radiation resistance suffers most.

| Cause | Evidence | Fix |
|---|---|---|
| Lossy substrate | 41% on FR-4 for one compact 2.45 GHz design, estimated 86% with a tenfold lower tangent [9] | Low-loss laminate, air or foam |
| Too thin a patch | Efficiency degrades if h is much below 0.025 lambda [10] | Increase height |
| Surface waves | Falls with thickness and permittivity [2, 11] | Lower er, or cavity back [11] |
| Too small for frequency | 7% at 169 MHz against 91% at 868 MHz on the same area [1] | Enlarge antenna and ground [1] |

Typical half-wave patches on good material are well above 90% [2]. Worked example: 41% is -3.9 dB and 86% is -0.66 dB.

Metal loss is usually smaller than dielectric loss on FR-4 patches [9], but small loops are different: loss resistance often exceeds radiation resistance [5].

## S11 looks good but gain is low

A deep S11 says that power enters the antenna, not that it radiates. Work through these checks:

1. Compare peak gain with directivity. The gap is radiation efficiency: 3 dB means half the power is heating something.
2. Check where the peak points. Gain may be fine in a direction you did not read, because the beam moved.
3. Check which polarisation you are reading. Partial gain in the wrong component looks like low gain.
4. Check the port reference. S11 referenced to 73 or 300 ohm is not mismatch to a 50 ohm radio.
5. In a real unit, check the enclosure: metal paint and carbon fibre lower gain without necessarily showing mismatch [12]. Check also the no-ground area against the datasheet [12].

A suspiciously wide, deep match on FR-4 suggests loss. Comparing with a lossless model, which shows a narrower band and higher gain, reveals it.

## Pattern tilt, squint and asymmetry

| Cause | Direction of effect | Fix |
|---|---|---|
| Small or offset ground on a monopole | Peak tilts away from the horizon | Larger or symmetric ground |
| Board ground shape, PCB antenna | Lobes change with ground size and shape [1] | Model the actual board |
| Inset gaps and feed line | Slight pattern distortion [2] | Symmetric layout, short feed |
| Feed transformer radiating | Added cross-polarisation and tilt [2] | Shorter feed, thinner substrate |
| Cable or unbalanced dipole | Tilt, filled nulls [13] | Balun or choke |
| Probe offset on a thick patch | E-plane asymmetry (rule of thumb) | Symmetric feed |
| Array phase error | cos(theta0) = -beta/(k*d) | Fix the feed lengths |

Worked example for an array: at spacing lambda/2 and broadside, a progressive phase error of 10 deg per element steers the beam by asin(10/180) = 3.2 deg. A delay-line feed makes this frequency dependent, which is beam squint: a 5% frequency change with 360 deg of feed phase per element moves phase by 18 deg, about 5.7 deg of squint at lambda/2 spacing.

Check antenna_results pattern cuts first. On a nominally symmetric design, also inspect mesh symmetry, since snapped edges can fake a tilt.

## Poor front to back ratio

A patch front to back ratio depends heavily on ground size and shape [2]. A ground that is too small reduces it; it should extend at least 2 to 3 substrate thicknesses beyond the patch edges [2], and more for good front to back. Microstrip-fed aperture coupling can reduce front to back through the slot, and a stripline feed avoids it [2].

| Antenna | Move | Rough size |
|---|---|---|
| patch_probe | Larger ground, sweep Gx, Gy | Try 0.15 lambda and then 0.25 lambda of margin |
| yagi | Sweep reflector Lr and spacing sr | Lr 3 to 8% above driven (rule of thumb); spacing 0.15 to 0.3 lambda [14] |
| corner_reflector | Larger plates, spacing near lambda/2 [4] | Sides about twice feed-to-vertex distance [15] |
| horn | Back lobe comes from edge diffraction [19] | Treat the aperture edges (rule of thumb) |

Optimising a Yagi for front to back usually gives up gain, so use front_to_back_above with gain_above. A single reading at 180 deg may sit in a null, so quote the worst value in the rear sector.

## Poor axial ratio

Two orthogonal modes need equal amplitude and 90 deg phase. A 3 dB AR is sufficient for most uses, and AR is best at broadside [2]. Diagnoses below are rules of thumb unless cited.

| Reading | Cause | Fix |
|---|---|---|
| Good AR, wrong frequency | Both modes offset | Scale both sides together by the frequency ratio |
| AR never under 3 dB, S11 has one dip | Modes too close | Increase length difference; L = W*(1 + 1/Q) is the start [16] |
| Two distinct S11 dips far apart | Modes too far apart | Reduce difference |
| AR stuck near a constant value | Amplitude imbalance | Move the feed nearer the diagonal [2] |
| Helix AR poor | Too few turns, or C off lambda | Use 3 or more turns [17], C at lambda |

Worked example: a 20 deg phase error alone gives 3.1 dB AR. A 2% side difference on a 29 mm patch is 0.6 mm, so 0.1 mm of etch error is a sixth of the split. Single-feed CP bandwidth of 1 to 3% [2] is small compared with a band like 7% worldwide RFID, which needs a dual-feed or wideband design.

## Simulated and measured disagree

CST's published example improved a patch array's average S-parameter difference by a factor of 10, and the largest error was a non-ideal adapter, not the simulation [18]. Its checklist covers mesh and solver, material values and missing or tolerance-shifted geometry, plus VNA calibration, reference plane and assembly repeatability [18].

| Observation | Likely cause | Check |
|---|---|---|
| Frequency low by 1 to 5% | Dk above model | f goes as 1/sqrt(er) |
| Only Smith trace rotated, magnitude same | Reference plane | Port extension |
| Loss higher than simulated | Laminate Df, solder, adapters | Compare peak S11 depth |
| Readings change when cable moves | Common mode | Choke, tie down [1] |

Worked example: measured 2.38 GHz against 2.45 GHz modelled is -2.9%. If Dk is the only cause, then er_actual = er_model * (2.45/2.38)^2 = 1.06 * er_model, so 4.4 becomes 4.66. Do not take that value as physics until geometry has been measured; it can absorb other errors.

## Applying this in Sense Sim

Run antenna_spec, antenna_run with antenna_wait, then antenna_assess after every change. It returns a verdict (not_trustworthy, not_met, met_mesh_unchecked or met_mesh_checked), plus a diagnosis from impedance, Q and pattern. Fix not_trustworthy before any tuning, using antenna_converge.

Use antenna_calc for arithmetic and antenna_sweep to find direction and rate, then antenna_optimize with sensible bounds and antenna_optimize_status. Use antenna_set to compare substrates, antenna_match for reactance offsets, design_integrate for enclosures and design_fit_measured for a Touchstone comparison.

Template note: the cos^2 model puts the default probe offset yf = 0.25*L on patch_probe near 160 ohm on 2.45 GHz FR-4, but a real FDTD run of that default gave -11.1 dB across 2.44 to 2.46 GHz in this app (2026-11-02), because probe reactance and the finite ground move the result. Judge the feed from antenna_assess, not from this model alone.

## Sources

1. Antenna Selection Guide, AN058 / SWRA161B — Texas Instruments, undated — https://ti.com/lit/swra161
2. The Basics of Patch Antennas, Updated (Orban, Moernaut) — Orban Microwave Products, 2025 — https://www.orbanmicrowave.com/wp-content/uploads/2025/08/The-Basics-Of-Patch-Antennas-Updated.pdf
3. PIFA, Planar Inverted-F Antenna — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/patches/pifa.php
4. Corner Reflector — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/reflectors/cornerReflector.php
5. Lecture 12: Loop Antennas (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L12_Loop.pdf
6. Lecture 21: Microstrip Antennas, Part II (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L21_Mstrip2.pdf
7. Bowtie Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/wideband/bowtie.php
8. Lecture 20: Microstrip Antennas, Part I (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L20_Mstrip1.pdf
9. Lambda/6 Suspended Patch Antenna (L. Giangrande) — arXiv, 2025 — https://arxiv.org/pdf/2504.03567
10. Patch Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/patches/antenna.php
11. Experimental comparison of the radiation efficiency for conventional and cavity backed microstrip antennas — MIT Lincoln Laboratory, 1996 — https://www.ll.mit.edu/r-d/publications/experimental-comparison-radiation-efficiency-conventional-and-cavity-backed
12. Understanding Chip Antennas Handbook — Johanson Technology, 2023 — https://www.johansontechnology.com/docs/4565/johanson-antenna-handbook.pdf
13. Lecture 11: Practical Dipole/Monopole Geometries, Matching (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L11_Match.pdf
14. Yagi-Uda Antenna — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/travelling/yagi.php
15. Reflector antennas lecture notes, corner reflector section — Rohini College of Engineering and Technology, undated — https://rcet.org.in/uploads/academics/rohini_84037743787.pdf
16. Lecture 22: Microstrip Antennas, Part III (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L22_Mstrip3.pdf
17. Helical Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/travelling/helix.php
18. Symbiosis: Measurement and Simulation in Modern Device Design (M. Rütschlin) — CST / ARMMS, 2012 — https://www.armms.org/media/uploads/04_armms_apr12_mruetschlin.pdf
19. Lecture 18: Horn Antennas (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L18_Horns.pdf
