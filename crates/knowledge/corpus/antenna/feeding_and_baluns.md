---
title: Antenna feeds, baluns, common-mode currents and circular polarisation feeds
domain: antenna
tags: balun, sleeve balun, bazooka balun, folded balun, half-wave balun, 4:1 balun, ferrite choke, common mode, cable current, feed cable, coax, probe feed, inset feed, aperture coupled, proximity coupled, stacked patch, microstrip feed, stripline, coax to waveguide, backshort, T-match, gamma match, off-centre feed, circular polarisation feed, CP, 90 degree hybrid, nearly square patch, truncated corner, quadrature, Wilkinson
aliases: bộ balun, cân bằng không cân bằng, dòng chung trên cáp, cuộn chặn ferrit, cáp đồng trục ảnh hưởng anten, cấp nguồn đầu dò, cấp nguồn khe, cấp nguồn vi dải, chuyển tiếp đồng trục, phân cực tròn từ cấp nguồn, cáp bức xạ, why does touching the cable change S11, do I need a balun, how to feed a dipole with coax, how to make a patch circularly polarised
verified: 2026-11-02
---
# Antenna feeds, baluns, common-mode currents and circular polarisation feeds

## Why a balanced antenna on coax needs a balun

A dipole connected straight to coax can show worse reflections than the impedance numbers predict, even with a 75 ohm cable on a 73 ohm dipole. Nikolova calls this field mismatch [1]. The current and field patterns of the coax and of the wire do not line up. The arms then carry unequal currents, and current flows on the outside of the shield, which radiates [1].

The effects are a tilted pattern with filled nulls, a changed input impedance, and readings that move when someone touches the cable. A balun is the device that restores equal arm currents and blocks the shield current [1].

Monopole, patch, PIFA and IFA types are unbalanced, since they are referenced to ground, and connect to a coax or microstrip directly. They still carry common-mode current if the ground is small. Dipole, folded dipole, loop, bow-tie, Yagi and LPDA are balanced, and need a balun when fed from coax.

## Balun types and when to use each

| Balun | Ratio | Bandwidth | How it works |
|---|---|---|---|
| Sleeve (bazooka) | 1:1 | Narrow | Sleeve over the coax, shorted lambda/4 from the feed, presents a high impedance to shield current [1] |
| Folded (split coax) | 1:1 | Narrow | Extra lambda/4 coax section balances the two currents; easier to make [1] |
| Half-wave coax | 1:4 | Narrow | A lambda/2 section inverts the voltage, so the load sees 4 times the impedance; used with folded dipole [1] |
| Tapered line | Varies | Wide, needs several wavelengths of taper [1] | Gradual transition from unbalanced to balanced |
| Ferrite bifilar transformer | 1:1 and others | Up to 10:1 [1] | High-permeability core makes the shield path high impedance; lossy, usually below UHF [1] |

Worked example of a lambda/4 air-filled sleeve: 82 mm at 915 MHz, 31 mm at 2.45 GHz, 11.5 mm at 6.5 GHz. Fringing at the open end shortens the required length as the sleeve gets wider [2]. A half-wave balun in polyethylene coax (velocity factor about 0.66, rule of thumb) is 108 mm at 915 MHz.

For over an octave, use a tapered line or a ferrite transformer.

## Common-mode current and feed cable effects on small antennas

The outer surface of a feed cable can carry current, which radiates and distorts a measurement, and also changes the impedance measured at the cable input [2]. For unbalanced antennas that current can be much larger than for balanced ones, and the effect grows as the device gets smaller in wavelengths [2]. Some small MF and HF antennas effectively rely on the cable to radiate [2].

TI gives the same warning for boards: ground planes that are a fraction of a wavelength carry larger currents, which makes reflection readings less stable, and cable position then changes the result [3]. The board and the cable form a dipole that nobody intended.

Signs of cable radiation in a real test:

- S11 changes when the cable moves or is touched.
- Gain or pattern depends on cable routing.
- A simulation with no cable resonates differently from the measurement.

Route the cable away from the antenna, leaving by the opposite end as in Johanson's tuning procedure [4], and tie it down. A simulated board with no cable is the idealised case.

## Chokes for measurement: sleeves against ferrite

A sleeve balun on the measurement cable is a shorted series stub with Zs = Z0 * tan(beta*l), where Z0 is set by the sleeve-to-cable radius ratio [2]. It works near one frequency. A higher Z0 widens it, but a larger sleeve becomes a scatterer that disturbs the field around the device [2]. FDTD results reported by Antenova show isolation stable for sleeve-to-cable radius ratios above 3, and absent at ratio 1 [2].

Ferrite beads are simpler, and work across a wider band, but they are lossy at higher frequencies and reduce the apparent gain of the device under test [2]. Dissipative chokes can restore pattern and polarisation purity while lowering gain and efficiency [2]. TI also finds that bead position along the cable changes the result, so accept some inaccuracy [3].

Position matters: attach the sleeve where the device has a current maximum, as it has no effect at a current null [2]. A point of high impedance on a ground has less effect than a low-impedance one [2]. Decision: use a sleeve for narrowband gain and efficiency measurement. Use ferrites for quick impedance work, then confirm any gain with a sleeve or a fibre-linked source (rule of thumb).

## Patch feed comparison: probe, inset, aperture and proximity

| Feed | Bandwidth | Spurious radiation | Build | Main issue |
|---|---|---|---|---|
| Microstrip edge or inset | 1 to 3% for the patch [6] | Rises with substrate thickness [5] | One layer, circuitry on same side [6] | Notch and feed line distort the pattern slightly [6] |
| Coaxial probe | A couple of percent [6] | Low [5] | Drilled via, solder | Inductive when thick, hard to model exactly [5, 6] |
| Aperture coupling | About 10% readily [6], though [5] calls basic versions narrow | Low [5] | Slot in ground, two substrates | Hard to match [5]; microstrip feed can reduce front to back, stripline avoids it [6] |
| Proximity coupling | Up to 13% [5] | Low to moderate | Two substrates, no contact | Alignment |
| Stacked patch, shunt fed | 3 to 10% [6] | Depends on the feed | Multilayer | Both resonances within a couple of percent [6] |

Worked decision: 5 to 6 GHz coverage is 18% FBW, beyond any single-resonance feed, so use a stacked aperture-coupled patch or a printed monopole. For the 3.4% of 2.4 GHz ISM, a probe on a thicker low-loss substrate is usually enough.

## Probe inductance and thick substrates

A probe is a short wire through the substrate. A thicker substrate gives a longer probe and a larger inductance, and degrades the match [6]. The reactance is X = 2*pi*f*L. Worked example: at 2.45 GHz, 1 nH is 15.4 ohm, and 2 nH is 30.8 ohm.

Cancelling X = +30 ohm at 2.45 GHz needs a series capacitor C = 1 / (2*pi*f*X) = 2.17 pF. A small gap or disc around the probe builds this in, common for thick air patches (rule of thumb).

Choose the feed point for resistance, then deal with reactance: moving a probe mainly changes resistance. A Smith trace looping on the inductive side signals probe inductance, and retuning length will not remove it (rule of thumb).

## Inset and microstrip feeds

Edge impedance of a half-wave patch is typically a couple of hundred ohm [6]. Options to connect at 50 ohm:

| Method | Effect | Cost |
|---|---|---|
| Inset notch to the 50 ohm point | Same point as a probe, no transformer [6] | Gaps slightly distort the pattern [6] |
| Quarter-wave transformer to the edge | Simple line design | Transformer can radiate [6] |
| Wider patch | Width increase lowers edge impedance [6] | Size, possible higher modes |
| Connect elements at their 100 or 200 ohm points | 2 or 4 in parallel give 50 ohm with no transformer [6] | Array only |

The patch_inset template computes line width with the Hammerstad formula, about 3 mm for 50 ohm on 1.6 mm FR-4. Keep the feed short and include it in the model, since a wide line can radiate.

## Off-centre, T and gamma feeds for wire antennas

Feeding a dipole off-centre raises its input impedance, following Zas(h) = Zs / cos^2(k*h) for a half-wave dipole [1]. Worked example: to reach 300 ohm from 73 ohm, cos^2(k*h) = 0.243, so k*h = 60.4 deg, or h = 0.168 lambda from centre. The feed is not symmetric, and phase reversal can change the pattern if the antenna is longer than lambda/2 [1].

Symmetric alternatives are shunt matches. In a T-match, the arm spacing D increases the input impedance up to a maximum at about half the dipole length [1]. The arms radiate slightly and may alter the pattern, so keep wire separation small [1]. Shorted line sections are inductive, and a lumped capacitor may be added [1]. The gamma match is the T-match adapted for an unbalanced coax feed [1].

A sleeve monopole gets 50 ohm from the same principle: a quarter-wave monopole is about 37 ohm, and the sleeve raises this while widening the band [1]. A sleeve ratio (lm - h)/h of 2.25 gives nearly constant patterns over a 4:1 band [1]. An LPDA feeds each element out of phase with its neighbours by crossing connections on two booms [7].

## Coax to waveguide probe transitions

In a probe-fed waveguide, a probe sits about a quarter wavelength from the shorted back wall [8]. Probe depth, diameter and back-wall distance are then tuned for the best match [8]. A patent on these transitions adds that the spacing has a major effect on bandwidth, and the optimum depends on many variables, so it is generally found empirically [9].

Which wavelength? In a guide, the relevant one is the guide wavelength, lambda_g = lambda0 / sqrt(1 - (lambda0 / (2*a))^2), which is longer than free space. Worked example with a = 0.71 lambda0: lambda_g = 1.41 lambda0, so lambda_g/4 = 0.35 lambda0. At 10 GHz, that is 10.6 mm.

The horn and oewg templates use zp = 0.16 lambda0, well under that starting value, so sweep zp together with hp, the probe length, and check that S11 is still good across the band.

## Circular polarisation from patch feeds

CP needs two orthogonal modes of equal amplitude and a 90 deg phase difference [6, 10].

| Method | Dimensions and feed | AR bandwidth |
|---|---|---|
| Dual feed, square patch | Splitter (for example Wilkinson) with 90 deg delay on one arm [6] | At least 3%, limited by the phase network [6] |
| Nearly square, single feed | Sides differ: L = W*(1 + 1/Q), Q = 1/tan_eff; feed near the diagonal [6, 10] | 1 to 3% [6] |
| Truncated corners, single feed | Common in GPS L1 patches [6] | 1 to 3% [6] |
| Slits in a square patch | c = L/2.72, d = c/10 [10] | Similar, compact |
| Circular patch, two probes | 90 deg apart for TM11 [10] | Set by phase network |

Worked example: Q = 50 gives sides differing by 2%. On a 28.8 mm FR-4 patch that is about 0.6 mm, so etch tolerance matters. The splitter and delay line use board space, add loss, tend to radiate, and may degrade the pattern [6]. AR is best at broadside and degrades towards low elevation [6].

## Phase and amplitude error budget for circular polarisation

The axial ratio follows from excitation error. Worked examples for two orthogonal fields:

| Error | AR |
|---|---|
| 3 deg phase, equal amplitude | 0.5 dB |
| 6 deg phase | 0.9 dB |
| 10 deg phase | 1.5 dB |
| 20 deg phase | 3.1 dB |
| 1 dB amplitude, exact 90 deg | 1 dB |
| 3 dB amplitude, exact 90 deg | 3 dB |

The phase formula is AR = tan(45 deg + delta/2), expressed in dB as 20*log10. For a 3 dB AR limit, the combined budget is roughly 20 deg or 3 dB; a hybrid must beat this across the band.

A physical offset counts too. In the turnstile template the dipoles sit lambda/60 apart in height, a path difference of 6 deg on axis, or about 0.9 dB AR. A turnstile radiates opposite hands up and down [1]. A helix is set by its winding sense, the hand variable of helix_axial.

## Applying this in Sense Sim

Studio ports are ideal lumped gaps: neither a balun nor a coax outer surface is modelled. So simulated balanced antennas behave as if perfectly balanced, and any cable effect on a real unit appears only in measurement. Add a cable as a box-shaped metal part with design_integrate, or as a shape with antenna_import_cad, and report whether it was included.

Use antenna_sweep on feed position variables: yf on patch_probe and patch_air, y0 on patch_inset, xf on patch_circular and pifa, zp and hp on horn and oewg. Read the impedance in antenna_results, and let antenna_assess say whether resistance or reactance is the problem. Use antenna_match for lumped networks. Use antenna_optimize with axial_ratio_below on a nearly square patch. patch_probe has its probe on one axis, so single-feed CP needs a blank model with a diagonal feed.

The turnstile template has two ports and no hybrid, so state that quadrature is assumed.

## Sources

1. Lecture 11: Practical Dipole/Monopole Geometries, Matching (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L11_Match.pdf
2. The Use of Baluns for Measurements — Antenova, via DigiKey, 2010 — https://www.digikey.be/en/articles/the-use-of-baluns-for-measurements
3. Antenna Selection Guide, AN058 / SWRA161B — Texas Instruments, undated — https://ti.com/lit/swra161
4. Understanding Chip Antennas Handbook — Johanson Technology, 2023 — https://www.johansontechnology.com/docs/4565/johanson-antenna-handbook.pdf
5. Lecture 20: Microstrip Antennas, Part I (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L20_Mstrip1.pdf
6. The Basics of Patch Antennas, Updated (Orban, Moernaut) — Orban Microwave Products, 2025 — https://www.orbanmicrowave.com/wp-content/uploads/2025/08/The-Basics-Of-Patch-Antennas-Updated.pdf
7. Log-Periodic Dipole Array — antenna-theory.com, undated — https://www.antenna-theory.com/antennas/wideband/log-periodic-dipole.php
8. Waveguide to Coax Transitions — RF Essentials, undated — https://rfessentials.com/industry-news/rf-design/waveguide-coax-transitions/
9. US5148131A, Coaxial-to-waveguide transducer with improved matching — Google Patents, 1992 — https://patents.google.com/patent/US5148131
10. Lecture 22: Microstrip Antennas, Part III (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L22_Mstrip3.pdf
