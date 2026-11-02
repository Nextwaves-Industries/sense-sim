---
title: Amplifiers, noise cascade and receiver nonlinearity
domain: rf
tags: amplifier, LNA, PA, gain, P1dB, 1 dB compression, IP3, IIP3, OIP3, third-order intercept, IM3, intermodulation, noise figure cascade, Friis formula, cascaded noise figure, dynamic range, SFDR, blocking, desensitization, desense, reciprocal mixing, phase noise, ACLR, ACPR, adjacent channel leakage, receiver front end, range limit
aliases: bộ khuếch đại, khuếch đại tạp âm thấp, điểm nén 1 dB, điểm chặn bậc ba, méo xuyên điều chế, xuyên điều chế, công thức Friis, hệ số nhiễu tầng, dải động máy thu, chặn tín hiệu, giảm độ nhạy, nhiễu pha, rò kênh lân cận, cái gì giới hạn tầm phủ, what limits radio range
verified: 2026-11-02
---
# Amplifiers, noise cascade and receiver nonlinearity

## Gain and 1 dB compression point (P1dB)

Small-signal gain is the linear output-to-input ratio. A real amplifier compresses for large signals, since voltage and current run out. The 1 dB compression point is the input or output level where gain has fallen 1 dB below its linear value [1].

Always read which reference a data sheet uses: input-referred and output-referred differ by the gain minus 1 dB, as IP1dB + G - 1 = OP1dB.

Keep transmit levels below P1dB, because large signals near it cause gain loss and considerable intermodulation [1]. Keep receivers well below it, since a strong blocker near P1dB also compresses the wanted signal. For a third-order power-series amplifier, a blocker desensitises the amplifier by 3 dB at P1dB + 1.2 dB [1].

Gain itself does not improve sensitivity beyond what is needed to overcome later noise, and it worsens linearity in the stages behind it. Too much front-end gain hurts dynamic range, so a chain has an optimum gain distribution.

## Third-order intercept point (IP3) and IM3 products

Two tones at f1 and f2 produce third-order products at 2 f1 - f2 and 2 f2 - f1 which fall close to the wanted signal, typically in the adjacent channel, and so cannot be filtered [2]. In dB, the fundamental grows 1 dB per dB of input while third-order terms grow 3 dB per dB [2]. IP3 is the extrapolated point where they would be equal, quoted at input (IIP3) or output (OIP3) [2].

Formulas, per tone (from the slopes):
- IM3 relative to carrier, in dBc = 2 x (P_tone - IP3), at input or output
- Absolute IM3 at input, dBm = 3 P_tone - 2 IIP3

Worked examples (computed):

| Tone level | IIP3 | IM3 level | Below carrier |
|---|---|---|---|
| -30 dBm | 0 dBm | -90 dBm | 60 dB |
| -20 dBm | 0 dBm | -60 dBm | 40 dB |
| -30 dBm | -10 dBm | -70 dBm | 40 dB |

Note how 10 dB more input means 20 dB less clearance [2]. An amplifier with OIP3 of 30 dBm needing IM3 30 dB down must run each tone at 15 dBm (computed from the exercise in [2]).

Rule from a power series: P1dB is about 9.6 dB below IIP3 [1]. It holds only if higher orders are negligible, so measure instead for a real part.

## Cascaded IP3 of a receiver chain

Input-referred, with all values in linear mW and linear power gain [3]:

1/IIP3_total = 1/IIP3_1 + G1/IIP3_2 + G1 G2/IIP3_3 + ...

Worked example from the reference: LNA 18 dB and -5 dBm; filter -2 dB and +50 dBm; mixer -7 dB and +15 dBm; IF amp 25 dB and +10 dBm gives -7.2 dBm total [3], which recomputes to -7.17 dBm.

What it teaches:
- Gain before a stage makes that stage's linearity matter more. Each later stage is hit by a signal raised by earlier gain.
- Loss before the chain improves IIP3 dB for dB. Adding a 2 dB high-linearity filter in front raises the example to -5.2 dBm (computed), at a noise cost.
- Noise is set near the front, while linearity is often set near the back. Good designs balance them.

## Noise figure cascade: the Friis formula

Noise factor F is input SNR over output SNR, and noise figure is 10 log10 F. A passive attenuator has F = its loss L [4][5]. In a cascade with linear values [4][5]:

F_total = F1 + (F2 - 1)/G1 + (F3 - 1)/(G1 G2) + ...

So the first amplifier should have the lowest possible noise figure, with enough gain to hide later stages [4].

Worked example (computed), chain with filter 2 dB loss, LNA NF 1.5 dB and 18 dB, passive mixer 8 dB loss and 8 dB NF, IF NF 4 dB and 20 dB:

| Order | System NF |
|---|---|
| Filter then LNA | 4.17 dB |
| LNA then filter | 2.54 dB |

The filter in front costs nearly its full 2 dB. Worth it only if out-of-band blockers would otherwise overload the LNA.

Cable worked example, chip NF 6 dB (computed):

| Layout | System NF |
|---|---|
| 0.5 dB cable, then chip | 6.5 dB |
| 3 dB cable, then chip | 9.0 dB |
| Tower LNA NF 1 dB, 20 dB gain, then 3 dB cable, then chip | 1.23 dB |

Every dB of loss before the first gain stage is a dB of lost sensitivity, on top of the radio's own NF.

## Receiver dynamic range and spurious-free dynamic range

Dynamic range is the span between the weakest and strongest usable signal. Noise floor F = -174 + 10 log10(B) + NF dBm, and sensitivity = F + SNRmin [5]. Spurious-free dynamic range (SFDR) is the range between sensitivity and the two-tone level whose IM3 product equals the noise floor:

SFDR = (2/3) (IIP3 - F) - SNRmin

Worked examples:
- Razavi's example: NF 9 dB, IIP3 -15 dBm, 200 kHz and SNRmin 12 dB gives SFDR 53 dB [5]. The formula above recomputes this at 52.7 dB.
- A BLE receiver with assumed NF 7 dB, IIP3 -10 dBm, 1 MHz and SNRmin 12 dB: floor -107 dBm, sensitivity -95 dBm, SFDR 52.7 dB (computed). This is an illustration, not a chip's specification.

A useful consequence: raising IIP3 by 3 dB buys only 2 dB of SFDR. Adding bandwidth raises the floor and takes away SFDR. AGC can extend total range beyond SFDR, but SFDR is what remains when strong and weak signals occur at once, as with a gateway beside a neighbouring reader.

## Blocking and desensitization

Desensitization is a severe fall in receiver SNR caused by another nearby radio [6]. Three main mechanisms differ in where they can be fixed [6]:

| Mechanism | Origin | Fix |
|---|---|---|
| Blocking | Receiver front end overload | Filter at the receiver input |
| Transmitter noise | Transmitter's broadband noise | Filter at the transmitter only |
| Reciprocal mixing | Receiver LO phase noise mixing with a strong signal | Filter at the receiver, or a cleaner LO (rule of thumb) |

Key figure: 3 dB desensitization occurs when transmitter noise equals receiver noise at the receiver input [6]. Worked example in the R&S tutorial: 47 dBm and -150 dBc/Hz, in 3.1 kHz, puts -68 dBm of noise at the receiver input, against internal noise of -129 dBm with 10 dB NF. So 61 dB of isolation is missing [6]. Check with the same chain in your own bandwidth.

Reciprocal mixing means a strong carrier imprints the LO's phase noise onto the wanted channel (rule of thumb). No front-end filter helps when the blocker is in-band, which is why adjacent-channel performance is set by LO quality as well as by linearity.

## ACLR: adjacent channel leakage from the transmitter

ACLR is the ratio of in-band transmit power to power in the adjacent upper and lower channels [7]. Its main cause is transmitter distortion [7]. Causes of a poor reading also include low S/N and high crest factor of the test signal [7].

IM3 is the two-tone cousin of ACLR. A real modulated signal has many tones, so its leakage cannot be read directly from a single OIP3 figure, but the slope still holds: backing off output by 1 dB improves third-order leakage by about 2 dB relative to the carrier, until the noise floor takes over (follows from the IP3 slopes in [2]).

Practical consequence: the highest power setting in a data sheet usually comes with a modulation or spectrum limit. A power amplifier driven close to P1dB gains little range and raises adjacent-channel and harmonic energy. It can fail regulatory limits (see the EMC document) and interfere with its neighbours. Check the required limit in the standard for the exact radio; none was verified here.

## What really limits range in a real radio

Range in a link budget assumes the receiver sits at its data sheet sensitivity. In the field, one of these usually sets it instead.

| Limit | Sign | Fix |
|---|---|---|
| Noise floor | Range matches NF and bandwidth | Lower NF, less front-end loss, lower rate |
| Own-board noise | Conducted sensitivity good, radiated much worse | Filter supplies, shield, see EMC document |
| External interference | Worse at busy times | Channel, antenna placement, filter |
| Blockers | Fails only near a strong transmitter | Front filter, isolation, attenuation |
| Compression at short range | Fails when very close | AGC, lower transmit power |
| Antenna | Mismatch, efficiency, body and enclosure | Tune on product |
| RFID tag power-up | Tag cannot wake, reader has plenty of margin | Only more forward power or gain |
| RFID self-jamming | Own carrier leakage degrades sensitivity (rule of thumb) | Antenna return loss, isolation |
| UWB first path | Good RSSI, bad range | Geometry, not power |

Example on noise from on-board supplies: TI reports about 2 dB of RX sensitivity loss on the CC2651P3 when using the integrated DC/DC, and recommends an internal LDO instead at higher current [8]. Reader sensitivity examples: Impinj R700 -92 dBm at a stated mode [9].

## Applying this in Sense Sim

rtls_coverage treats hearing as a threshold on the sensitivity in the profile from rtls_list_profiles, with no compression, IP3, ACLR or noise model. To plan for a real front-end, estimate system NF using Friis, then shift the effective sensitivity by the difference and report the changed margin; do not silently edit it.

Use antenna_calc with thermal_noise and {bw_hz, nf_db} for the floor, and link_range with {f, tx_dbm, gt_dbi, gr_dbi, sens_dbm, margin_db} for range. Friis and IP3 cascades are not formulas there, so calculate them explicitly and show the chain.

design_coexist models desense and blocking between named radios. Its presets for blocking level and receiver NF are typical figures and are not tied to a chip, and its results read "Estimate: not validated on hardware". Pass full radio objects when the real data sheet is known, and always tell the user the numbers are estimates, particularly isolation_db, which defaults to 20 dB if not given.

## Sources

1. EECS 142 Module 15, Distortion — UC Berkeley, accessed 2026 — https://rfic.eecs.berkeley.edu/courses/ee142/pdf/module15_disto.pdf
2. ELEX 7860 RF Design: IP3 — E. Casas, BCIT, accessed 2026 — https://people.ece.ubc.ca/~edc/7860/lectures/lec17.pdf
3. How do I cascade IP3 values through a multi-stage receiver chain — RF Essentials, accessed 2026 — https://rfessentials.com/rf-knowledge-base/how-do-i-cascade-ip3-values-through-a-multi-stage-receiver-chain/
4. ELEX 7860 RF Design: Noise — E. Casas, BCIT, 2021 — https://people.ece.ubc.ca/edc/7860.jan2021/lec12.pdf
5. EE215C HO2, Noise in RF Design — B. Razavi, UCLA, 2013 — https://www.seas.ucla.edu/brweb/teaching/215C_W2013/Noise.pdf
6. RF System Aspects for SDR, a tutorial — R. Leschhorn, Rohde & Schwarz, Wireless Innovation Forum, 2011 — https://winnforum.org/assets/Proceedings/2011/2011-2a%20-%20leschhorn.pdf
7. MG3710A Vector Signal Generator Adjacent Channel Leakage Ratio (ACLR) — Anritsu, accessed 2026 — https://dl.cdn-anritsu.com/en-au/test-measurement/files/Application-Notes/Application-Note/MG3710A_EF2100.pdf
8. CC2651P3 errata, Radio_04 — Texas Instruments, 2022 — https://ti.com/document-viewer/lit/html/SWRZ120/GUID-8FCD1A63-0CC9-4EF0-A015-769E7F199FDE
9. Impinj R700 RAIN RFID Reader datasheet — Impinj, 2020 — https://www.cisper.com/datasheets/impinj/Impinj_R700_RAIN_RFID_Reader_Datasheet.pdf
