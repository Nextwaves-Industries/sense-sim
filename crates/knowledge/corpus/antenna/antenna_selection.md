---
title: Choosing an antenna type from requirements
domain: antenna
tags: antenna selection, which antenna, antenna type, comparison, trade-off, requirements, bandwidth, FBW, gain, omnidirectional, directional, end-fire, broadside, circular polarisation, CP, size, cost, mounting, dipole, monopole, folded dipole, loop, Yagi, LPDA, log-periodic, helix, turnstile, discone, meander, bow-tie, patch, PIFA, IFA, slot, corner reflector, horn, open waveguide, OEWG, Vivaldi, UWB monopole, MIMO, dielectric resonator, DRA, chip antenna, whip
aliases: chọn loại anten, nên dùng anten nào, so sánh anten, bảng chọn anten, anten đẳng hướng, anten định hướng, anten băng rộng, anten nhỏ gọn, anten giá rẻ, which antenna should I use, best antenna for my band, what antenna gives 10 dBi, wideband antenna options, antenna for a small device
verified: 2026-11-02
---
# Choosing an antenna type from requirements

## Order of decisions when selecting an antenna

Fix the requirements in this order, because each one removes options:

1. Pattern and gain: the required gain sets the electrical size. Omnidirectional means about 2 to 5 dBi; 10 dBi or more needs an aperture, a reflector, or an array.
2. Fractional bandwidth: FBW = (fhi - flo) / fc. A thin patch covers 1 to 3% [1]. Wider bands need a different type, not a matching network.
3. Polarisation: linear, or circular when the far-end orientation is unknown.
4. Size: compute ka with the current-carrying ground included, then apply the Chu limit.
5. Mounting: on metal, near the body, on a board, or free-standing.
6. Cost and assembly.

Worked example: BLE at 2.40 to 2.4835 GHz is 3.4% FBW, on a small board with an unknown peer orientation. That gives ifa_pcb or a chip antenna, not a patch. UHF RFID at 865 to 928 MHz is 7% with an unknown tag rotation, which gives patch_air with circular polarisation or a turnstile.

TI's guidance adds a useful warning: a small efficient antenna at high frequency can give the same or better range than a small inefficient one at low frequency, even though lowering frequency by two doubles the ideal range [2].

## Decision table: resonant wire and sheet antennas

Values are typical, not guarantees. Bandwidth criteria vary between sources, so simulate against your own limit.

| Template id | Peak gain | Bandwidth | Pattern, polarisation | Note |
|---|---|---|---|---|
| dipole | 2.15 dBi [1] | about 8% at VSWR 3 for thin wire [4] | Omni, linear | Balanced, needs a balun |
| strip_dipole | about 2 dBi (rule of thumb) | Wider with strip width | Omni, linear | Etched or stamped |
| folded_dipole | Same pattern as dipole [3] | Slightly wider (rule of thumb) | Omni, linear | About 292 ohm [3] |
| monopole | 5.15 dBi on infinite ground | Similar to dipole | Omni, linear | Needs real ground, unbalanced |
| loop | About 3 dBi (rule of thumb) | Narrow | Broadside, linear | One wavelength around [5] |
| helix_normal | Monopole-like | Narrow | Omni, linear | Small, sensitive to detuning |
| meander | Below monopole | Narrow, Chu-limited | Omni, linear | Fits tight spaces |
| turnstile | About 2 dBi on boresight (worked example) | Set by dipole and hybrid | CP on axis, linear in plane [3] | Two ports in quadrature |

Worked example for the turnstile: two half-wave dipoles each take half the power. Each gives 2.15 - 3 = -0.85 dBi on axis, and the orthogonal components add in power, so about 2 dBi total.

## Decision table: directional antennas

| Template id | Peak gain | Bandwidth | Pattern, polarisation | Note |
|---|---|---|---|---|
| yagi | Usually above 10 dB [6] | A few percent [6] | End-fire, linear | First three directors add about 3, 2 and 1.5 dB [6] |
| corner_reflector | Feed gain plus 9 to 12 dB at half-wave spacing [7] | Moderate | Broadside, linear | Spacing lambda/3 to 2*lambda/3, aperture 1 to 2 lambda [8] |
| horn | 10 to 20 dB typical, up to 25 dB [9] | About 50% [10] | End-fire, linear | Low loss, so gain nearly equals directivity [10] |
| oewg | About 5 to 7 dBi (rule of thumb) | Waveguide band | Broad, linear | Probe or feed, not a high-gain antenna |
| helix_axial | 10 to 15 dBi [11] | AR under 2 dB over 1.7:1 [11] | End-fire, CP | Tall; about 140 ohm |
| patch_air | Up to 8 to 9 dBi [1] | Several percent, simulate | Broadside, linear | Low loss reader panel; CP needs a blank model |

Worked example for a horn: G = 4*pi*A*B*ea / lambda^2 with aperture efficiency ea = 0.51 for an optimum-gain design [10]. A 2.4 x 1.8 wavelength aperture gives 27.7, or 14.4 dBi. Going to 20 dBi needs an aperture area of about 15.6 square wavelengths, 3.6 times larger.

Choosing between them: a corner reflector is cheap and simple, a Yagi is light but narrow, and a horn is the most predictable and is used as a gain standard [10].

## Decision table: wideband antennas

| Template id | Peak gain | Bandwidth | Pattern, polarisation | Note |
|---|---|---|---|---|
| lpda | 6 to 10 dBi, set by tau and sigma [12] | 3:1 to 10:1 [12] | End-fire, linear | 50 to 200 ohm feed [12], long boom |
| discone | About dipole level (rule of thumb) | A couple of octaves [3] | Omni, linear | High-pass: cut-off where cone slant is about lambda/4 [3] |
| bowtie | About dipole level (rule of thumb) | 33% against 8% for a thin wire, same length, VSWR 3 [4] | Omni, linear | Balanced feed |
| uwb_monopole | About 2 to 5 dBi (rule of thumb) | Several GHz | Omni-like, linear, varies with frequency | Pattern and delay change across band |
| vivaldi | About 5 to 10 dBi (rule of thumb) | Very wide | End-fire, linear [13] | Two orthogonal units in quadrature give CP [13] |

A wide S11 band does not mean a stable pattern. An LPDA keeps gain nearly constant, but a printed disc monopole changes pattern with frequency. For pulse systems such as UWB ranging, check group delay as well as match. As a commercial reference, a Johanson UWB chip antenna covers 3.1 to 10.3 GHz, which is 107% [14].

Cost scales with the boom and the machining: discone and LPDA are mechanical assemblies, whereas bowtie, uwb_monopole and vivaldi can be etched on a board. Use vivaldi on low-loss laminate, since the template default is RO4003C.

## Decision table: printed and small device antennas

| Template id | Peak gain | Bandwidth | Pattern, polarisation | Note |
|---|---|---|---|---|
| patch_probe | Directivity 5 to 7 dB [15], at most 8 to 9 dBi [1] | 1 to 3% [1] | Broadside, linear | Probe needs a drilled via |
| patch_inset | As above | 1 to 3% [1] | Broadside, linear | One-layer feed, notch distorts pattern slightly [1] |
| patch_circular | As above | 1 to 3% (rule of thumb) | Broadside, linear | TM11 mode; probe on one axis |
| mimo_patch | As patch | As patch | Two ports | Study isolation and ECC |
| pifa | Peak near monopole on small ground (rule of thumb) | Grows with height | Near-omni | About half a patch size [16] |
| ifa_pcb | 3.3 dBi peak measured [17] | Over 300 MHz at -10 dB, about 12% at 2.44 GHz [17] | Near-omni | Free copper, needs board edge |
| slot | About 2 to 3 dB [18] | About 8% at -6 dB with a cavity [18] | Bidirectional or one side | High feed impedance |
| dra | About 5 dBi (rule of thumb) | Fraction of a percent to about 20% [19] | Broadside | No conductor loss in the resonator [19] |

A chip antenna has no template. Most have a peak gain near 0 dBi, with average gain from -6 to +1 dBi, so use -2 dBi for a first link budget [14]. Chip parts are not suitable when 3 dBi average gain is required [14].

## Choosing by size, cost and mounting

TI compares device antennas as follows [2]. A PCB antenna costs almost nothing and performs well above 868 MHz, but is hard to make small and efficient below 433 MHz. A chip antenna is small and costs about 0.10 to 0.50 USD, but it is often narrowband and needs matching on each new board. A whip performs best but needs a connector and can be large at low frequencies.

Measured examples of the penalty for shrinking: two TI boards with identical size and antenna area reached 91% efficiency at 868 MHz and 7% at 169 MHz [2]. In a second TI design, efficiency was 84.1% at 2440 MHz and 25.3% at 510 MHz [20].

Mounting rules:

| Mounting | Prefer |
|---|---|
| Directly on metal | patch_air, patch_probe, pifa, dra: own ground plane |
| Free-standing mast | dipole, monopole with radials, yagi, discone |
| Small board in plastic | ifa_pcb, chip antenna, meander |
| Wall anchor with a sector beam | patch_probe, patch_air, corner_reflector |
| Lab standard or probe | horn, oewg, dipole |

## Choosing by polarisation

Use linear polarisation when both ends are fixed and aligned, since it avoids the 3 dB loss a linear antenna takes from a circular wave. Use circular when the far end rotates or its orientation is unknown, or for satellite links where Faraday rotation applies [1].

| Way to get CP | Template start | AR bandwidth |
|---|---|---|
| Single-feed nearly square or truncated patch | Blank model: patch_probe has its probe on one axis | 1 to 3% [1] |
| Dual-feed square patch with splitter and 90 deg delay | Blank model or two-port | At least 3%, set by the phase network [1] |
| Crossed dipoles in quadrature | turnstile | Set by the network; CP only near the axis [3] |
| Axial helix | helix_axial | AR under 2 dB over 1.7:1 [11] |
| Two Vivaldi units in quadrature | vivaldi, two built | Wide [13] |

A 3 dB axial ratio is considered sufficient for most uses [1]. Only the helix gives circular polarisation with no phase network or perturbed shape. The penalty is height: 225 mm for eight turns at 2.45 GHz in the existing helix worked example.

## Applying this in Sense Sim

Call antenna_spec first with the band and limits, then antenna_list_templates and antenna_new with the chosen id and frequency. Use antenna_calc to check Chu and starting dimensions, run antenna_run with antenna_wait, and let antenna_assess judge it. If the verdict shows the type cannot meet a limit, change type rather than raising antenna_optimize max_runs.

Check the port reference before judging S11: several templates default to a port impedance other than 50 ohm so that the resonance shows clearly. These are dipole and strip_dipole 73, folded_dipole 300, loop 130, lpda, vivaldi and corner_reflector 100, helix_axial 140, helix_normal 20, meander 15, slot 500, turnstile 73, discone and horn 75. A good S11 in those cases still needs a transformer or matching network to reach a 50 ohm radio, which antenna_match can design. Use antenna_set to change Z0 to 50 when you want the true mismatch.

## Sources

1. The Basics of Patch Antennas, Updated (Orban, Moernaut) — Orban Microwave Products, 2025 — https://www.orbanmicrowave.com/wp-content/uploads/2025/08/The-Basics-Of-Patch-Antennas-Updated.pdf
2. Antenna Selection Guide, AN058 / SWRA161B — Texas Instruments, undated — https://ti.com/lit/swra161
3. Lecture 11: Practical Dipole/Monopole Geometries, Matching (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L11_Match.pdf
4. Bowtie Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/wideband/bowtie.php
5. Lecture 12: Loop Antennas (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L12_Loop.pdf
6. Yagi-Uda Antenna — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/travelling/yagi.php
7. Corner Reflector — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/reflectors/cornerReflector.php
8. Reflector antennas lecture notes, corner reflector section — Rohini College of Engineering and Technology, undated — https://rcet.org.in/uploads/academics/rohini_84037743787.pdf
9. Horn Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/aperture/horn.php
10. Lecture 18: Horn Antennas (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L18_Horns.pdf
11. Axial-Mode Helix Antenna, RF glossary — RF Essentials, undated — https://rfessentials.com/resources/rf-glossary/axial-mode-helix/
12. How do I design a log-periodic dipole array — RF Essentials, undated — https://rfessentials.com/rf-knowledge-base/how-do-i-design-a-log-periodic-dipole-array-for-a-wideband-measurement-antenna/
13. Vivaldi antenna — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Vivaldi_antenna
14. Understanding Chip Antennas Handbook — Johanson Technology, 2023 — https://www.johansontechnology.com/docs/4565/johanson-antenna-handbook.pdf
15. Patch Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/patches/antenna.php
16. PIFA, Planar Inverted-F Antenna — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/patches/pifa.php
17. 2.4-GHz Inverted F Antenna, SWRU120D — Texas Instruments, 2019 — https://ti.com/lit/swru120
18. Cavity-backed Slot Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/aperture/slot2.php
19. Dielectric resonator antenna — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Dielectric_resonator_antenna
20. SWRA730 Application Note, dual-band PCB antenna section — Texas Instruments, undated — https://www.ti.com/document-viewer/lit/html/SWRA730/GUID-E2C4847B-FF4F-4DB4-AEAB-23D546313AB2
