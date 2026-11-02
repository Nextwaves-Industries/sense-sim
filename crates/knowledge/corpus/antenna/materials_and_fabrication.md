---
title: Antenna materials, fabrication tolerance and detuning
domain: antenna
tags: FR-4, FR4, 370HR, Rogers, RO4003C, RO4350B, RT/duroid, RT5880, 6002, 6010, PTFE, alumina, Dk, Df, dielectric constant, permittivity, loss tangent, design Dk, process Dk, copper, skin depth, surface resistance, tolerance, etch, resonance shift, radome, plastic housing, enclosure, detuning, near metal, human body, kappa, conductivity
aliases: vật liệu anten, hằng số điện môi, tổn hao điện môi, tang tổn hao, mạch in FR4, dung sai chế tạo, độ sâu da, vỏ nhựa, lệch cộng hưởng, gần kim loại, gần cơ thể người, what Dk should I use, antenna detuned in case, why is measured frequency lower
verified: 2026-11-02
---
# Antenna materials, fabrication tolerance and detuning

## FR-4 permittivity and loss versus frequency

FR-4 is a family of materials, not one specification, and its Dk and Df depend on frequency and resin content. Isola 370HR is one published example, by stripline test [1]:

| Frequency | Dk | Df |
|---|---|---|
| 100 MHz | 4.24 | 0.0150 |
| 1 GHz | 4.17 | 0.0161 |
| 2 GHz | 4.04 | 0.0210 |
| 5 GHz | 3.92 | 0.0250 |
| 10 GHz | 3.92 | 0.0250 |

Generic design values of Dk 4.3-4.6 are commonly used (rule of thumb), but those often describe low-frequency tests of a different laminate construction. Using 4.4 for a 2.45 GHz design on a material near 4.04 is a real risk. Worked example, a patch dimensioned for er 4.4 (1.6 mm, W 37.23 mm, L 28.81 mm, nominally 2.45 GHz) would actually resonate near 2.553 GHz at er 4.04, and near 2.590 GHz at er 3.92. Both are outside the 2.4 GHz ISM band. Ask the fabricator for the actual stack-up data, then retune with a measured sample before relying on FR-4 for a narrow antenna.

## Rogers laminate Dk and Df values

Rogers separates process Dk (a measurement method, used for quality control) from design Dk (recommended for circuit design). Design Dk comes from differential phase length tests at 8-40 GHz, and is an average across several lots and common thicknesses [2].

| Laminate | Process Dk (10 GHz) | Design Dk | Df | Notes |
|---|---|---|---|---|
| RO4003C | 3.38 +/-0.05 | 3.55 | 0.0027 at 10 GHz, 0.0021 at 2.5 GHz | TCDk +40 ppm/C, 0.71 W/m/K [2] |
| RO4350B | 3.48 +/-0.05 | 3.66 | 0.0037 at 10 GHz, 0.0031 at 2.5 GHz | TCDk +50 ppm/C, 0.69 W/m/K [2] |
| RT/duroid 5880 | 2.20 +/-0.02 | 2.20 | 0.0009 at 10 GHz | [3] |
| RT/duroid 6002 | 2.94 +/-0.04 | 2.94 | 0.0012 at 10 GHz | TCDk +12 ppm/C [4] |
| RT/duroid 6010.2LM | 10.20 +/-0.25 | 10.7 | 0.0023 at 10 GHz | [5] |

Worked example of why this matters: a RO4003C patch sized with er 3.38 but behaving at er 3.55 shifts from 2.45 to 2.393 GHz (-57 MHz), and from 6.5 to 6.351 GHz (-149 MHz), which exceeds its estimated bandwidth. Design Dk is measured at 8-40 GHz, so use it as a starting point and verify with a measured prototype, especially at lower frequencies.

## Other substrate reference values

Lecture table values for common antenna substrates [6]:

| Material | Dk | Loss tangent |
|---|---|---|
| Unreinforced PTFE (Cuflon) | 2.1 | 0.0004 |
| RT/duroid 5880 | 2.20 | 0.0009 |
| Fused quartz | 3.78 | 0.0001 |
| 96% alumina | 9.40 | 0.0010 |
| 99.5% alumina | 9.80 | 0.0001 |
| Semi-insulating GaAs | 12.9 | 0.0020 |

Most patches use er from 2.2 to 12, and substrate height from 0.003 to 0.05 lambda0 [6]. Lower permittivity and a thicker substrate give better efficiency and more bandwidth, and higher permittivity gives a smaller size [6]. Substrate selection criteria also include dispersion of Dk and Df, and anisotropy [6].

Decision guidance: use low-loss laminate if the patch Q is high, as a thin high-Q antenna stores most of its energy in the substrate so Df matters directly. For a wideband thick antenna, or a small low-cost printed monopole where little field is in the board, FR-4 can be acceptable. Foam or air under a metal patch gives the lowest loss.

## Copper skin depth and surface resistance

Metal loss depends on surface resistance: Rs = sqrt(pi * f * mu0 / sigma), and skin depth is delta = 1 / sqrt(pi * f * mu0 * sigma). Worked example for copper at sigma = 5.8e7 S/m:

| Frequency | Skin depth | Rs |
|---|---|---|
| 915 MHz | 2.18 um | 7.89 milliohm |
| 2.45 GHz | 1.34 um | 12.91 milliohm |
| 6.5 GHz | 0.82 um | 21.03 milliohm |

Standard 1 oz copper is about 35 um thick (rule of thumb), well over 10 skin depths at these frequencies, so extra copper weight does little for RF loss. Finish and roughness matter more than thickness: current flows near the surface, and Rogers offers low-profile foil specifically for reduced insertion loss [2]. Lower-conductivity metals, printed inks and surface plating raise Rs, which matters most for high-Q antennas. For very thin tag conductors, also check layer thickness against skin depth. A perfect conductor assumption gives an optimistic efficiency, so use real metal when efficiency is a deliverable.

## Tolerance effects on patch resonance

Patch frequency depends mainly on length and permittivity, and approximately df/f = -(1/2) * d(er)/er. Worked example from the Hammerstad model:

| Change | Nominal | New frequency | Shift |
|---|---|---|---|
| FR-4 L + 0.1 mm | 2.450 GHz | 2.4419 GHz | -8.1 MHz |
| FR-4 h 1.5 mm rather than 1.6 | 2.450 GHz | 2.4529 GHz | +2.9 MHz |
| FR-4 h 1.7 mm rather than 1.6 | 2.450 GHz | 2.4470 GHz | -3.0 MHz |
| RO4003C er 3.50 (-0.05) | 2.450 GHz | 2.4667 GHz | +16.7 MHz |
| RO4003C er 3.60 (+0.05) | 2.450 GHz | 2.4336 GHz | -16.4 MHz |

Compare the shift with bandwidth: the estimated bandwidth of a 2.45 GHz 1.5 mm patch is under 30 MHz, so the Dk tolerance of even a controlled laminate can consume half of it or more. Practical design rules: centre the antenna in the band with margin, run corner cases (min and max Dk, etch, and thickness) in a sweep or yield run, and include a footprint for a matching network near the feed. TI recommends such a network and notes that large bandwidth reduces sensitivity to detuning by encapsulation or nearby objects [7].

## Loss tangent to FDTD conductivity

An FDTD solver models dielectric loss with conductivity. The openEMS patch tutorial uses kappa = tan_d * 2 * pi * f0 * eps0 * er, so the loss matches at one chosen frequency [8]. A conductivity grows less realistic away from that frequency, since loss from a fixed conductivity falls off as 1/f in terms of tan_d.

Worked example: FR-4 with er 4.4 and tan_d 0.02 at 2.45 GHz gives kappa = 0.012 S/m. To apply, calculate kappa at the design frequency, not the frequency the datasheet reports. For a 6.5 GHz FR-4 design, take er around 3.92 and Df around 0.025 from a 5-10 GHz measurement rather than a nominal 2 GHz figure [1].

A lossless substrate (kappa = 0) gives a resonance close to the right one, but it overstates gain and efficiency, and it can give misleadingly narrow bandwidth and long ring-down. On FR-4 this can hide a large loss. Always use a real loss value when efficiency or gain is being reported for a design on lossy material.

## Plastic housing and radome detuning

Plastic near an antenna adds permittivity, so resonance moves downward. Evidence is mixed and setup-specific, so treat any figure as indicative.

| Source type | Finding |
|---|---|
| TI inverted-F report | A large bandwidth reduces sensitivity to plastic encapsulation or nearby objects [7] |
| TI inverted-F report | Housing, batteries and mechanical parts alter impedance, so add a pi network [7] |
| Practical 2.4 GHz A/B enclosure study (hobby-level, method not fully specified) | Over 20-30 MHz shift with antenna-wall clearance under 3 mm; 5-8 mm is a starter target; 0.8-1.0 mm walls close to free space, 1.4-1.8 mm walls strong detuning [9] |

Guidance: narrower antennas suffer most; on a 2.45 GHz patch with about 27 MHz bandwidth, a 20-30 MHz shift is essentially the whole band. Model the real wall in simulation with an actual Dk and thickness and evaluate the antenna in its enclosure. Keep walls thin and uniform near the antenna, avoid paint with metal flake, and retune with the housing in place. ABS and polycarbonate Dk values depend on grade and frequency, so measure them or get datasheet data for important designs.

## Detuning near metal

Metal nearby acts as a mirror, changes current distribution, and may short the radiating fields. TI notes that if a ground plane is too close to a dipole the antenna is effectively shorted by the reflected inverse wave, and a further-away ground still reflects RF power back to the antenna [10]. A horizontal wire just above a conductor couples strongly with its out-of-phase image, so radiation resistance falls toward zero and a resonant dipole is detuned [11].

Clearance figures vary by antenna. Taoglas specifies at least 3 mm, ideally 5 mm, from metal components for its UWC.01 ceramic UWB antenna, along with a minimum 25 x 13 mm ground plane, and a fixed 0.3 mm antenna-to-ground clearance [12]. That figure is specific to that part and frequency and should not be applied to a lower-frequency antenna without scaling or simulation.

Deployment rules: mount anchors off metal walls and rails where possible, because reflections affect pattern and first-path; use an antenna designed with its own ground (patch, PIFA) if metal is unavoidable; and characterise the mounting. Quarter wavelength spacing is 82 mm at 915 MHz, 31 mm at 2.45 GHz and 11.5 mm at 6.5 GHz.

## Detuning and loss near the human body

Human tissue has a high permittivity and high loss, so it both detunes and absorbs. Published examples vary widely with antenna type and spacing, and body-worn figures should not be taken as universal. An antenna-theory.com measurement showed about 10 dB efficiency loss when antennas sat 2.7 mm above a body-simulating lossy block across 1-6 GHz [13]. The same article illustrates how a -3 dB (50%) antenna may drop to -13 dB (5%) on the body [13]. S11 can look better near the body because of the added loss, not because of a better match [13].

Implications for tags and wearables: a good S11 near the body is not evidence of a good antenna. Measure or simulate efficiency, not match. Include body shadowing in positioning budgets, because a body blocks a line of sight regardless of antenna tuning. Antennas with their own ground plane (patch, PIFA, or an artificial magnetic conductor) reduce sensitivity to the body, and a larger stand-off helps. For UWB ranging, a body also delays or blocks the first path, an error that antenna tuning cannot fix.

## Applying this in Sense Sim

The Studio material library contains conductors, including copper and aluminium, and dielectrics including FR-4 (4.4, 0.02), RO4003C (3.38, 0.0027), RO4350B (3.48, 0.0037), RT/duroid 5880, 6002 and 6010, foam, ABS and polycarbonate. Library loss is treated as a tangent, and the solver converts it to conductivity for one frequency. These laminate values are process or nominal values. For resonant designs, change the laminate value to the design Dk, or override with measured data, and state which you used. On a real FR-4 stack, ask for measured Dk at your frequency before trusting the frequency of a narrow design.

Run the sensitivity directly: sweep er, L and h between plausible limits, and check that the band stays within -10 dB at all corners. Add an extra solid for a plastic wall to model the housing. Use a real metal for efficiency estimates, because PEC reports an optimistic result. The RTLS FDTD patch models the substrate without loss, so its gain and efficiency are optimistic on FR-4.

## Sources

1. 370HR Laminate and Prepreg Data Sheet — Isola Group, undated — https://www.isola-group.com/wp-content/uploads/data-sheets/370hr.pdf
2. RO4000 Series High Frequency Circuit Materials Data Sheet — Rogers Corporation, undated — https://www.rogerscorp.com/-/media/project/rogerscorp/documents/advanced-electronics-solutions/english/data-sheets/ro4000-laminates-ro4003c-and-ro4350b---data-sheet.pdf
3. RT/duroid 5870/5880 Data Sheet — Rogers Corporation, 2014 — https://a-contract.ru/fileadmin/datasheets/base-material/RT5880-Duroid.pdf
4. RT/duroid 6002 Laminate Data Sheet — Rogers Corporation, undated — https://www.pcbpower.com/static/base/images/resources/RT-duroid%206002%20Laminate%20Data%20Sheet.pdf
5. RT/duroid 6006 and 6010.2LM Laminates — Rogers Corporation, accessed 2026 — https://rogerscorp.com/advanced-electronics-solutions/rt-duroid-laminates/rt-duroid-6006-and-6010-2lm-laminates
6. Lecture 20: Microstrip Antennas, Part I (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L20_Mstrip1.pdf
7. 2.4-GHz Inverted F Antenna, SWRU120D — Texas Instruments, 2019 — https://ti.com/lit/swru120
8. Tutorial: Simple Patch Antenna — openEMS wiki, 2017 — https://wiki.openems.de/index.php/Tutorial:_Simple_Patch_Antenna.html
9. Co-Design of PCB Antennas and 3D-Printed Enclosures — AISLER Community, undated — https://community.aisler.net/t/co-design-of-pcb-antennas-and-3d-printed-enclosures-practical-rf-insights/5518
10. Bluetooth Angle of Arrival (AoA) Antenna Design, TIDA029A — Texas Instruments, 2023 — https://ti.com/lit/tida029
11. Lecture 9: Linear Wire Antennas, Dipoles and Monopoles (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L09_Dipole.pdf
12. UWC.01 Accura UWB Ceramic Antenna Datasheet — Taoglas, undated — https://www.taoglas.com/datasheets/UWC.01.pdf
13. Wearable Antennas — antenna-theory.com, undated — https://antenna-theory.com/antennas/wearable-antennas.php
