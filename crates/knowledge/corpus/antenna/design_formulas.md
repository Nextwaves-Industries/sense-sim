---
title: Antenna design formulas with 915 MHz, 2.45 GHz and 6.5 GHz numbers
domain: antenna
tags: antenna design, dipole, half-wave, monopole, quarter-wave, folded dipole, patch, microstrip, MSA, Hammerstad, Balanis, inset feed, probe feed, circular patch, PIFA, IFA, inverted-F, meander, helix, axial mode, Yagi, Yagi-Uda, slot, Babinet, dimensions, resonant length
aliases: thiết kế anten, công thức anten, anten lưỡng cực, anten đơn cực, anten vi dải, anten patch, kích thước patch, điểm cấp nguồn, anten PIFA, anten xoắn, anten Yagi, anten khe, how long should the antenna be, patch size calculator
verified: 2026-11-02
---
# Antenna design formulas with 915 MHz, 2.45 GHz and 6.5 GHz numbers

## Half-wave dipole length and impedance

An exact half-wavelength thin dipole has about 73 + j42.5 ohm [1]. The positive reactance is removed by shortening: about 0.47 lambda for a thick dipole and 0.48 lambda for a thin one [1]. Near a half wavelength the resistance stays close to 73 ohm regardless of diameter, while the reactance depends strongly on length and diameter [1]. Directivity is 1.643 (2.16 dB) [1].

| Frequency | lambda | 0.47 lambda | 0.48 lambda |
|---|---|---|---|
| 915 MHz | 327.6 mm | 154.0 mm | 157.3 mm |
| 2.45 GHz | 122.4 mm | 57.5 mm | 58.7 mm |
| 6.5 GHz | 46.1 mm | 21.7 mm | 22.1 mm |

(Worked example, c0 = 299792458 m/s.) A dipole printed on a substrate is shorter than these numbers because the board raises effective permittivity. Dipoles are balanced and need a balun. A folded half-wave dipole has about 292 ohm, or four times the single-dipole resistance, with wire spacing no more than 0.05 lambda [2].

## Quarter-wave monopole dimensions and ground

A monopole on a ground plane has half the impedance and twice the directivity of its dipole counterpart [1]. An ideal quarter-wave monopole has about 36.5 ohm, and D = 3.29 (5.16 dB).

| Frequency | 0.235 lambda height |
|---|---|
| 915 MHz | 77.0 mm |
| 2.45 GHz | 28.8 mm |
| 6.5 GHz | 10.8 mm |

(Worked example.) A monopole connected directly to 50 ohm shows VSWR of about 1.4 (36.5/50) at resonance, already adequate for most uses; a -10 dB match needs no matching network in the ideal case. The figures assume a large ground. Radials of 0.25-0.35 lambda are the practical approximation for a good ground plane [1]. On a small PCB the board radiates too and resonance shifts, so simulate the real board size.

## Rectangular patch width and length equations

Transmission-line model for a rectangular microstrip patch, dominant mode [3][4]. Inputs: resonant frequency fr, substrate relative permittivity er, height h.

1. Width: W = c / (2 * fr) * sqrt(2 / (er + 1))
2. Effective permittivity, W/h > 1: eeff = (er + 1)/2 + (er - 1)/2 * (1 + 12*h/W)^(-1/2)
3. Fringing extension (Hammerstad): dL = 0.412 * h * (eeff + 0.3) * (W/h + 0.264) / ((eeff - 0.258) * (W/h + 0.8))
4. Length: L = c / (2 * fr * sqrt(eeff)) - 2 * dL

The model is simple rather than exact, and resonance is very sensitive to L [3]. Use it for the starting point, then simulate. Check: with c = 3e8 m/s, this procedure gives W = 11.86 mm and L = 9.06 mm at 10 GHz, er 2.2, h 1.588 mm, the dimensions used in the Balanis inset example [5].

## Worked rectangular patch dimensions by frequency and laminate

Worked examples from the Hammerstad transmission-line equations (c0 = 299792458 m/s). Values are starting points to simulate, not finished designs.

| fr | Laminate, er used | h (mm) | W (mm) | eeff | dL (mm) | L (mm) |
|---|---|---|---|---|---|---|
| 915 MHz | FR-4, 4.4 | 1.6 | 99.70 | 4.257 | 0.745 | 77.91 |
| 915 MHz | RO4003C, 3.55 | 1.524 | 108.61 | 3.455 | 0.732 | 86.68 |
| 915 MHz | foam, 1.07 | 8.0 | 161.03 | 1.063 | 5.439 | 148.04 |
| 2.45 GHz | FR-4, 4.4 | 1.6 | 37.23 | 4.081 | 0.739 | 28.81 |
| 2.45 GHz | FR-4, 4.04 | 1.6 | 38.54 | 3.762 | 0.748 | 30.05 |
| 2.45 GHz | RO4003C, 3.55 | 1.524 | 40.56 | 3.334 | 0.727 | 32.06 |
| 6.5 GHz | RO4003C, 3.55 | 0.813 | 15.29 | 3.271 | 0.386 | 11.98 |

A 915 MHz patch on a printed board is large (about 100 x 80 mm) and very narrowband, one reason reader panels favour air or foam. The er value matters: use laminate design Dk, not process Dk, where the maker gives both.

## Patch feed position: edge resistance and inset depth

At resonance, the input resistance at the radiating edge is Redge = 1 / (2 * (G1 + G12)), with G1 the slot conductance and G12 the mutual conductance between the two slots [3]. The resistance at distance y0 from the edge along the resonant length is approximately R(y0) = Redge * cos^2(pi * y0 / L) [3], so y0 = (L/pi) * acos(sqrt(Rtarget/Redge)). For microstrip inset feeds, notch width also affects impedance.

Worked example, 50 ohm target, using numerically integrated G1 and G12 (validated by reproducing the Balanis example, 228.35 ohm [5]):

| Design | L (mm) | Redge | y0 from edge | Distance from centre |
|---|---|---|---|---|
| 2.45 GHz FR-4 4.4, 1.6 mm | 28.81 | 321 ohm | 10.69 mm | 3.72 mm |
| 2.45 GHz RO4003C 3.55, 1.524 mm | 32.06 | 288 ohm | 11.65 mm | 4.38 mm |
| 6.5 GHz RO4003C 3.55, 0.813 mm | 11.98 | 287 ohm | 4.35 mm | 1.64 mm |

The 50 ohm point is about L/7 to L/8 from centre. A probe at L/6 from centre (L/3 from the edge) gives Redge/4, about 80 ohm and -12.7 dB S11 before probe inductance, which is a usable start but not an exact match.

## Patch bandwidth and substrate height limits

A simple patch has narrow impedance bandwidth, at most a couple of percent [4]. One published approximation for VSWR below 2 is BW = 3.771 * ((er - 1)/er^2) * (h/lambda0) * (W/L) [6], which applies to thin substrates and low loss and does not suit er near 1.

| Design (worked example) | BW estimate |
|---|---|
| 915 MHz FR-4, 1.6 mm | 0.41%, 3.8 MHz |
| 2.45 GHz FR-4, 1.6 mm | 1.12%, 27 MHz |
| 2.45 GHz RT/duroid 5880, 1.588 mm | 1.45%, 36 MHz |
| 6.5 GHz RO4003C, 0.813 mm | 1.72%, 112 MHz |

Consequences: a thin patch covers only part of a 499.2 MHz UWB channel; and a 915 MHz thin-laminate patch covers one regional band at best. Thicker, lower-permittivity substrates raise efficiency and bandwidth [4]. TM0 surface wave excitation becomes appreciable at h/lambda above about 0.09 for er 2.3 and above about 0.03 for er 10 [4]. A proximity-coupled feed can reach up to 13% [4]. Dielectric loss and thick substrates will broaden the real response beyond this estimate.

## Circular patch radius equation

For a circular microstrip patch in the dominant TM11 mode, the resonant frequency is fr = 1.8412 * c / (2 * pi * aeff * sqrt(er)), with fringing-corrected radius aeff = a * (1 + (2*h/(pi*a*er)) * (ln(pi*a/(2*h)) + 1.7726))^(1/2) [7].

Design form (SI units): F = 8.791e7 / (fr * sqrt(er)), then a = F / sqrt(1 + (2*h/(pi*er*F)) * (ln(pi*F/(2*h)) + 1.7726)).

| Worked example | F (mm) | a (mm) |
|---|---|---|
| 915 MHz, FR-4 4.4, h 1.6 mm | 45.80 | 45.17 |
| 2.45 GHz, FR-4 4.4, h 1.6 mm | 17.11 | 16.60 |
| 2.45 GHz, RO4003C 3.55, h 1.524 mm | 19.04 | 18.43 |
| 6.5 GHz, RO4003C 3.55, h 0.813 mm | 7.18 | 6.88 |

Input resistance varies with probe position: low near the centre and high near the edge. Find the position by simulation or sweep.

## PIFA and PCB inverted-F design rules

A planar inverted-F antenna (PIFA) is a shorted plate above ground. With a thin shorting pin, resonance occurs when L1 + L2 (open edge to short) is about a quarter wavelength; if the short spans the full plate width, L1 alone sets the length [8]. Moving the feed closer to the short lowers impedance [8]. Capacitive loading shortens the antenna but reduces efficiency and bandwidth [8]. The ground radiates too, so it is part of the antenna [8].

Measured reference: TI SWRU120 inverted-F, on 1 mm FR-4, 25.7 x 7.5 mm excluding ground, peak gain +3.3 dBi, under 10% reflection across more than 300 MHz [9]. TI advises keeping ground and components away from the antenna end points and including a pi network near the feed [9].

Worked starting point (0.2 lambda length, 0.1 lambda wide, 0.05 lambda high, feed 0.045 lambda from short):

| f | L | W | h | Feed offset |
|---|---|---|---|---|
| 915 MHz | 65.5 mm | 32.8 mm | 16.4 mm | 14.7 mm |
| 2.45 GHz | 24.5 mm | 12.2 mm | 6.1 mm | 5.5 mm |
| 6.5 GHz | 9.2 mm | 4.6 mm | 2.3 mm | 2.1 mm |

Here plate length plus height is 0.25 lambda.

## Meander line monopole starting dimensions

A meander line folds a monopole to fit a smaller height. It has no reliable closed-form resonance, because adjacent segments carry opposite currents that partly cancel and shorten the electrical length. Expect the conductor length needed to exceed a straight quarter wave, and tune it by simulation. Meandering is also a common way to reduce RFID tag size [10].

Starting point (worked example, about 0.268 lambda of wire: h0 0.02 lambda, width 0.05 lambda, pitch 0.012 lambda, 4 folds):

| f | h0 | Width | Pitch | Wire length |
|---|---|---|---|---|
| 915 MHz | 6.6 mm | 16.4 mm | 3.9 mm | 87.8 mm |
| 2.45 GHz | 2.4 mm | 6.1 mm | 1.5 mm | 32.8 mm |
| 6.5 GHz | 0.9 mm | 2.3 mm | 0.6 mm | 12.4 mm |

Physics limits the result: a smaller enclosing sphere raises the minimum Q, so check the Chu limit before promising both a small size and a wide band. Sweep N or width rather than scaling blindly, and include the board for printed versions.

## Axial-mode helix dimensions and gain

An axial-mode helix radiates end-fire with circular polarisation when circumference C is comparable to the wavelength [11]; the operating range is about 0.75 C to 1.33 C in wavelength [12]. Helices of at least 3 turns give near-circular polarisation near C = lambda, and AR improves with more turns [12]. Typical values: pitch angle 12-14 deg, gain 10-15 dBi, AR below 2 dB over about 1.7:1 bandwidth [13]. An empirical Kraus gain is G = 15 * N * C^2 * S / lambda^3 [13]; treat it as optimistic and verify in simulation. Input resistance is about 140 ohm when C = lambda (rule of thumb), so 50 ohm systems need a matching section.

Worked example, C = lambda, S = 0.23 lambda (12.95 deg pitch), N = 8, G about 14.4 dBi:

| f | Diameter | Spacing | Height |
|---|---|---|---|
| 915 MHz | 104.3 mm | 75.4 mm | 603 mm |
| 2.45 GHz | 38.9 mm | 28.1 mm | 225 mm |
| 6.5 GHz | 14.7 mm | 10.6 mm | 85 mm |

A normal-mode helix (C well below lambda) is a separate small, narrowband, monopole-like design, not an end-fire CP antenna.

## Yagi-Uda element rules

A Yagi-Uda has one driven element (typically 0.45-0.48 lambda long), a longer reflector that behaves inductively, and shorter directors that behave capacitively [14]. One reflector is usually enough. Gain steps: the first director adds about 3 dB, the second about 2 dB, the third about 1.5 dB, and past 8 directors each adds under 0.5 dB [14]. Typical gain exceeds 10 dB, with bandwidth of a few percent [14].

Worked starting point (0.5 lambda reflector, 0.47 lambda driven, 0.42 lambda first director, 0.2 lambda spacing):

| f | Reflector | Driven | Director 1 | Spacing |
|---|---|---|---|---|
| 915 MHz | 163.8 mm | 154.0 mm | 137.6 mm | 65.5 mm |
| 2.45 GHz | 61.2 mm | 57.5 mm | 51.4 mm | 24.5 mm |
| 6.5 GHz | 23.1 mm | 21.7 mm | 19.4 mm | 9.2 mm |

Close parasitic elements pull the feed impedance away from a free dipole, so check the actual value, then match. Optimise front-to-back and gain together, since the settings that maximise one usually do not maximise the other. A single forward-looking Yagi is directional enough to need aiming.

## Slot antenna and Babinet impedance

A slot in a conducting plate is the complement of a strip dipole, with fields interchanged. Babinet relates their impedances: Zslot * Zdipole = eta^2 / 4 [15], with eta about 376.73 ohm, so Zslot = 35481 / Zdipole ohm. Reactance signs flip: an inductive dipole gives a capacitive slot.

| Complementary dipole (ohm) | Slot impedance (worked example, ohm) |
|---|---|
| 73 | 486 |
| 65 | 546 |
| 65 + j15 | 518 - j120 |
| 73 + j42.5 | 363 - j211 |

A slot about 0.48 lambda long is 157.3 mm at 915 MHz, 58.7 mm at 2.45 GHz, and 22.1 mm at 6.5 GHz. Polarisation is rotated 90 deg relative to the complementary dipole. Feeding a resonant slot directly from 50 ohm gives severe mismatch, so feed it off-centre or use a matching section. Babinet assumes a large thin plate; a finite plate or a cavity changes impedance and pattern.

## Applying this in Sense Sim

Antenna Studio templates hold these equations as editable expressions: wire types (dipole, monopole, folded dipole, loop, Yagi, helix, turnstile, meander), sheet types (strip dipole, air patch, PIFA, slot, corner reflector), apertures (horn, open waveguide, dielectric resonator), printed types (probe, inset and circular patches, inverted-F, two-port pair), wideband types (log-periodic, discone, bow-tie, UWB disc monopole, Vivaldi) and a blank model. In src/antenna/tools.rs, antenna_new starts a design from a template id at a frequency, antenna_set edits variables and materials, antenna_run simulates and antenna_results reports.

Treat template defaults as starting points: confirm frequency and substrate, run once, adjust the dimension that governs resonance, then the feed. The inset template edge-resistance expression, 90*er^2/(er-1)*(L/W)^2, agrees within about 5% with the slot-conductance calculation for er above 2, but it diverges as er approaches 1, so do not use it for air or foam; set the inset depth directly there.

## Sources

1. Lecture 9: Linear Wire Antennas, Dipoles and Monopoles (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L09_Dipole.pdf
2. Lecture 11: Practical Dipole/Monopole Geometries, Matching (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L11_Match.pdf
3. Lecture 21: Microstrip Antennas, Part II (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L21_Mstrip2.pdf
4. Lecture 20: Microstrip Antennas, Part I (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L20_Mstrip1.pdf
5. Balanis Antenna Theory, Chapter 14 worked examples as code — FOSSEE, IIT Bombay, undated — https://tbc-python.fossee.in/convert-notebook/Antenna_Theory_-_Analysis_and_Design_by_C.A._Balanis/chapter14.ipynb
6. Topic 5: Microstrip Patch Antenna, EE-4382/5306 Antenna Engineering (R. Rumpf) — University of Texas at El Paso, 2018 — https://empossible.net/wp-content/uploads/2018/03/Topic-5-Microstrip-Patch-Antenna.pdf
7. Lecture 22: Microstrip Antennas, Part III (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L22_Mstrip3.pdf
8. PIFA, Planar Inverted-F Antenna — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/patches/pifa.php
9. 2.4-GHz Inverted F Antenna, SWRU120D — Texas Instruments, 2019 — https://ti.com/lit/swru120
10. Analysis of UHF RFID Tag Antennas Using an Equivalent Circuit Approach (Nikitin, Kim, Rao) — IEEE J. Radio Frequency Identification, 2026 — https://people.ece.uw.edu/nikitin_pavel/papers/JRFID_2026_antennas.pdf
11. Helical Antenna Design example — MathWorks, undated — https://www.mathworks.com/help/antenna/ug/helical-antenna-design.html
12. Helical Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/travelling/helix.php
13. Axial-Mode Helix Antenna, RF glossary — RF Essentials, undated — https://rfessentials.com/resources/rf-glossary/axial-mode-helix/
14. Yagi-Uda Antenna — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/travelling/yagi.php
15. Slot Antennas — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/aperture/slot.php
