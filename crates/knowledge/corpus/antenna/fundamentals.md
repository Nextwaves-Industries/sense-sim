---
title: Antenna fundamentals, gain, polarisation, match and far field
domain: antenna
tags: antenna, gain, dBi, directivity, realized gain, radiation efficiency, HPBW, beamwidth, polarisation, polarization, axial ratio, AR, PLF, polarisation loss, RHCP, LHCP, S11, VSWR, return loss, mismatch loss, far field, Fraunhofer, bandwidth, FBW, ground plane, monopole, Friis
aliases: độ lợi anten, độ định hướng, hiệu suất bức xạ, độ rộng búp sóng, phân cực, phân cực tròn, tỷ số trục, tổn hao lệch phân cực, hệ số phản xạ, tỷ số sóng đứng, suy hao phản xạ, trường xa, băng thông anten, mặt phẳng đất, antenna gain vs directivity, how good is my S11
verified: 2026-11-02
---
# Antenna fundamentals, gain, polarisation, match and far field

## Directivity, gain and realized gain definitions

Antenna directivity D compares radiation intensity in a direction with an isotropic radiator of the same radiated power. Antenna gain includes dielectric and conductor loss: G = eA * D, where eA is radiation efficiency. Under IEEE definitions, gain excludes impedance and polarisation mismatch [1]. Realized gain adds impedance mismatch: Gr = eA * (1 - |Gamma|^2) * D [1]. Total efficiency is et = er * ep * ec * ed (reflection, polarisation, conduction, dielectric) [1].

| Efficiency | Loss |
|---|---|
| 90% | -0.46 dB |
| 70% | -1.55 dB |
| 50% | -3.01 dB |
| 25% | -6.02 dB |

Decision guidance: compare antennas by realized gain, because a link sees mismatch loss too. A high directivity with low efficiency means power is heating the substrate or copper, not reaching the far field. Reference values: a half-wave dipole has D = 1.643 (2.16 dB) [3] and a quarter-wave monopole on an ideal ground has twice that [3].

## Half-power beamwidth and the Kraus directivity estimate

Half-power beamwidth (HPBW) is the angle between the two directions where radiation intensity falls to half of the peak; first-null beamwidth is roughly twice HPBW [1]. For a narrow single beam with negligible side lobes, Kraus gives D0 approx 41000 / (theta1 * theta2), with both orthogonal-plane HPBWs in degrees [1]. The estimate is meant for pencil beams and gets unreliable for wide patterns, so treat wide-beam results as a rough sanity check.

Worked example: 60 deg x 60 deg gives 41000/3600 = 11.4, or 10.6 dBi. 90 deg x 90 deg gives 5.06, or 7.0 dBi.

Use this to sanity-check a directional antenna entered as a peak gain and beamwidth. A claim of 12 dBi with 90 deg beamwidth in both planes is not credible, because it exceeds the estimate by about 5 dB. A wider beam in one plane reduces the achievable peak further.

## Axial ratio and circular polarisation definitions

Axial ratio (AR) is the major-to-minor axis ratio of the polarisation ellipse. AR = 1 (0 dB) is perfect circular polarisation (CP) and AR approaches infinity for linear polarisation [2]. Handedness (RHCP or LHCP) depends on rotation relative to the propagation direction [2]. With rC the magnitude ratio of the RHCP and LHCP field components, AR = (rC + 1) / |rC - 1| [2].

Worked example: AR = 3 dB is a linear ratio of 1.41, so rC = 5.83, meaning the unwanted hand is 15.3 dB below the wanted hand. AR = 1 dB corresponds to rC of about 24.8 dB.

AR varies with both angle and frequency. Quote AR with its angle and band rather than boresight alone, because a CP patch that is good on boresight can be strongly elliptical off axis. Passive RFID reader panels are commonly specified at AR below 3 dB in the band (rule of thumb); check the actual datasheet.

## Polarisation loss factor (PLF) table

Polarisation loss factor is PLF = |pw . pa|^2, the squared dot product of the unit polarisation vectors of wave and receive antenna. PLF is 1 at match and 0 at complete mismatch, and it is not dissipation [2]. Two linear antennas rotated by angle phi give PLF = cos^2(phi). A linear antenna receiving an ideal CP wave gets half the power, -3.01 dB, at any rotation. Ideal opposite-hand CP antennas give zero.

Worked example, linear antenna and elliptical wave:

| Wave AR | Best rotation | Worst rotation |
|---|---|---|
| 0 dB | -3.01 dB | -3.01 dB |
| 1 dB | -2.54 dB | -3.54 dB |
| 2 dB | -2.12 dB | -4.12 dB |
| 3 dB | -1.76 dB | -4.76 dB |
| 6 dB | -0.97 dB | -6.97 dB |

Worked example, linear to linear: 15 deg -0.30 dB, 30 deg -1.25 dB, 45 deg -3.01 dB, 60 deg -6.02 dB, 75 deg -11.74 dB. A CP reader trades a steady 3 dB for tolerance to unknown tag rotation. Multipath reflections can change polarisation, so real rooms rarely show the deep theoretical null.

## Friis link equation with all antenna losses

The Friis transmission equation in far-zone conditions, with all efficiency terms, is:

Pr / Pt = (1 - |Gamma_t|^2) * (1 - |Gamma_r|^2) * eAt * eAr * |pt . pr|^2 * (lambda / (4 * pi * R))^2 * Dt * Dr [4]

Each term is a separate engineering lever. Directivity and efficiency belong to the antenna design. The mismatch terms belong to the feed. The polarisation term belongs to the deployment orientation.

Worked example: free-space path loss at 1 m is 31.7 dB at 915 MHz, 40.2 dB at 2.45 GHz and 48.7 dB at 6.5 GHz. The higher path loss at 6.5 GHz arises because a 0 dBi antenna has a smaller effective aperture at a higher frequency. It does not arise from air loss. Indoor multipath can move measured results well away from Friis, so use it for link budget structure and not as a measured range.

## S11, VSWR, return loss and mismatch loss table

These are conversions of one quantity: |Gamma| = 10^(S11_dB/20), VSWR = (1 + |Gamma|) / (1 - |Gamma|), reflection efficiency = 1 - |Gamma|^2 [1]. Values below are computed (worked example).

| S11 (dB) | abs(Gamma) | VSWR | Power reflected | Mismatch loss |
|---|---|---|---|---|
| -3 | 0.708 | 5.85 | 50.1% | 3.02 dB |
| -6 | 0.501 | 3.01 | 25.1% | 1.26 dB |
| -9.54 | 0.333 | 2.00 | 11.1% | 0.51 dB |
| -10 | 0.316 | 1.92 | 10.0% | 0.46 dB |
| -14 | 0.200 | 1.50 | 4.0% | 0.18 dB |
| -15 | 0.178 | 1.43 | 3.2% | 0.14 dB |
| -20 | 0.100 | 1.22 | 1.0% | 0.04 dB |
| -30 | 0.032 | 1.07 | 0.1% | 0.004 dB |

Decision guidance: improving S11 from -10 dB to -20 dB gains only 0.42 dB, so stop chasing match once the band edge meets -10 dB and look at efficiency and pattern. S11 alone proves nothing: a lossy antenna or a lossy substrate can show an excellent match with poor radiated power.

## Fractional bandwidth and what RTLS bands demand

Narrowband antennas quote fractional bandwidth FBW = (fmax - fmin) / fcentre x 100%; broadband antennas quote fmax/fmin [1]. A bandwidth means nothing without its criterion, because separate impedance, pattern and axial-ratio bandwidths exist [1]. Common impedance criteria are S11 below -10 dB or VSWR below 2.

Worked example, required bandwidth for bands in Sense Sim:

| Band | Span | FBW |
|---|---|---|
| BLE 2.4 GHz | 2.40-2.4835 GHz | 3.4% |
| UHF RFID worldwide | 865-928 MHz | 7.0% |
| UWB channel 5 | 499.2 MHz at 6489.6 MHz | 7.7% |
| UWB channel 9 | 499.2 MHz at 7987.2 MHz | 6.25% |

Simple patches give at most a couple of percent [8], so a basic thin patch on a printed board cannot cover UWB channel 5 or worldwide RFID without a thicker substrate or a wideband feed. Add margin for tolerance: a design that just fits the band nominally will lose band edges in production.

## Far-field distance rule 2D^2/lambda

The usual antenna measurement far-field criterion is Rmin = 2 * D^2 / lambda, where D is the largest antenna dimension. At that distance the phase error over the aperture is about 22.5 deg (pi/8 rad) [5]. The rule is for apertures that are large compared with a wavelength. For an antenna that is small relative to a wavelength, also stay several wavelengths away (rule of thumb), because the D-based number becomes unrealistically small.

| Antenna (worked example) | Frequency | D | 2D^2/lambda |
|---|---|---|---|
| Half-wave dipole | 2.45 GHz | 61 mm | 61 mm |
| Reader patch panel | 915 MHz | 200 mm | 0.24 m |
| 8-element array at lambda/2 | 2.45 GHz | 430 mm | 3.0 m |

Implication for RTLS: a single anchor antenna is in the far field at normal room range, so a gain lookup is valid. A large array or a person holding a tag very close to an antenna is not, and a far-field pattern will not describe the near-field coupling.

## Ground plane effects on monopoles and printed antennas

A monopole on a ground plane has half the input impedance and twice the directivity of the corresponding dipole [3], so an ideal quarter-wave monopole has about 36.5 ohm and 5.16 dB (2 x 1.643). A real ground plane is finite. Broadcast monopoles use radials of 0.25-0.35 wavelength to approximate perfect ground [3]. With a smaller ground the pattern tilts away from the horizon and the impedance moves (rule of thumb), so treat infinite-ground results as optimistic.

On small devices the board is part of the antenna. TI states that ground size and shape, nearby housings and batteries, and PCB thickness and material all change the impedance of its 2.4 GHz inverted-F design, and recommends a pi network near the feed for retuning [6]. For a dipole held too close to a ground, the reflected image cancels the antenna, effectively shorting it [7]. Copying a reference antenna onto a different board size therefore requires retuning, not just copying the layout.

## Applying this in Sense Sim

The RTLS floor map uses a gain lookup, so what matters there is gain versus direction, not S11. A device can use omni, directional (peak gain plus half-power beamwidth) or a pattern saved from an FDTD run. Check any directional entry against the Kraus estimate before relying on it. A library pattern that has been deleted falls back to 0 dBi rather than an error, so re-check coverage after removing library files.

The Studio template notes assume idealised reference values: dipole about 2.15 dBi and 73 ohm, monopole about 5.15 dBi and 36 ohm on an infinite ground. Antenna Studio reports resonances, bandwidth, impedance, VSWR, gain, directivity, efficiencies, beam widths, side lobes, axial ratio and polarisation. Read these together: confirm match first, then efficiency, then pattern. Separate realized gain from gain and state the criterion when quoting a bandwidth. Far-field results are not valid for very short-range tag coupling.

## Sources

1. Lecture 4: Fundamental Antenna Parameters (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L04_Param.pdf
2. Lecture 5: Polarization and Related Antenna Parameters (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L05_Polar.pdf
3. Lecture 9: Linear Wire Antennas, Dipoles and Monopoles (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L09_Dipole.pdf
4. Lecture 6: Friis Transmission Equation (N. Nikolova) — McMaster University, 2026 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L06_Friis.pdf
5. Lecture 8: Basic Methods in Antenna Measurements (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L08_Measure.pdf
6. 2.4-GHz Inverted F Antenna, SWRU120D — Texas Instruments, 2019 — https://ti.com/lit/swru120
7. Bluetooth Angle of Arrival (AoA) Antenna Design, TIDA029A — Texas Instruments, 2023 — https://ti.com/lit/tida029
8. Lecture 20: Microstrip Antennas, Part I (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L20_Mstrip1.pdf
