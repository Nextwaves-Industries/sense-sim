---
title: Lumped RF parts, matching on a board and filters
domain: rf
tags: RF capacitor, RF inductor, self-resonant frequency, SRF, series resonance, ESL, ESR, Q factor, MLCC, C0G, 0402, 0201, E24, E12, E-series, preferred values, Coilcraft 0402CS, Murata GJM, Johanson, matching network, L network, pi network, T network, tuning, VNA, SAW filter, ceramic filter, LTCC, LC filter, bandpass filter, harmonic filter, insertion loss, part loss
aliases: tụ RF, cuộn cảm RF, tần số tự cộng hưởng, hệ số phẩm chất, mạch phối hợp trở kháng, phối hợp trở kháng, tinh chỉnh mạch phối hợp, giá trị linh kiện chuẩn, bộ lọc SAW, bộ lọc gốm, bộ lọc LC, suy hao chèn, suy hao linh kiện, which capacitor value for matching, how to tune a matching network
verified: 2026-11-02
---
# Lumped RF parts, matching on a board and filters

## RF capacitors: ESL and self-resonance

Every capacitor is a series C, ESR and ESL. Below its self-resonant frequency (SRF) it is capacitive; at SRF impedance falls to ESR; above it, it is inductive [1]. Murata shows an MLCC with about 0.5 nH at high frequency, and notes that this varies by part [1]. SRF = 1 / (2 pi sqrt(ESL x C)).

With 0.5 nH, computed:

| Value | SRF | Effective C at 2.45 GHz |
|---|---|---|
| 1 pF | 7.1 GHz | 1.1 pF |
| 2.2 pF | 4.8 GHz | 3.0 pF |
| 4.7 pF | 3.3 GHz | 10.6 pF |
| 10 pF | 2.25 GHz | Inductive |

So a 2.2 pF part acts as 3 pF, and a 4.7 pF at 2.45 GHz is more than twice its label. A 1 pF part at 6.5 GHz acts as 6 pF. RF high-Q series do better: Johanson's S-Series lists self-resonance up to 25 GHz across its range, 0402 from 0.2 to 33 pF [2].

Choose C0G for the stable temperature coefficient: the S-Series lists 0 +/- 30 ppm/C [2]. Pad and via inductance add to ESL, so a shunt capacitor is only as good as its ground via. Use a near-SRF capacitor as a low-impedance RF short for DC block or decoupling: 10 pF is good near 2.25 GHz, for example.

## RF inductors: Q and self-resonance by value

The Coilcraft 0402CS wirewound series shows the usual trend: as inductance rises, SRF falls [3].

| Part | L at 900 MHz | Q at 900 MHz | Q at 1.7 GHz | SRF min | DCR max |
|---|---|---|---|---|---|
| 1N0 | 1.02 nH | 77 | 69 | 12.7 GHz | 0.045 ohm |
| 2N2 | 2.19 nH | 59 | 100 | 10.8 GHz | 0.070 ohm |
| 4N3 | 4.19 nH | 47 | 71 | 6.0 GHz | 0.091 ohm |
| 10N | 9.8 nH | 50 | 67 | 3.9 GHz | 0.20 ohm |
| 18N | 18.3 nH | 57 | 62 | 3.1 GHz | 0.23 ohm |
| 27N | 28.7 nH | 49 | 63 | 2.48 GHz | 0.30 ohm |
| 47N | 50 nH | 38 | not listed | 2.1 GHz | 0.83 ohm |

Near SRF, apparent inductance climbs: the 27 nH part is 33.5 nH at 1.7 GHz [3]. Use the L at your frequency, not the label.

Infineon says to choose capacitors and inductors with self-resonance at least twice the operating frequency [4]. By that rule a 27 nH part suits up to about 1.2 GHz. At 2.45 GHz the limit is 4.9 GHz, which 4.3 nH clears and 5.1 to 7.5 nH (4.8 GHz) just miss. Tolerance also matters: 5% of 2.2 nH is 0.11 nH, similar to a short trace.

## Standard E-series values for matching parts

IEC 60063 defines preferred values, with E6 for 20%, E12 for 10%, E24 for 5%, E48 for 2% and E96 for 1% [5]. E24 values per decade: 1.0, 1.1, 1.2, 1.3, 1.5, 1.6, 1.8, 2.0, 2.2, 2.4, 2.7, 3.0, 3.3, 3.6, 3.9, 4.3, 4.7, 5.1, 5.6, 6.2, 6.8, 7.5, 8.2, 9.1 [5].

RF parts often have finer ranges than E24. For example 0402CS adds 1.9, 8.7, 9.0, 9.5, 19 and 23 nH [3]. Small capacitors have absolute tolerance rather than percentage. Treat the tolerance as absolute when the value is small.

Rounding to E24 is rarely the main error at RF. Worked example at 2.45 GHz: a 2.2 nH inductor is j33.9 ohm, and the next value up, 2.4 nH, adds 3 ohm (computed). The parasitics in the previous sections, pad inductance and 5% tolerance, are of the same size. So a simulated network with five significant figures is not meaningful: round to real parts, evaluate the result, then tune on the board.

## Choosing an L, pi or T matching network

An L network has two parts, and Q is fixed by the resistance ratio; the equations are in the antenna matching document. A pi or T has three parts and allows Q to be chosen. Layout choices:

- Infineon's tip: with an unknown antenna, lay out three parts in a pi or T, as it lets you use all possible topologies later [4].
- Espressif calls for at least one CLC circuit at the chip with 0201 parts placed close to the pin, and an extra CLC near a PCB antenna for tuning [6].
- For high-rate links Infineon recommends two parts for radio tuning and two for antenna tuning [4].

Picking element types (rule of thumb):
- Series L with shunt C is low-pass and helps reject harmonics. Prefer it after a transmitter.
- Series C with shunt L is high-pass, blocks DC and gives an ESD path to ground at the antenna.
- Avoid a matching path with very high transformation Q, because loss and tolerance sensitivity rise with Q.

Populate unused footprints with a 0 ohm link for series positions and leave shunt positions open. Check that a 0 ohm link is low in inductance at your frequency.

## Loss in matching parts: worked estimate

A series element with reactance X and quality factor Q has resistance Rs = |X| / Q. Fraction lost is Rs / (Rs + Rload), for a series element.

Worked example: match 10 ohm to 50 ohm at 2.45 GHz with Xs = 20 ohm (the antenna matching document example).
- Inductor Q = 60, so Rs = 0.33 ohm: 0.14 dB lost.
- Inductor Q = 30, so Rs = 0.67 ohm: 0.28 dB lost.

Values computed. Part loss rises with transformation ratio. A small chip antenna with a few ohms radiation resistance loses far more, since Rs competes with Rrad directly. For example 2 ohm radiation resistance and 1 ohm in an inductor is 1.8 dB lost before matching error (computed).

Q values across the Coilcraft series run from 28 to 100 at 900 MHz and 1.7 GHz [3]. Measured Q is taken in a fixture, so a board with long pads is worse. Note that a part's Q is its own loss only: it does not cover copper on the matching traces, which the microstrip document covers separately. In a receive chain, these losses add directly to noise figure.

## Tuning a matching network on a real board

Infineon AN91445 gives the standard method [4]:

1. Calibrate the network analyzer through its cables and connectors up to the measuring point. Measure at the same point where parts will be placed.
2. Disconnect the chip side by removing its matching parts, and look toward the antenna from the 50 ohm reference point.
3. Adjust matching parts while watching the Smith chart, aiming for over 10 dB return loss across the band.
4. Tune the radio side separately, with the antenna parts disconnected.
5. Repeat on the final product. Tuning is two-step: bare PCB first, then again after the design is final, with the enclosure and body contact.

Practical notes (rule of thumb): add a pigtail with its shield soldered to ground at the measuring point, and use port extension to remove its delay. Ferrite beads on the cable reduce common-mode current on small ground planes. Change one part at a time and photograph the Smith trace each time. A shunt part's real value includes pad and via; if a value does not act as calculated, believe the instrument.

A return loss over 10 dB transfers 90% of the power [4]. Also keep a margin for detuning by the enclosure, hands and tolerances; a wider band is preferred for that reason [4].

## Filter types and insertion loss

Measured data sheet values:

| Filter | Type | Band | Insertion loss | Rejection | Max power |
|---|---|---|---|---|---|
| TAI-SAW TA1102A | SAW | 902-928 MHz | 2 typ, 3 max dB | 40 min at 10-857.5 MHz; 35 min at 970-1005 MHz; 30 min at 1110-3000 MHz | 15 dBm [7] |
| Johanson 2450BP14D0100001T | Multilayer ceramic | 2400-2500 MHz | 1.5 typ, 1.7 max dB | 30 min at 880-915 MHz; 25 min at 4.8-5 GHz; 15 min at 7.2-7.5 GHz | 2 W CW [8] |
| Johanson 2450BP15B0100001E | Multilayer ceramic | 2400-2500 MHz | 2.2 max dB | 34 min at 4.8-5 GHz; 30 min at 3.6-3.8 GHz; 12 min at 3 GHz | 0.5 W CW [9] |

Trade-offs (rule of thumb):
- SAW is the sharpest near the band but low in power and limited in frequency, so use it on receive paths. A 15 dBm limit rules it out after a 30 dBm reader.
- Ceramic has moderate selectivity with good power and good far-out rejection. Ceramic rejection is often weaker at high harmonics.
- Discrete LC is cheap and is good for harmonics, but poor for narrowband. Its loss follows the Cohn estimate: 4.343 x sum(g) x (f / BW) / Q dB. A 3-pole Butterworth at 10% bandwidth with Q 50 costs about 3.5 dB (computed).

Return loss of a filter is often only 9.5 dB (the 2450BP14 minimum), so it also changes the match.

## Applying this in Sense Sim

After antenna_run, call antenna_match with f_ghz and optional q_l and q_c. It returns L, pi, T, stub and band-optimised networks, with ideal values, the result with E24 parts, S11 at the frequency and worst over the band, and power lost in the parts. Its defaults are inductor Q 40 and capacitor Q 200. The Coilcraft inductor data supports 40 as a fair low-end figure; no opened source gave a capacitor Q at the design frequency, so treat 200 as an assumption, and pass measured values when known. It assumes no pad or via parasitics, so state that values need tuning on the real board.

Run it before changing geometry if z_im from antenna_results is the main error. Change the antenna with antenna_set if the match needs a large transformation or the band is narrow; compare the lost power from antenna_match with antenna_assess. Use antenna_calc with bode_fano to see whether any network can reach the band. Add filter losses by hand to a link budget, as rtls_coverage does not model them.

## Sources

1. 6-5-2. Capacitor equivalent circuits — Murata, accessed 2026 — https://www.murata.com/en-global/products/emc/emifil/knowhow/basic/chapter06-p6
2. S-Series Ultra-Low ESR Capacitors — Johanson Technology, accessed 2026 — https://www.montana.edu/aolson/ee433/Johanson_S-Series.pdf
3. Chip Inductors 0402CS Series, Document 198 — Coilcraft, 2007 — https://ipv4.pccomponents.com/datasheets/COIL-0402CS.PDF
4. AN91445, Antenna Design and RF Layout Guidelines — Cypress / Infineon, Rev. H — https://www.infineon.com/dgdl/Infineon-AN91445_Antenna_Design_and_RF_Layout_Guidelines-ApplicationNotes-v09_00-EN.pdf?fileId=8ac78c8c7cdc391c017d073e054f6227
5. E series of preferred numbers (IEC 60063) — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/E_series_of_preferred_numbers
6. ESP32-C5 PCB Layout Design — Espressif, accessed 2026 — https://docs.espressif.com/projects/esp-hardware-design-guidelines/en/latest/esp32c5/pcb-layout-design.html
7. TA1102A SAW Filter 915 MHz — TAI-SAW Technology, Rev. 2 — https://www.rfmw.com/datasheets/taisaw/TA1102A%20_Rev.2.0_.pdf
8. 2450BP14D0100001T 2.45 GHz Band Pass Filter, Rev. 3.0 — Johanson Technology, 2025 — https://www.johansontechnology.com/docs/4650/BandPassFilter-2450BP14D0100001T-Rev3.0.pdf
9. 2450BP15B0100001E Band Pass Filter — Johanson Technology, accessed 2026 — https://www.johansontechnology.com/docs/4601/BandPassFilter-2450BP15B0100001E.pdf
