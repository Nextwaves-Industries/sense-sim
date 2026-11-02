---
title: Antenna arrays, impedance matching and small-antenna limits
domain: antenna
tags: array factor, AF, phased array, grating lobe, element spacing, beam steering, Dolph-Chebyshev, Tschebyscheff, side lobe, SLL, binomial, HPBW, directivity, impedance matching, L-match, L network, pi network, stub, single stub, quarter-wave transformer, Smith chart, Bode-Fano, Fano limit, Chu limit, Chu-Harrington, Wheeler, small antenna, Q factor, ka, mutual coupling
aliases: hệ số mảng, mảng anten, búp phụ, búp lưới, khoảng cách phần tử, phối hợp trở kháng, mạch phối hợp L, dây chêm, biểu đồ Smith, giới hạn Bode-Fano, giới hạn Chu, anten nhỏ, hệ số phẩm chất, how small can an antenna be, how wide can matching get
verified: 2026-11-02
---
# Antenna arrays, impedance matching and small-antenna limits

## Uniform linear array factor and beam direction

The array factor (AF) of an N-element uniform linear array of isotropic sources, with spacing d and progressive phase beta, has psi = k * d * cos(theta) + beta, and normalised AF = sin(N*psi/2) / (N * sin(psi/2)) [1]. Total pattern is element pattern times AF [1], with theta measured from the array axis. The main beam points where psi = 0, so cos(theta0) = -beta / (k*d) [1].

Directivity approximately equals 2*N*d/lambda for a broadside array [2]; at d = lambda/2 that is N. Worked example, numerically integrated AF for d = lambda/2:

| N | HPBW | First side lobe | Directivity |
|---|---|---|---|
| 4 | 26.3 deg | -11.3 dB | 6.02 dB |
| 8 | 12.8 deg | -12.8 dB | 9.03 dB |
| 16 | 6.4 deg | -13.1 dB | 12.04 dB |

These results ignore element pattern and mutual coupling, which change real active impedance and gain. Doubling N gains 3 dB and halves the beam, with a first side lobe approaching about -13 dB. Add the element pattern for true beam shape, since a patch element also suppresses wide-angle radiation.

## Grating lobes and maximum element spacing

Extra equal-strength major lobes (grating lobes) appear when spacing is too large for the steering angle. A single main beam needs d/lambda < 1 / (1 + |beta| / (2*pi)) [1]. With beta chosen to steer the beam by theta_s away from broadside, this is equivalent to d/lambda < 1 / (1 + |sin(theta_s)|).

Worked example, maximum spacing with no grating lobe in real space:

| Scan from broadside | Max d/lambda | At 915 MHz | At 2.45 GHz | At 6.5 GHz |
|---|---|---|---|---|
| 0 deg | 1.000 | 327.6 mm | 122.4 mm | 46.1 mm |
| 15 deg | 0.794 | 260 mm | 97.2 mm | 36.6 mm |
| 30 deg | 0.667 | 219 mm | 81.6 mm | 30.8 mm |
| 45 deg | 0.586 | 192 mm | 71.7 mm | 27.0 mm |
| 60 deg | 0.536 | 176 mm | 65.6 mm | 24.7 mm |
| 90 deg | 0.500 | 164 mm | 61.2 mm | 23.1 mm |

Use the highest operating frequency, as lambda is smallest there. For planar arrays, spacing must be under lambda and preferably about lambda/2 in both axes [3]. Note that this condition is for beamforming; angle-finding has a stricter need of no phase ambiguity, which also sits at lambda/2.

## Mutual coupling trade-off in array spacing

Reducing spacing below lambda/2 avoids grating lobes but increases mutual coupling and reduces aperture. TI describes this in AoA arrays: lambda/2 phase-centre spacing is required, while coupling then reduces array efficiency, and the dipole's null along its axis can be exploited to reduce coupling between neighbouring elements [4].

Practical measures: report S21 or isolation between ports as a design parameter; simulate with all ports terminated rather than from a single-element model; and check that the scan-dependent active impedance stays matched, since real arrays can mismatch at some scan angles. An isolated element result does not predict in-array match or pattern, so embed the element in the real array before finalising dimensions (rule of thumb).

For two-port MIMO, isolation alone is incomplete: envelope correlation depends on pattern and polarisation diversity, too. Keep this distinct from angle-of-arrival arrays, where the goal is well-known phase relationships, not decorrelated signals.

## Dolph-Chebyshev array amplitude taper

For a given directivity and number of elements, the Dolph-Chebyshev broadside array achieves the lowest side-lobe level; a uniform array has high directivity but high side lobes, and a binomial array has low side lobes but poor directivity [5]. Method: represent the AF with a Chebyshev polynomial of order N-1 and choose z0 so that T(N-1)(z0) = R0, the main-to-side-lobe voltage ratio [5]. Closed form: z0 = cosh(acosh(R0) / (N-1)).

Worked example, N = 8, d = lambda/2 (weights symmetric, centre to edge; verified numerically):

| SLL | Centre to edge amplitudes | HPBW | Directivity |
|---|---|---|---|
| Uniform (-12.8 dB) | 1, 1, 1, 1 | 12.8 deg | 9.03 dB |
| -20 dB | 1, 0.875, 0.660, 0.580 | 14.2 deg | 8.84 dB |
| -30 dB | 1, 0.812, 0.519, 0.262 | 16.4 deg | 8.28 dB |
| -40 dB | 1, 0.759, 0.418, 0.146 | 18.1 deg | 7.85 dB |

Going from uniform to -30 dB costs about 0.75 dB of directivity and widens the beam by 28%. With large N and moderate side lobes, the edge taper flattens (N = 16 at -30 dB has edge weights 0.291 beside 0.317). Very low side lobes need tight amplitude and phase control (rule of thumb).

## L-section matching network equations

A two-element L network matches a real source and load. Q is fixed by the resistance ratio: Q = sqrt(Rhigh/Rlow - 1), and the series and shunt reactances have opposite signs [6]. Series reactance magnitude Xs = Q * Rlow; shunt reactance magnitude Xp = Rhigh / Q [6]. The shunt element is on the high-resistance side. Bandwidth cannot be chosen independently, because given the two resistances the network Q is set [6].

Worked example: match 10 ohm to 50 ohm at 2.45 GHz. Q = 2, Xs = 20 ohm, Xp = 25 ohm: series inductor 1.30 nH plus shunt capacitor 2.60 pF on the 50 ohm side. Check: Zin = 50 + j0 ohm. An approximate network bandwidth is f/Q, 1.2 GHz, but the antenna Q will govern.

Worked example: match 300 ohm to 50 ohm at 915 MHz, Q = 2.24, Xs = 112 ohm, Xp = 134 ohm: 19.4 nH series, 1.30 pF shunt across the 300 ohm side.

If the load is complex, first absorb its reactance into the series or shunt element. A pi network is two L networks through an intermediate resistance, so Q can be chosen; it is usually narrower-band than a single L [7]. TI recommends placing a pi network footprint near a PCB antenna feed [8].

## Stub and quarter-wave transformer matching

A quarter-wave transformer matches two real impedances with Zt = sqrt(Z0 * RL). Example: 200 ohm to 50 ohm needs Zt = 100 ohm, and it works at the frequency where the section is lambda/4 in the line (guided wavelength, not free space).

A single shunt stub match uses a line length d from the load to reach conductance 1/Z0, then a short- or open-circuited stub to cancel the remaining susceptance. Worked example, solved numerically, in guided wavelengths for Z0 = 50 ohm:

| Load (ohm) | d from load | Stub at that point | Short stub | Open stub |
|---|---|---|---|---|
| 200 | 0.176 | B = +30.0 mS | 0.094 | 0.344 |
| 200 | 0.324 | B = -30.0 mS | 0.406 | 0.156 |
| 30 + j40 | 0.292 | B = +23.1 mS | 0.114 | 0.364 |
| 30 + j40 | 0.458 | B = -23.1 mS | 0.386 | 0.136 |

Prefer the shortest total length, as bandwidth generally decreases with length. On a microstrip board, convert guided wavelengths with eeff, as lambda_g = lambda0 / sqrt(eeff). At 915 MHz, stubs on a board are long enough that lumped parts are often easier.

## Reading a Smith chart for matching moves

On an impedance Smith chart the centre is Z0, the right end is open and the left end is short. These moves follow from the geometry of the chart.

| Component | Motion |
|---|---|
| Series inductor | Clockwise on a constant-resistance circle (up) |
| Series capacitor | Anticlockwise on a constant-resistance circle (down) |
| Shunt capacitor | Clockwise on a constant-conductance circle (down) |
| Shunt inductor | Anticlockwise on a constant-conductance circle (up) |
| Lossless line | Clockwise around the centre towards the generator; one full turn is lambda/2 |

The -10 dB circle has radius |Gamma| = 0.316, and VSWR 2 has radius 0.333. Interpreting antenna results (rule of thumb): a tight small loop near the centre often signals a double-tuned response and better bandwidth. A long arc across the chart means high Q, so a single-point match will be narrow. Points above the real axis are inductive and points below are capacitive. If a resonance sits well off centre on the real axis, an L or transformer can correct it. If the trace is far from any match over the band, change the antenna rather than adding parts.

## Bode-Fano limit on matching bandwidth

Bode-Fano bounds any lossless matching network with a reactive load. For an RC or RL load, with Q the load Q, BW/f0 * ln(1/Gamma_avg) <= pi/Q [9]. Forms by load: parallel RC, pi / (R * w0 * C); parallel RL, pi * w0 * L / R; series RL, pi * R / (w0 * L); series RC, pi * R * w0 * C [9]. The limit assumes an ideal, arbitrarily complex network and a flat in-band reflection.

Worked example, the maximum fractional bandwidth:

| Load Q | S11 = -10 dB | S11 = -20 dB |
|---|---|---|
| 10 | 27.3% | 13.6% |
| 20 | 13.6% | 6.8% |
| 50 | 5.5% | 2.7% |
| 100 | 2.7% | 1.4% |

Decision guidance: asking for better match costs bandwidth in direct proportion. A high-Q antenna cannot be made broadband with any matching network without adding loss, and a practical one- or two-element network achieves less than this bound. Added loss widens apparent bandwidth but wastes power, so check efficiency.

## Chu and Wheeler limits on small antennas

An electrically small antenna fits in a sphere of radius a, with ka < 1 meaning a smaller than lambda/(2*pi), the Wheeler radian-sphere boundary [10]. For a lossless, linearly polarised antenna, Q >= 1/(ka)^3 + 1/ka [10]. The half-power impedance bandwidth is approximately 2/Q when Q >> 1 with a single resonance [11]. The bound is optimistic, as real structures do not fill the sphere uniformly and loss lowers apparent Q. Circularly polarised designs were noted to allow half the size [10].

Worked example:

| ka | Min Q | Max 3 dB BW | a at 915 MHz | a at 2.45 GHz |
|---|---|---|---|---|
| 0.2 | 130 | 1.5% | 10.4 mm | 3.9 mm |
| 0.3 | 40.4 | 5.0% | 15.6 mm | 5.8 mm |
| 0.5 | 10.0 | 20% | 26.1 mm | 9.7 mm |
| 0.75 | 3.7 | 54% | 39.1 mm | 14.6 mm |
| 1.0 | 2.0 | 100% | 52.1 mm | 19.5 mm |

Example: a tag fitting within a 25 mm diameter sphere at 915 MHz has ka = 0.24, Q >= 77, and 3 dB BW at most 2.6%, or 24 MHz. That cannot cover 865-928 MHz with a single tuned antenna at high efficiency. Include current-carrying ground in the sphere: a board used as a ground plane counts.

## Applying this in Sense Sim

Antenna Studio can study array and match effects with the two-port patch pair (mimo_patch) and turnstile, and its optimiser supports goals such as match, resonance, gain, axial ratio, side lobes and isolation, as defined in src/antenna/tools.rs. For a quick array judgement, combine element pattern with an array factor from this note first, then simulate only the promising design.

Before optimising a small antenna, calculate ka including the board, the minimum Q, and the achievable bandwidth; if the goal violates Chu or Bode-Fano, relax the goal instead of raising max_runs. Choosing a match goal such as -10 dB across a band is more realistic than asking for -30 dB at a point. If a design is high Q, expect mesh and tolerance sensitivity to grow. Template geometry itself does not include a matching network, so report whether a stated S11 is bare or includes a network.

## Sources

1. Lecture 13: Linear Array Theory, Part I (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L13_Arrays1.pdf
2. Lecture 14: Linear Array Theory, Part II (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L14_Arrays2.pdf
3. Lecture 16: Planar Arrays and Circular Arrays (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L16_Arrays4.pdf
4. Bluetooth Angle of Arrival (AoA) Antenna Design, TIDA029A — Texas Instruments, 2023 — https://ti.com/lit/tida029
5. Lecture 15: Linear Array Theory, Part III (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L15_Arrays3.pdf
6. The L Matching Network, in Microwave and RF Design III: Networks (M. Steer) — LibreTexts, undated — https://eng.libretexts.org/Bookshelves/Electrical_Engineering/Electronics/Microwave_and_RF_Design_III_-_Networks_(Steer)/06%3A_Chapter_6/6.4%3A_The_L_Matching_Network
7. ECE 6130 Pi-Matching Networks — University of Utah, undated — https://my.ece.utah.edu/~ece5321/L16.pdf
8. 2.4-GHz Inverted F Antenna, SWRU120D — Texas Instruments, 2019 — https://ti.com/lit/swru120
9. Fano-Bode Limits, in Microwave and RF Design III: Networks (M. Steer) — LibreTexts, undated — https://eng.libretexts.org/Bookshelves/Electrical_Engineering/Electronics/Microwave_and_RF_Design_III_-_Networks_(Steer)/07%3A_Chapter_7/7.2%3A_Fano-Bode_Limits
10. Chu-Harrington limit — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Chu%E2%80%93Harrington_limit
11. New Bounds on Spherical Antenna Bandwidth and Directivity: Updates to the Chu-Harrington Limits — arXiv, 2024 — https://arxiv.org/pdf/2408.07085
