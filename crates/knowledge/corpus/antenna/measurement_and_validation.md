---
title: Antenna measurement and validation, VNA, chamber, gain transfer and yield
domain: antenna
tags: measurement, validation, VNA, network analyser, network analyzer, SOLT, SOL, open short load, calibration, port extension, electrical delay, reference plane, semi-rigid, pigtail, return loss, anechoic chamber, open site, OATS, compact range, CATR, near field, gain transfer, gain comparison, substitution, standard gain horn, SGH, three antenna method, uncertainty, far-field distance, Wheeler cap, efficiency measurement, tolerance, yield, Monte Carlo, Touchstone
aliases: đo anten, máy phân tích mạng, hiệu chuẩn VNA, mặt phẳng tham chiếu, kéo dài cổng, buồng không phản xạ, đo độ lợi, anten chuẩn, độ không đảm bảo đo, dung sai, tỷ lệ đạt sản xuất, so sánh mô phỏng và đo, how do I measure my antenna, why does my VNA reading change, how accurate is a gain measurement, can I measure without a chamber
verified: 2026-11-02
---
# Antenna measurement and validation, VNA, chamber, gain transfer and yield

## VNA calibration for an antenna on a board

Calibrate with open, short and load over a frequency range that contains the band [1]. With a kit, the reference plane ends at the connector, so a soldered cable is still in the measurement [1]. For in-device work, TI describes on-board standards [2]:

| Standard | How to build it |
|---|---|
| Open | Solder the shield to board ground near the feed, inner conductor free |
| Short | Inner conductor to shield, a 0 ohm SMD jumper works |
| Load | Two 100 ohm SMD resistors in parallel, or one 49.9 ohm |

This is not highly precise, especially above 2 GHz, because such standards are not ideal. It is still adequate for in-device antenna matching [2].

Mounting matters. Isolate the antenna from the radio, solder the shield first, and keep the bare inner conductor short to avoid extra inductance [1]. Solder the outer as close as possible to the cable end [1]. Do not add or remove cables or adapters after calibration [2]. Tie the VNA cable in a fixed position, because placement changes the result [1]. Reconnecting a load several times, rotating it, shows connector repeatability (rule of thumb).

## Return loss and impedance: reference plane pitfalls

Return loss needs only the magnitude of reflection, so no reference-plane shift is required [1]. Impedance needs it, because cable length rotates the trace on the Smith chart [1, 2].

Worked example: a 50 mm semi-rigid cable, velocity factor 0.7, has a round-trip delay of 0.476 ns. At 2.45 GHz this is 420 deg of rotation, more than a full turn of the Smith chart, even though S11 in dB is almost unchanged. An impedance read before correction is meaningless.

Correct it with electrical delay or port extension. With the cable end shorted, adjust until the trace sits at the left of the chart; then check that the open sits at the right, and average if they differ slightly [1]. TI's 868 MHz example ended at 32.82 ps [2]. Open and short delays differ, even for precision kits [2].

Know what the tool does [3]:

- Port extension corrects phase only, not loss or fixture mismatch.
- Use de-embedding when fixture loss exceeds about 0.2 dB, or fixture return loss is below about 25 dB.
- Electrical delay on some instruments changes only the display and not exported data. Check before saving a Touchstone file.

## Common return loss measurement mistakes

| Mistake | Effect | Fix |
|---|---|---|
| Cable on the antenna side, moved during test | Trace changes [1] | Fix cables, route away, choke |
| Measuring on the bench only | Plastic and a hand lower resonance [1] | Measure in the final case, in the real position [1] |
| Lying on the table | Table detunes the antenna [1] | Support on low-loss foam [1] |
| Antenna connected to the radio | Wrong load in the path [1] | Disconnect from the circuit [1] |
| Stale calibration or wide sweep | Poor interpolation | Recalibrate on the actual band |
| Using an unverified adapter | The largest error in one CST study [4] | Characterise or remove it [4] |
| Trusting low S11 alone | Loss can hide as match | Measure radiated power too |

Ferrites reduce currents running on the outside of the cable, but TI notes that position changes the result, and that small grounds make this worse, so a certain inaccuracy remains [1]. A quick relative check without a chamber: transmit a stepped carrier and read power on a spectrum analyser with a flat receiving antenna. TI's example shows about 2 dB variation over the 2.4 GHz band. It shows whether the antenna peaks mid-band, but it does not give an absolute level [1].

## Chamber, open site and compact range choices

| Range | Best for | Limits |
|---|---|---|
| Anechoic chamber | Most above 1 GHz, controlled and shielded [5] | Costly, size-limited [5] |
| Ground reflection range | 0.1 to 1 GHz gain, moderately broad patterns [5] | Needs a flat site, weather and EMI [5] |
| Elevated or slant range | Long distance, low-side-lobe source [5] | Needs open space, and reflections |
| Compact range (CATR) | Large AUT in a short distance [5] | Precision reflector 3 to 4 times the AUT [5] |
| Near-field scan | Most compact, full 3D data [5] | Probe at least 3 wavelengths away, software, phase needed [5] |
| In situ | Mounted systems below 0.1 GHz or complicated platforms [5] | Environment-specific [5] |

Absorber reflection is as low as -50 dB at normal incidence, and about -25 dB at 70 deg incidence [5], so a small chamber has worse reflections from walls at grazing angles. A CATR's quiet zone is typically 50 to 60% of the reflector, with less than 1 dB of ripple and taper as an acceptable level [5].

Measure very small antennas on their real board, with the feed cable treated carefully. If no range is available, a large outdoor site gives relative pattern data, but treat its gain as indicative and quote it as such.

## Far-field distance limits in practice

Both antennas must be in each other's far zone, so use the larger limit of the two: Rmin = 2*D^2 / lambda, for a maximum phase error of 22.5 deg [5]. D is the largest dimension of the radiating structure, which on a small device includes the ground.

Worked example:

| Antenna | Frequency | D | 2D^2/lambda |
|---|---|---|---|
| Horn, 2.4 lambda aperture | 10 GHz | 72 mm | 0.35 m |
| Patch on 100 mm board | 2.45 GHz | 100 mm | 0.16 m |
| Reader panel | 915 MHz | 300 mm | 0.55 m |
| Two-element array, 300 mm | 6.5 GHz | 300 mm | 3.9 m |

For low-gain small antennas the formula gives an unrealistically short result. Also keep several wavelengths away (rule of thumb); 3 lambda is 0.37 m at 2.45 GHz and 0.98 m at 915 MHz. A too-short range broadens the main beam, fills nulls and lowers measured peak gain. One published outdoor example used a range more than 15 times the far-field criterion, so its phase error term was zero in the budget [6]. Check the source antenna too, which is often the larger of the two.

## Gain transfer with a reference antenna

Gain comparison, also called gain transfer or substitution, needs a standard of known gain and a source of unknown gain. Measure received power from the antenna under test and then from the standard in exactly the same setup [5]:

G_AUT(dB) = G_standard(dB) + 10*log10(P_AUT / P_standard)

Both must be matched to the receiver [5], otherwise correct for mismatch. Worked example: standard 10.0 dBi, reading -42.3 dBm, against antenna under test at -46.8 dBm gives 5.5 dBi.

For circular or elliptical antennas, use two orthogonal linear standards and add the linear gains: G = 10*log10(Gv + Gh) [5]. Horns are common standards because gain closely follows theory [7].

Absolute methods use Friis instead. Two identical antennas give 0.5*(20*log10(4*pi*R/lambda) + 10*log10(Pr/Pt)) [5]. Three different antennas give all three gains: G1 = 0.5*(A + B - C), where A, B and C are the measured pair sums [5]. They need accurate distance and matched antennas, but no standard. Directivity comes from the integrated pattern, and radiation efficiency is gain divided by directivity [5]. MIT Lincoln Laboratory compared gain comparison, Wheeler cap and input admittance methods for patch efficiency [8].

## Typical gain measurement uncertainty

One published budget for a 1.2 m reflector at 7.5 GHz on an outdoor range lists standard uncertainties [6]:

| Term | Gain (dB) |
|---|---|
| Gain standard (certificate 0.570 at k = 2) | 0.285 |
| Source polarisation purity | 0.161 |
| Impedance mismatch | 0.157 |
| Multipath, gain standard | 0.080 |
| Connection repeatability | 0.058 |
| Multipath, antenna under test | 0.033 |
| Random errors | 0.032 |

Worked example, root sum of squares of these: 0.38 dB, or about 0.76 dB at k = 2 (the article's own total could not be read). The gain standard dominates. Side-lobe errors are larger: multipath of 1.127 dB at a -30 dB side lobe [6].

Mismatch worked example: with both ends at -15 dB, |Gamma1*Gamma2| = 0.032, a limit of about +/-0.27 dB. With both ends at -10 dB, 0.1 gives +0.83 to -0.92 dB. On ordinary small-antenna setups, treat 1 dB agreement with simulation as a good outcome (rule of thumb), and expect more from cable effects on small grounds. Report a measured gain with its k value.

## Comparing measured results with simulation

Compare the same quantity under the same conditions. Agree on:

- Reference plane: move the simulated port to the measurement plane, or de-embed the cable.
- Port impedance: convert both to 50 ohm.
- Geometry: include the connector, cable, board, and housing as built.
- Definition of gain: realized or not, total or partial, and linear or CP.

Order of comparison: first the Smith chart, since it reveals both reference and resonance errors; then S11 magnitude; then pattern cuts in two planes; last, absolute gain. Compare a resonance shift against the band rather than as a percentage. A CST example improved the average linear S-parameter error from about 0.11 to about 0.01 to 0.02, mainly by accounting for the adapter [4].

Build and measure more than one unit. Differences between units show the tolerance spread, and only their mean should be compared with the nominal simulation. One published study attributes a shortfall to manual assembly of the probe feed rather than dimension tolerance, because its statistical analysis gave high yield anyway [9].

## Tolerances and yield targets

Yield is the fraction of built units that satisfy the specification [9]. Inputs should be honest process spreads. Examples:

| Input | Published value |
|---|---|
| Finished 1.6 mm board thickness | +/-0.15 mm [10] |
| Controlled impedance | +/-10% typical, +/-5% tighter and costs more [10] |
| Planar antenna study, dimension spread | 0.03 mm sigma Gaussian, 0.05 mm uniform [9] |
| RO4003C process Dk | +/-0.05 (see the materials note) |

Across that study's six designs, Gaussian yield ranged from 75.5% to 100% [9], so a good nominal response does not guarantee yield.

Sample count sets accuracy. Worked example: the standard error of a yield estimate is sqrt(p*(1-p)/N). With p = 0.95: N = 50 gives +/-3.1%, N = 200 gives +/-1.5%, and N = 1000 gives +/-0.7%, one sigma. No universal published yield target for antennas was found. Use a limit set by the cost of a failure; the Studio tool reports whether the lower 95% bound clears 90%, a sensible default for a low-cost device. Worst-case corners are simpler, but they usually overstate the spread of a process.

## Applying this in Sense Sim

Before building: run antenna_converge, and set tolerance limits with antenna_yield, giving 1 sigma values (rel or abs in metres) for the variables that matter. Use design_field_yield when the concern is keeping link range rather than holding S11 under a line. Use antenna_sweep for corner studies of er, L and h, and antenna_get to record the exact model measured against.

After measuring: save a Touchstone file from the VNA with the reference plane stated, and pass it to design_fit_measured with the model. It reports frequency scale, extra loss, the permittivity that would explain the difference, and the error before and after. Use design_integrate if the case or battery was absent in the model. Treat a fitted permittivity as a model parameter, not as a material measurement, unless geometry was verified.

Keep labels honest: say whether a stated number is simulated or measured, which solver produced it, whether the mesh was converged, and with what uncertainty. antenna_assess only judges simulated results against antenna_spec, and is not a substitute for a hardware check.

## Sources

1. Antenna Selection Guide, AN058 / SWRA161B — Texas Instruments, undated — https://ti.com/lit/swra161
2. Antenna Impedance Measurement and Matching, SWRA726 — Texas Instruments, 2022 — https://ti.com/lit/swra726
3. What is the port extension feature on a VNA — RF Essentials, undated — https://rfessentials.com/rf-knowledge-base/what-is-the-port-extension-feature-on-a-vna-and-when-should-i-use-it/
4. Symbiosis: Measurement and Simulation in Modern Device Design (M. Rütschlin) — CST / ARMMS, 2012 — https://www.armms.org/media/uploads/04_armms_apr12_mruetschlin.pdf
5. Lecture 8: Basic Methods in Antenna Measurements (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L08_Measure.pdf
6. Measurement Uncertainties in Outdoor Far-Field Antenna Ranges (Barry, Betjes, Kim) — In Compliance Magazine, 2023 — https://digital.incompliancemag.com/issue/december-2023/measurement-uncertainties-in-outdoor-far-field-antenna-ranges/
7. Lecture 18: Horn Antennas (N. Nikolova) — McMaster University, 2025 — https://www.ece.mcmaster.ca/faculty/nikolova/antenna_dload/current_lectures/L18_Horns.pdf
8. Experimental comparison of the radiation efficiency for conventional and cavity backed microstrip antennas — MIT Lincoln Laboratory, 1996 — https://www.ll.mit.edu/r-d/publications/experimental-comparison-radiation-efficiency-conventional-and-cavity-backed
9. Unsupervised Surrogate-Assisted Synthesis of Free-Form Planar Antenna Topologies for IoT Applications — arXiv, 2026 — https://arxiv.org/pdf/2603.03802
10. Designer's guide for Printed Circuit Board tolerances, AP521 — Polar Instruments, undated — https://www.polarinstruments.com/support/stackup/AP521.html
