---
title: Transmission lines, reflection and S-parameters
domain: rf
tags: transmission line, characteristic impedance, Z0, reflection coefficient, Gamma, return loss, RL, VSWR, SWR, mismatch loss, reflected power, S-parameters, S11, S21, S12, S22, scattering parameters, insertion loss, ABCD, Touchstone, s1p, s2p, snp, Smith chart, group delay, phase delay, electrical length, velocity factor, line loss, VNA, network analyzer
aliases: đường truyền, trở kháng đặc tính, hệ số phản xạ, suy hao phản hồi, tổn hao phản xạ, tỷ số sóng đứng, sóng đứng, tham số S, tham số tán xạ, đồ thị Smith, giản đồ Smith, độ trễ nhóm, chiều dài điện, suy hao đường dây, suy hao chèn, công suất phản xạ, máy phân tích mạng, file s2p, how much power is reflected, what does S11 mean
verified: 2026-11-02
---
# Transmission lines, reflection and S-parameters

## Characteristic impedance of a transmission line

A line carries a wave whose voltage-to-current ratio is its characteristic impedance Z0. For a low-loss line Z0 = sqrt(L/C) and delay per length = sqrt(L x C), where L and C are per-unit-length values.

Worked check against a real cable: Belden 7805 (RG-174 type) lists 0.077 uH/ft and 31.2 pF/ft, with 50 ohm and 1.54 ns/ft nominal [1]. The formulas give 49.7 ohm and 1.55 ns/ft (computed), so the data sheet is self-consistent.

Z0 is not a resistance you can measure with a meter. It sets how the line transforms a load. It depends only on geometry and materials, not on length. A 50 ohm line ending in 50 ohm looks like 50 ohm at any length. Ending in anything else, it reflects.

## Reflection coefficient, return loss and VSWR formulas

Reflection coefficient at a load: Gamma = (ZL - Z0) / (ZL + Z0), and in reverse ZL = Z0 (1 + Gamma) / (1 - Gamma) [2]. VSWR = (1 + |Gamma|) / (1 - |Gamma|), and |Gamma| = (VSWR - 1) / (VSWR + 1) [2].

Return loss in dB = -20 log10|Gamma|, always positive; S11 in dB is the same number with a negative sign [3]. So "S11 = -10 dB" and "return loss = 10 dB" mean the same thing. Reflected power fraction = |Gamma|^2. Mismatch loss in dB = -10 log10(1 - |Gamma|^2) (from power conservation).

Worked examples, Z0 = 50 ohm (computed):

| Load | Gamma | Return loss | VSWR |
|---|---|---|---|
| 75 ohm | 0.2 at 0 deg | 14.0 dB | 1.5 |
| 30 + j40 ohm | 0.5 at 90 deg | 6.0 dB | 3.0 |

Going back from a measurement: S11 = 0.5 at -60 deg gives Z = 50 - j57.7 ohm (computed). Note that the same |Gamma| can come from many impedances, so VSWR alone does not tell you which way to match.

## Conversion table: return loss, VSWR, mismatch loss, reflected power

All values computed from the formulas in the previous section.

| Return loss (dB) | abs Gamma | VSWR | Mismatch loss (dB) | Reflected power |
|---|---|---|---|---|
| 3 | 0.708 | 5.85 | 3.02 | 50.1% |
| 6 | 0.501 | 3.01 | 1.26 | 25.1% |
| 9.54 | 0.333 | 2.00 | 0.51 | 11.1% |
| 10 | 0.316 | 1.92 | 0.46 | 10.0% |
| 12 | 0.251 | 1.67 | 0.28 | 6.3% |
| 14 | 0.200 | 1.50 | 0.18 | 4.0% |
| 15.6 | 0.167 | 1.40 | 0.12 | 2.8% |
| 20 | 0.100 | 1.22 | 0.04 | 1.0% |
| 25 | 0.056 | 1.12 | 0.01 | 0.3% |
| 30 | 0.032 | 1.07 | 0.004 | 0.1% |

Reading it: improving a match from 10 dB to 20 dB return loss recovers only 0.42 dB of delivered power. Past about 10 dB a mismatch rarely limits range by itself. It matters more because it detunes filters and amplifiers, stresses a power amplifier, and warns that the antenna may be sitting near its band edge.

## Input impedance and electrical length of a line

A lossless line of length l transforms a load: Zin = Z0 (ZL + j Z0 tan(beta l)) / (Z0 + j ZL tan(beta l)) [2]. Zin repeats every half guided wavelength [2]. A quarter wave inverts: Zin = Z0^2 / ZL.

Electrical length in degrees = 360 x l / lambda_g, with lambda_g = c / (f sqrt(eeff)) = c x VF / f, where VF is the velocity factor.

Worked values (computed):
- LMR-400 has 85% velocity and 3.94 ns/m [4]. At 2.45 GHz, lambda_g = 104 mm, so a quarter wave is 26 mm and one metre is 3475 deg.
- At 915 MHz, one metre of the same cable is 1298 deg.
- Microstrip with eeff = 3 at 2.45 GHz runs about 5.1 deg per mm.

A trace or cable matters as a line once it exceeds about lambda_g / 10 (rule of thumb). Below that it acts as a small series L and shunt C. This is why moving a VNA reference plane by a few millimetres visibly rotates a Smith chart trace at 6.5 GHz, and is why a phase-matched antenna array needs equal cable lengths.

## Reading values off a Smith chart

The Smith chart plots Gamma in polar form, with impedance grids drawn on top. Distance from the centre is |Gamma|, so a circle around the centre is one constant VSWR and return loss. Normalise first: z = Z / Z0.

Reading a point:
- Centre: matched. Right end of the axis: open. Left end: short.
- Upper half: inductive. Lower half: capacitive.
- On the horizontal axis: purely resistive, so it is at resonance.

The angle of Gamma falls by twice the electrical length when moving toward the source, so half a wavelength is a full turn [2]. Therefore a measurement that seems to need matching may only need the reference plane moved.

Worked example from a tutorial: load z = 1 - j2 at 1 GHz on 50 ohm sits on the r = 1 circle. A 15.9 nH series inductor moves it clockwise along that circle to the centre, which is a perfect match [5]. A point not on the r = 1 or g = 1 circle needs two elements. The matching document in the antenna domain tabulates the moves for series and shunt parts.

## What each S-parameter means

S-parameters are ratios of outgoing to incoming waves with every other port terminated in its reference impedance. The first index is the port where the wave leaves and the second where it enters, so S21 is output at port 2 for a drive into port 1 [3].

| Term | Meaning | Passive part in dB |
|---|---|---|
| S11, S22 | Reflection at port 1 or 2 | Negative; return loss is its magnitude |
| S21 | Forward transmission | Insertion loss is its magnitude [3] |
| S12 | Reverse transmission | Equals S21 if reciprocal |
| Sij between antennas | Coupling | Isolation is its magnitude |

They are complex: magnitude and phase. Phase carries the delay and the impedance information, so keep it. For a lossless two-port, |S11|^2 + |S21|^2 = 1. If the sum is below 1 the rest is dissipated: 0.5 dB of S21 with a 20 dB return loss means about 10% lost as heat (computed).

S-parameters only hold for the reference impedance they were measured in, typically 50 ohm, and for a linear, small-signal device.

## Converting S-parameters to impedance and ABCD

One port: Z = Z0 (1 + S11) / (1 - S11) [2]. To get admittance, invert Z.

Two ports with the same Z0 on both, from Steer [6]:
- A = ((1 + S11)(1 - S22) + S12 S21) / (2 S21)
- B = Z0 ((1 + S11)(1 + S22) - S12 S21) / (2 S21)
- C = (1/Z0) ((1 - S11)(1 - S22) - S12 S21) / (2 S21)
- D = ((1 - S11)(1 + S22) + S12 S21) / (2 S21)

And back, with Delta = A + B/Z0 + C Z0 + D [6]:
- S11 = (A + B/Z0 - C Z0 - D) / Delta
- S21 = 2 / Delta
- S12 = 2 (AD - BC) / Delta
- S22 = (-A + B/Z0 - C Z0 + D) / Delta

Use ABCD to cascade: multiply the matrices in order, then convert back. S matrices cannot be multiplied directly. A reciprocal network has AD - BC = 1, which makes S12 equal S21 (follows from the equations above). The ABCD forms need S21 not equal to zero, so they fail for a fully blocked path, for example a port with an open switch.

## Touchstone files: s1p and s2p format

Touchstone is the plain-text exchange format for network data. The option line is # [Hz|kHz|MHz|GHz] [S|Y|Z|G|H] [DB|MA|RI] [R n] [7]. Missing values default to GHz, S, MA and 50 ohm [7]. Comments start with ! [7]. Extensions are .snp for n ports, for example .s2p, with .ts suggested for version 2.0 [7].

Traps:
- Defaults apply silently, so a file with a bare "#" line is read as GHz, S, MA and 50 ohm. Check the option line before trusting a plot.
- DB means 20 log10 magnitude, and angles are always in degrees [7].
- Two-port data has a special column order: N11, N21, N12, N22 in version 1.0 [7]. Three or more ports use row order, so S21 and S12 trap anyone reading columns by eye.
- Version 2.0 adds [Version], [Number of Ports], [Reference] for per-port impedance and [Two-Port Data Order], where the argument 12_21 or 21_12 states the order [7].

The minimal check on an imported file: find S11 at a known point, confirm the magnitude is at most 1 for a passive device, and confirm S21 equals S12.

## Group delay and how to measure it

Group delay is the transit time through a device as a function of frequency [8], defined as the negative slope of phase against frequency: tau = -(1/360) x d(phase in deg) / d(f in Hz) [11]. Worked example: 36 deg of phase falling across 10 MHz is 10 ns (computed).

A VNA computes it by finite difference over an aperture. A narrow aperture shows fine detail but is noisy, and a wide one is smoother but averages out variation, so always record the aperture when comparing results [8]. Keep phase steps between adjacent points under 180 deg, or the delay comes out wrong [8].

Why it matters: flat group delay keeps a pulse shape, and variation distorts it. The TA1102A 915 MHz SAW filter lists group delay variation of 35 ns typical and 50 ns maximum across 902-928 MHz [9]. For ranging, 1 ns of delay is 0.3 m (computed), so UWB chains need calibrated antenna delay, and any change of cable or filter after calibration shifts every range.

## Line loss and its effect on measurements

Coax and microstrip loss has two parts. Conductor loss grows with sqrt(f), and dielectric loss grows with f. Times Microwave writes it as attenuation per 100 ft = k1 sqrt(F_MHz) + k2 F_MHz [10]. See the cables document for values by cable.

A lossy line flatters the match. A reflected wave crosses the line twice, so measured return loss = load return loss + 2 x line loss (one-way loss in dB).

Worked example: 3 m of LMR-195 at 2.45 GHz is about 1.85 dB (computed from [10]). An antenna with true 10 dB return loss then reads 13.7 dB at the far end of the cable, while 1.85 dB of power never reaches it. A very long cable makes any antenna, even a broken one, look well matched.

To avoid this: calibrate the VNA at the point of interest, not at the instrument, and calculate delivered power from both the mismatch loss and the line loss. For a total, add them in dB.

## Applying this in Sense Sim

Call antenna_results after antenna_run: each port reports s_db, z_re, z_im and vswr at the design frequency, resonances, and bands_below_10db. Read z_im and the Smith position before changing anything, since a reactive load is often fixed by antenna_match with no new geometry, while a resistive mismatch may need antenna_set to change the feed or dimensions. Set the pass limit with antenna_spec (s11_db defaults to -10) and let antenna_assess judge it.

Use antenna_calc with formula "match" and s11_db, vswr or return_loss_db to convert values, and "wavelength" with f and er for guided length. design_coexist accepts a touchstone file with antenna coupling; port order must equal radio order.

The parser in `crates/antenna_core/src/sparam.rs` follows version 1 defaults and two-port order, but rejects any Touchstone 2 file with a keyword line. Ask the user to export version 1 from the VNA. All antenna results assume the port reference of the run, normally 50 ohm.

## Sources

1. Belden 7805, RF 100 Wireless Coax, RG174 — Belden, accessed 2026 — https://media.onlinecomponents.com/productfiles/mf-bl1/7805_techdata.pdf
2. The Lossless Terminated Line, Fundamentals of Microwave and RF Design — M. Steer, NC State, via LibreTexts, 2022 — https://eng.libretexts.org/Bookshelves/Electrical_Engineering/Electronics/Fundamentals_of_Microwave_and_RF_Design_(Steer)/03%3A_Transmission_Lines/3.03%3A_The_Lossless_Terminated_Line
3. Six ways not to be confused by S-parameters — Teledyne LeCroy, 2020 — https://blog.teledynelecroy.com/2020/04/six-ways-not-to-be-confused-by-s.html
4. LMR-400-BULK data sheet — Fairview Microwave, 2023 — https://fairviewmicrowave.com/images/productPDF/LMR-400-BULK.pdf
5. Impedance matching basics: series L and C — antenna-theory.com, accessed 2026 — https://antenna-theory.com/tutorial/smith/smithchart5.php
6. Scattering Parameter Two-Port Relationships, Microwave and RF Design III — M. Steer, via LibreTexts, accessed 2026 — https://eng.libretexts.org/Bookshelves/Electrical_Engineering/Electronics/Microwave_and_RF_Design_III_-_Networks_(Steer)/02%3A_Chapter_2/2.7%3A_Scattering_Parameter_Two-Port_Relationships
7. Touchstone File Format Specification, Version 2.0 — IBIS Open Forum, 2009 — https://www.ibis.org/touchstone_ver2.0/touchstone_ver2_0.pdf
8. Group Delay Format — Keysight FieldFox help, accessed 2026 — https://helpfiles.keysight.com/csg/FieldFoxOnlineSupplementalHelp/Concepts/Group_Delay_Format.htm
9. TA1102A SAW Filter 915 MHz — TAI-SAW Technology, Rev. 2 — https://www.rfmw.com/datasheets/taisaw/TA1102A%20_Rev.2.0_.pdf
10. Communications Coax Selection Guide — Times Microwave Systems, accessed 2026 — https://www.rfparts.com/old_site/pdf_docs/LMR/Coax_Selection_Guide.pdf
11. Group and Phase Delay Measurements, application note 1EZ35 — Rohde & Schwarz, accessed 2026 — https://scdn.rohde-schwarz.com/ur/pws/dl_downloads/dl_application/application_notes/1ez35/1ez35_1e.pdf
