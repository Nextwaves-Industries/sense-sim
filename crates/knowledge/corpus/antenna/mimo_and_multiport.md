---
title: MIMO and multiport antennas, isolation, ECC, TARC and decoupling
domain: antenna
tags: MIMO, multiport, multi-port, isolation, mutual coupling, S21, envelope correlation coefficient, ECC, diversity gain, DG, TARC, total active reflection coefficient, active VSWR, active reflection, decoupling, neutralization line, defected ground, DGS, EBG, metasurface, parasitic element, decoupling network, polarisation diversity, pattern diversity, element spacing, chassis mode, spectral regrowth, ACPR, mimo_patch
aliases: anten đa cổng, anten MIMO, độ cách ly giữa các cổng, ghép tương hỗ, hệ số tương quan đường bao, độ lợi phân tập, hệ số phản xạ tích cực toàn phần, giảm ghép tương hỗ, khoảng cách phần tử MIMO, how much isolation do I need, what is a good ECC, how to decouple two antennas, how close can two antennas be
verified: 2026-11-02
---
# MIMO and multiport antennas, isolation, ECC, TARC and decoupling

## Isolation targets for MIMO antennas: what is really needed

Isolation is -20*log10(|Smn|) between two ports. A common industry rule of thumb of -30 dB for base-station arrays is stricter than link performance alone requires, according to Chen and co-authors [1].

| Metric | Finding [1] |
|---|---|
| MIMO capacity | Coupling at -17, -22 and -27 dB gave about the same capacity in their example |
| 256-QAM symbol errors | Going from -6 to -12 dB gains about 3 dB; below -15 dB, little further gain |
| General conclusion | Coupling below about -10 dB has negligible effect on capacity or error rate in these studies |
| Industry rule for large base stations | Below -30 dB |
| Active VSWR at 15 dB isolation | Up to about 6 in a multi-port example, reducing to 2 at 20 dB |

Why stricter targets persist: coupling also pulls power amplifiers. Reported spectral regrowth at table separations of 0.36, 0.44 and 0.59 (unit not stated, presumably wavelengths) was -46.4, -52.3 and -57.4 dBc, at coupling of -14.0, -20.9 and -28.4 dB [1]. So set the target by the actual concern: about 10 to 15 dB for a small receive-dominant terminal, 15 to 20 dB for general design (rule of thumb), and much more for a many-port transmit array. In compact terminals below 1 GHz, isolation under 6 dB is typical because the antennas share the chassis as a radiator [1].

## Envelope correlation coefficient: definition and targets

ECC measures how independent two antennas' radiation patterns are, considering pattern shape, polarisation and relative field phase. A value of 1 means identical patterns and 0 means independent [2]. Orthogonal polarisation or opposite radiation directions drive it towards 0 [2].

Common thresholds: 0.5 is acceptable, above that is poor, and 0.3 or lower is considered good for MIMO [2]. Chen and co-authors note that even 0.3 can noticeably degrade capacity with more than three elements [1].

The usual reporting formula for diversity gain is DG = 10 * sqrt(1 - ECC^2) dB, with an ideal 10 dB [3].

| ECC | DG (worked example) |
|---|---|
| 0.5 | 8.66 dB |
| 0.3 | 9.54 dB |
| 0.1 | 9.95 dB |
| 0.01 | 10.00 dB |

This formula is a convention that hides efficiency. A rigorous diversity gain at the 1% CDF level improves with both lower correlation and higher embedded radiation efficiency [1]. A pair at 30% efficiency with ECC 0.01 reports a perfect-looking DG, yet it has a poor link. Always quote efficiency alongside ECC.

## ECC from S-parameters and its limits

For two ports, the S-parameter form is:

ECC = |S11* S12 + S21* S22|^2 / ((1 - |S11|^2 - |S21|^2) * (1 - |S22|^2 - |S12|^2)) [3]

It was derived for lossless, single-mode antennas [4]. Worked example: |S11| = |S22| = 0.316 (-10 dB) and |S21| = 0.178 (-15 dB), worst-case phases: numerator (2*0.316*0.178)^2 = 0.0127, denominator 0.868^2 = 0.754, ECC = 0.017. It is very small, even for a mediocre pair.

Limits of this shortcut:

- It is not valid for lossy antennas without correction, and the corrected forms still have problems [4].
- ECC depends on all currents on the antenna, whereas S-parameters see only the ports [4].
- Antenna-theory.com limits the shortcut to antennas above 90% efficiency [2].

So a very small S-parameter ECC on FR-4 proves little. Compute ECC from the far-field patterns, with magnitude and phase in both polarisations, and take the S-parameter value as a quick check. Antenna-theory.com also argues that isolation is the simpler design target, since reciprocity links high correlation to tight coupling [2].

## TARC: total active reflection coefficient

Mutual coupling means that a port's reflection depends on what the other ports are doing. TARC captures this in one number: the square root of total outgoing power divided by the square root of total incident power, with b = S*a [5]. It runs from 0, all power accepted, to 1, all power returned [5].

Worked form for two ports, derived from this definition, with equal amplitude and relative phase theta:

TARC = sqrt(|S11 + S12*exp(j*theta)|^2 + |S21 + S22*exp(j*theta)|^2) / sqrt(2)

Worked example: S11 = S22 = 0.316, S21 = S12 = 0.178, both real. At theta = 0, TARC = 0.494, or -6.1 dB. At theta = 180 deg, TARC = 0.138, or -17.2 dB. A single S11 of -10 dB does not predict either of them. Always sweep theta and quote the worst value in band.

Loss spoils the interpretation. With losses, total efficiency is 1 - TARC^2 only when radiated power is computed properly, and a low TARC does not prove that power was radiated [6]. Capek and co-authors give an example with a radiation efficiency bound of 0.96, which keeps minimum TARC at about 0.199 even under a perfect match [6]. Lossy antennas may look better on TARC than they are.

## Active impedance in multiport arrays

Each port's active reflection adds the coupled waves: Gamma_active,m = Sum over n of Smn * an / am. In the worst phase, this is approximately |Smm| + the sum of |Smn|.

Worked example, equal amplitudes:

| Case | S11 | Each Smn | Worst active reflection |
|---|---|---|---|
| 2 ports | -15 dB | -15 dB | 0.356, -9.0 dB |
| 2 ports | -15 dB | -20 dB | 0.278, -11.1 dB |
| 4 ports, equal neighbours | -15 dB | -20 dB | 0.478, -6.4 dB |
| 8 ports, 7 equal terms | -15 dB | -20 dB | 0.878, -1.1 dB |

Real arrays have coupling falling with distance, so an all-equal value is pessimistic. The trend remains, and this is what drives the -30 dB industry rule: Chen and co-authors note an active VSWR as high as 6 at 15 dB isolation under random port phases [1]. A power amplifier that sees this mismatch will degrade [1]. In angle-of-arrival arrays the concern differs: known phases rather than decorrelation, so keep their calibration separate from a MIMO target.

## Decoupling techniques and what they cost

| Technique | How it works | Cost and risk |
|---|---|---|
| More spacing | Weakens coupling, reduces correlation [2] | Board area |
| Polarisation diversity | Orthogonal elements couple little [2] | Needs orthogonal space and rich multipath |
| Pattern offset | Peaks in different directions [2] | Coverage holes |
| Parasitic element | Adds a second coupling path that cancels the first [3] | Tuned to a narrow band, often shifts resonance |
| Neutralization line | Metal link feeds a cancelling current [7] | Narrowband, extra radiation |
| Defected ground | Cuts or slots block ground current [7] | Back radiation, filter behaviour |
| EBG or metasurface | Stop-band for surface waves [7] | Thick, larger, narrowband |
| Decoupling network | Lumped or line network at the ports [7] | Loss, and it narrows matching |
| Chassis mode control | Place one element mid-chassis, or pair electric and magnetic antennas at opposite short edges [1] | Layout freedom |

Published example: Liu and co-authors placed composite parasitic elements between two monopoles at an edge separation of 0.15 wavelength. Coupling started close to -10 dB. Average improvement was nearly 16 dB, simulated peak isolation was 42.9 dB, and measured isolation was better than 23 dB across the band, with radiation efficiency above 0.9 and ECC below 0.001 [3].

Peak isolation numbers flatter a design. Quote the worst value in band.

## Element spacing for MIMO and arrays

A typical spacing for low correlation is at least half a wavelength [2]. Half a wavelength is 164 mm at 915 MHz, 61 mm at 2.45 GHz and 23 mm at 6.5 GHz, computed free-space.

| Layout | Effect on spacing |
|---|---|
| Patch side by side, H-plane coupling | Edge gap, not centre spacing, sets coupling |
| Collinear wires | Dipole axial nulls reduce coupling |
| Orthogonal | Can sit much closer because polarisation does the work |
| Small terminal | Shared chassis mode dominates below 1 GHz [1] |

Worked example for mimo_patch at 2.45 GHz on FR-4: centre spacing d = 0.5*lambda0 = 61.2 mm and W = 37.2 mm, which leaves an edge gap of only 24 mm, or 0.2 lambda0. Raising d improves isolation directly.

Coupling and correlation both tend to rise as elements get closer. Compared with the open-circuit case at the same spacing, coupling lowers correlation but also lowers embedded efficiency. The efficiency loss usually wins, so diversity gain falls except at very small separations [1]. Use embedded patterns with the other port terminated in its normal load, because that is how the antennas are used.

## Applying this in Sense Sim

Start from antenna_new with mimo_patch. Use antenna_run and antenna_wait, then antenna_results, reading S11, S21 and both port patterns. Use antenna_sweep on d to see isolation against spacing, keeping a mind on the board size. For a design goal, use antenna_optimize with isolation_below, ports i and j, and a band, together with match_below for each port: isolation at one frequency alone will buy a narrow notch.

Use antenna_spec for the shared requirements, and let antenna_assess check them. Other two-port cases: turnstile for orthogonal dipoles, or a blank template with imported geometry using antenna_import_cad for arbitrary layouts, including a parasitic element or defected ground.

Report which ECC you are quoting: a far-field calculation or the S-parameter shortcut. Report the board loss, the worst TARC over phase, and the worst isolation in band. antenna_array summarises an array factor, but coupling between elements is not included in it, so use a two-port or full-array solve for any statement about isolation or active match.

## Sources

1. A Review of Mutual Coupling in MIMO Systems (Chen, Zhang, Li) — IEEE Access, 2018 — https://vbn.aau.dk/ws/files/279622125/FINAL_Article.pdf
2. Envelope Correlation Coefficient (ECC) — antenna-theory.com, undated — https://www.antenna-theory.com/definitions/envelope-correlation-coefficient-ecc.php
3. Isolation Enhancement of a Two-Monopole MIMO Antenna Array with Various Parasitic Elements for Sub-6 GHz Applications (Liu et al.) — Micromachines, 2022 — https://pmc.ncbi.nlm.nih.gov/articles/PMC9780919
4. Application of Cross-correlation Green's Function along with FDTD for Fast Computation of Envelope Correlation Coefficient over Wideband for MIMO Antennas — arXiv, 2016 — https://arxiv.org/pdf/1604.06599
5. Total active reflection coefficient — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Total_active_reflection_coefficient
6. Finding Optimal Total Active Reflection Coefficient and Realized Gain for Multi-port Lossy Antennas (Capek, Jelinek, Masek) — arXiv, 2020 — https://ar5iv.labs.arxiv.org/html/2002.12747
7. Analysis of Isolation Techniques for Mutual Coupling Reduction in MIMO Antennas (Al-tameemi et al.) — J. Communications Software and Systems, 2025 — https://jcoms.fesb.unist.hr/10.24138/jcomss-2025-0091/
