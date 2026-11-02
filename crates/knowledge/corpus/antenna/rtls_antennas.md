---
title: Antennas for RTLS, UWB, BLE AoA and UHF RFID
domain: antenna
tags: RTLS, indoor positioning, UWB, ultra-wideband, DW3000, channel 5, channel 9, group delay, phase centre, phase center, fidelity factor, antenna delay, PDoA, BLE, Bluetooth, AoA, AoD, direction finding, element spacing, phase ambiguity, RF switch, RFID, UHF RFID, reader antenna, circular polarisation, tag antenna, T-match, chip impedance, conjugate match, on-metal tag
aliases: anten định vị trong nhà, anten UWB, độ trễ nhóm, tâm pha, mảng anten góc tới, khoảng cách phần tử mảng, anten đầu đọc RFID, thẻ RFID, thẻ gắn kim loại, anten thẻ, phối hợp trở kháng chip, why does my anchor range have bias, tag on metal not reading
verified: 2026-11-02
---
# Antennas for RTLS, UWB, BLE AoA and UHF RFID

## UWB antenna requirements beyond S11

A UWB ranging antenna must preserve the pulse, not only match the band. A DW3000 supports channels at 6489.6 MHz and 7987.2 MHz [1]. Band edges in Sense Sim are 6.24-6.74 GHz and 7.74-8.24 GHz, 7.7% and 6.25% fractional. A thin patch cannot cover that span, so printed monopoles, disc monopoles and ceramic chip antennas are common.

Taoglas uses these benchmarks for its UWC.01 ceramic UWB antenna: group delay variation over theta of 100-150 ps or less is acceptable, and fidelity factor above 0.9 is the target across angle [2]. That part (5.5 x 5.5 x 2 mm) claims under 100 ps variation from 6 to 7.3 GHz, a peak gain around 4 dBi, and efficiency above 50% [2]. Treat these as vendor benchmarks, measured on that vendor's reference board. Evaluate S11, pattern, group delay versus angle and fidelity together; a matched antenna that disperses the pulse will still bias first-path detection.

## UWB group delay and phase centre effect on range error

Ranging error from an antenna delay equals the delay times the speed of light. Worked example:

| Delay | Equivalent one-way length |
|---|---|
| 1 ps | 0.3 mm |
| 50 ps | 15 mm |
| 100 ps | 30 mm |
| 150 ps | 45 mm |

The benchmark of 100-150 ps [2] therefore corresponds to several centimetres of angle-dependent bias before any multipath or noise. Pelka et al., as cited by Schroeder et al., reported up to 25 cm of UWB time-of-flight distance error attributed to antenna orientation [3]. Schroeder et al. measured angle-dependent errors in 2.4 GHz phase-based ranging across different antenna types, so the same issue is not unique to UWB [3].

Phase centre also moves with angle and frequency; it is not a fixed point on an unsymmetrical antenna. Decision guidance: for high-accuracy anchors, mount antennas in a known consistent orientation, calibrate the average delay, and do not expect calibration of a single number to remove angle-dependent bias.

## UWB antenna delay calibration workflow

UWB chips measure time at a reference inside the device, so each device needs a calibrated delay to the antenna reference point. Community discussion of the Qorvo APS014 note describes a method using devices at surveyed distances: measure a matrix of ranges, compute expected time of flight from laser-measured distances, then search candidate delay sets, keeping the best ones and perturbing them over about 100 iterations with about 1000 candidates each [4]. The worked thread uses three devices, because a two-device measurement yields only the combined delay. This is community guidance and a poster reported a sign issue in the published equation, so verify it against the current Qorvo document [4].

Separately, dual-antenna PDoA characterisation is design-specific: a forum answer on using a channel 5 antenna on channel 9 notes that a new lookup table is needed [5]. Antenna enclosure, mounting and connector changes can alter delay, so calibrate with the final mechanical build, per channel, and re-check after any change to antenna or housing.

## UWB PDoA antenna spacing

Phase Difference of Arrival (PDoA) needs the phase difference between two antennas to stay unambiguous. With spacing d, delta_phi = 360 * (d/lambda) * sin(theta) degrees. Spacing of lambda/2 gives +/-180 deg at theta = +/-90 deg, so wrap appears right at end-fire. Forum discussion by Qorvo users uses about 0.45 lambda: 16.89 mm calculated at channel 9, with about 16.75 mm reported as used successfully [6].

Worked example:

| Channel | lambda | 0.45 lambda | 0.5 lambda |
|---|---|---|---|
| 5, 6489.6 MHz | 46.20 mm | 20.79 mm | 23.10 mm |
| 9, 7987.2 MHz | 37.53 mm | 16.89 mm | 18.77 mm |

Spacing is between phase centres, so it is not necessarily the distance between part outlines. A channel 5-spaced antenna used on channel 9 was reported to work well around +/-40 deg and reasonably to about +/-60 deg, with unreliable jumps beyond [5]. These are forum reports, not a Qorvo specification. Simulate the far-field phase of the real two-antenna geometry to derive a PDoA-to-angle table.

## BLE AoA array element spacing and phase ambiguity

Bluetooth AoA estimates angle by comparing carrier phase between antennas. TI requires phase centres within lambda/2 (maximum 180 deg difference) because a larger difference can be misread, and states 61.5 mm or less at 2.44 GHz [7]. Going to half-wave spacing raises coupling, so isolation must be checked [7].

For a two-element pair, theta = arcsin(delta_phi * lambda / (2 * pi * d)) [7]. Worked example of sensitivity, 10 deg of phase error:

| d/lambda | at 0 deg | at 30 deg | at 60 deg |
|---|---|---|---|
| 0.50 | 3.2 deg | 3.7 deg | 6.4 deg |
| 0.45 | 3.5 deg | 4.1 deg | 7.1 deg |

Accuracy degrades towards end-fire. TI measured fairly linear phase versus angle from -60 to +60 deg on a bare board, with much more error beyond [7]. Plan anchor geometry so tags are mostly within +/-60 deg, and use 2D arrays for azimuth and elevation.

## BLE AoA phase centre, polarisation and RF switch

Phase centre stability matters for angle finding. TI recommends a constant phase centre; otherwise the shift with direction must be corrected in the AoA calculation [7]. A dipole has its phase centre fixed at the feed point and low radiation along its axis, reducing coupling between neighbours [7]. Its weakness is needing isolation from ground: too close and the antenna is effectively shorted, so TI used a corrugated ground [7].

RF switching: Bluetooth 5.1 uses 2 us switch slots with an option of 1 us, so the switch must switch and settle within a slot. Channel isolation directly affects efficiency, and switch loss adds to the path [7]. TI measured about 0.3 dB in its reference switch [7]. Discard samples during settling: TI drops the first 8 samples per slot [7].

Tag polarisation matters: tag orientation affects received amplitude, and reflections can bias phase. Use CP or dual-polarised elements if tag orientation is unknown, and expect room reflections to be a large real-world error source (rule of thumb). Enclosure materials also shift phase, so calibrate with the final housing.

## RFID reader antenna: circular polarised patch

UHF RFID readers often use circular polarised (CP) panels so an unknown tag rotation does not cause a deep null. A linear tag receives half the power from an ideal CP wave, -3 dB, at any rotation, compared with a matched linear pair at the right rotation (0 dB) and a deep null at a perpendicular rotation [8]. With a 3 dB axial ratio the actual loss varies between -1.76 and -4.76 dB depending on rotation (worked example).

CP from a patch needs two orthogonal modes excited with 90 deg phase difference, using either two feeds or a single feed on a perturbed shape, such as a nearly square patch or truncated corners [9]. Two-feed designs with a hybrid generally give wider AR bandwidth (rule of thumb). A turnstile (two crossed dipoles in quadrature) gives the same effect. Reader panels favour air or foam dielectric for efficiency and bandwidth because a thin laminate patch at 915 MHz would have only about 0.4% impedance bandwidth (worked example, from the 3.771 formula [10]), and worldwide RFID spans 7%. Verify AR over the band and over the beam, not only on boresight.

## RFID tag dipole and T-match to chip impedance

Most practical planar UHF RFID tags use T-matched antennas: a loop connected to a dipole with the chip on the loop, which forms a broadband match using only distributed traces [11]. The chip is modelled as Rp in parallel with Cp, which vary with absorbed power [11]. Chip reactance is large and capacitive, so the tag antenna must be inductive. Example, NXP UCODE 9 at 915 MHz: the antenna should be matched to about 15 - j231 ohm for the blade-diced part, with -24 dBm read sensitivity on a 2.15 dBi reference antenna [12].

Power transfer coefficient is tau = 4 * Rc * Ra / |Zc + Za|^2 [11]. Worked example for Zc = 15 - j231 ohm:

| Za (ohm) | tau | Loss |
|---|---|---|
| 15 + j231 | 1.000 | 0 dB |
| 15 + j220 | 0.881 | -0.55 dB |
| 15 + j240 | 0.917 | -0.37 dB |
| 15 + j190 | 0.349 | -4.58 dB |
| 15 + j270 | 0.372 | -4.30 dB |

A low-resistance, high-reactance chip makes tag matching narrow, so a material on the tag that shifts Xa by 40 ohm costs over 4 dB.

## RFID tag link budget and read range

For a passive UHF tag, power on tag forward is POTF = Ptx * Greader * Gpath, and absorbed chip power is Pabs = POTF * tau * G with tag gain G [11]. In free space Gpath = (lambda / (4 * pi * d))^2 [11]. Tag turn-on requires Pabs to reach chip threshold Pth. Forward link usually limits range for passive tags, since chip sensitivity is now the bottleneck relative to reader receive sensitivity (rule of thumb).

Worked example, free space at 915 MHz, tau = 1: EIRP assumed 36 dBm, tag gain 2.15 dBi, chip threshold -24 dBm:

| Reader polarisation and EIRP | Allowable path loss | Range |
|---|---|---|
| 36 dBm, ideal linear match | 62.15 dB | 33.4 m |
| 36 dBm, CP to linear (-3 dB) | 59.15 dB | 23.6 m |
| 33 dBm, CP to linear (-3 dB) | 56.15 dB | 16.7 m |

These are upper-bound free-space figures. A 3 dB loss reduces range by 29%. Check local regulatory EIRP limits and treat the threshold as a part-specific value; real ranges are lower because of detuning, tag material, multipath and interference.

## On-metal RFID tags

A dipole-type tag placed flat on metal stops working, because the image current in the metal is opposite and cancels the dipole, and impedance collapses. The metal must be used as the antenna ground instead. Physics of the image: a horizontal current above a conducting plane is opposed by its image, so radiation rises with height and is small at low height [13]. A quarter wavelength of height at 915 MHz is 82 mm (worked example), which is too thick for a tag, so practical on-metal tags use a thin structure designed with a ground: PIFA-like, patch-like or high-permittivity substrate designs. Nikitin et al. note that tags designed for metal objects often use T-matching [11].

Design rules: simulate the tag on the actual metal plate at the true spacing; never assume free-space tag performance carries over. Expect narrow bandwidth, since a thin structure is electrically small in height. Also expect different readability near the plate edges. Tag performance on metal cannot be inferred from free-space data, and the reverse holds too (rule of thumb).

## Applying this in Sense Sim

Floor-map coverage uses gain versus direction; it does not model group delay, phase centre shift, or polarisation mismatch from an FDTD pattern. Do not assume those are captured by a coverage result. If UWB accuracy needs a group-delay budget, add it as a separate estimate from the antenna data. In the RTLS FDTD flow, the parametric antennas cover dipole, monopole and probe patch, for UWB channels 5 and 9, BLE and UHF RFID bands, and a result can be saved to the antenna library for use on anchors.

Antenna Studio adds wider templates. Useful starting points: uwb_monopole for UWB anchors and tags, ifa_pcb for BLE nodes, patch_air and turnstile for RFID reader CP studies, mimo_patch for two-port coupling and spacing studies, and dipole or folded_dipole as a free-space tag starting point. There is no dedicated T-match tag template, so build one from a blank model. Report the assumptions: a free-space tag result does not predict an on-metal or on-carton result.

## Sources

1. DW3000 Data Sheet — Qorvo, undated — https://www.mouser.com/datasheet/2/412/DW3000_Data_Sheet-3532945.pdf
2. UWC.01 Accura UWB Ceramic Antenna Datasheet — Taoglas, undated — https://www.taoglas.com/datasheets/UWC.01.pdf
3. Investigation of Angle Dependent Errors in Phase-based Ranging with Different Antennas (Schroeder et al.) — EWSN, 2020 — https://www.ewsn.org/file-repository/ewsn2020/156_161_schroder.pdf
4. DWM3000 APS014 Calibration, forum thread — Qorvo Tech Forum, undated — https://forum.qorvo.com/t/dwm3000-aps014-calibration/15314
5. DW3120 PDoA Antenna Space, forum thread — Qorvo Tech Forum, undated — https://forum.qorvo.com/t/dw3120-pdoa-antenna-space/13898
6. Distance Between Antennas for PDoA, forum thread — Qorvo Tech Forum, undated — https://forum.qorvo.com/t/distance-between-antennas-for-pdoa/13813
7. Bluetooth Angle of Arrival (AoA) Antenna Design, TIDA029A — Texas Instruments, 2023 — https://ti.com/lit/tida029
8. Lecture 5: Polarization and Related Antenna Parameters (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L05_Polar.pdf
9. Lecture 22: Microstrip Antennas, Part III (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L22_Mstrip3.pdf
10. Topic 5: Microstrip Patch Antenna (R. Rumpf) — University of Texas at El Paso, 2018 — https://empossible.net/wp-content/uploads/2018/03/Topic-5-Microstrip-Patch-Antenna.pdf
11. Analysis of UHF RFID Tag Antennas Using an Equivalent Circuit Approach (Nikitin, Kim, Rao) — IEEE J. Radio Frequency Identification, 2026 — https://people.ece.uw.edu/nikitin_pavel/papers/JRFID_2026_antennas.pdf
12. SL3S1206 UCODE 9 Product Data Sheet, Rev. 3.5 — NXP Semiconductors, 2025 — https://www.nxp.com/docs/en/data-sheet/SL3S1206.pdf
13. Lecture 9: Linear Wire Antennas, Dipoles and Monopoles (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L09_Dipole.pdf
