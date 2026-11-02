---
title: Human RF exposure limits, MPE distance and SAR
domain: rf
tags: RF exposure, human exposure, ICNIRP 2020, FCC, OET 65, 47 CFR 1.1310, 1.1307, 2.1091, 2.1093, MPE, maximum permissible exposure, power density, SAR, specific absorption rate, 1 g, 10 g, 1.6 W/kg, 2 W/kg, safe distance, separation distance, compliance distance, RFID reader safety, UWB tag safety, body worn, portable device, mobile device, general public, occupational, reference levels, basic restrictions
aliases: phơi nhiễm RF, phơi nhiễm trường điện từ, an toàn bức xạ, giới hạn phơi nhiễm, khoảng cách an toàn, tỷ lệ hấp thụ riêng, mật độ công suất, đầu đọc RFID có an toàn không, is this reader safe, how far from the antenna, thiết bị đeo trên người
verified: 2026-11-02
---
# Human RF exposure limits, MPE distance and SAR

## Basic restrictions and SAR limits (ICNIRP 2020 and FCC)

Specific absorption rate (SAR) is power absorbed per unit mass, in W/kg. It is the quantity that the limits are built on up to 6 GHz.

| Rule | Scope | Limit | Averaging |
|---|---|---|---|
| ICNIRP general public | Whole body | 0.08 W/kg | 30 min [1] |
| ICNIRP general public | Head and torso | 2 W/kg | 10 g, 6 min [1] |
| ICNIRP general public | Limbs | 4 W/kg | 10 g, 6 min [1] |
| ICNIRP occupational | Whole body; head and torso | 0.4; 10 W/kg | Same [1] |
| FCC general population | Whole body | 0.08 W/kg | 30 min [2] |
| FCC general population | Peak spatial | 1.6 W/kg | 1 g, 30 min [2] |
| FCC occupational | Whole body; peak | 0.4; 8 W/kg | 1 g, 6 min [2] |

Above 6 GHz ICNIRP changes to absorbed power density: 20 W/m2 general public and 100 W/m2 occupational, over 4 cm2 [1]. FCC applies its SAR limits from 100 kHz to 6 GHz and evaluates portable devices above 6 GHz using MPE instead [3].

So 1.6 W/kg over 1 g and 2 W/kg over 10 g are not interchangeable numbers: a 1 g average captures a sharper hot spot. A design can pass one and fail the other.

## Power density reference levels at radio frequencies

Limits on incident power density, used when you are far enough from the antenna. Here f is in MHz, and values in W/m2 are given for general public.

| Rule | Frequency | Limit | Averaging |
|---|---|---|---|
| FCC [2] | 300-1500 MHz | f/1500 mW/cm2 = f/150 W/m2 | 30 min |
| FCC [2] | 1500 MHz to 100 GHz | 1 mW/cm2 = 10 W/m2 | 30 min |
| ICNIRP whole body [1] | 400-2000 MHz | f/200 W/m2 | 30 min |
| ICNIRP whole body [1] | 2-300 GHz | 10 W/m2 | 30 min |
| ICNIRP local [1] | 400-2000 MHz | 0.058 f^0.86 W/m2 | 6 min |
| ICNIRP local [1] | 2-6 GHz | 40 W/m2 | 6 min |
| ICNIRP local [1] | 6-300 GHz | 55 / f_GHz^0.177 W/m2 over 4 cm2 | 6 min |

Occupational limits are five times higher: FCC f/300 mW/cm2 and 5 mW/cm2, and ICNIRP whole-body f/40 and 50 W/m2 [1][2].

Computed values: at 915 MHz FCC is 6.1 W/m2, ICNIRP whole body 4.6 W/m2, ICNIRP local 20.4 W/m2. At 6.5 GHz ICNIRP local is 39.5 W/m2. ICNIRP says that reference levels do not determine compliance in the reactive near field above 2 GHz, where basic restrictions must be assessed [1].

## MPE distance formula

In the far field, power density S = EIRP / (4 pi R^2), and S = P G / (4 pi R^2) with G as a numeric gain [4]. Solving for distance: R = sqrt(EIRP / (4 pi S)).

Keep units consistent. With W and W/m2, R is in metres; with mW and mW/cm2, R is in cm [4]. Convert ERP by 1.64 to get EIRP [4].

Ground and surface reflection: for a truly worst-case prediction near a reflecting surface, assume 100% reflection, so four times the power density and twice the distance [4]. The EPA model for broadcast sites uses a more realistic factor of 2.56, which multiplies the distance by 1.6 [4].

Time averaging: use the highest time-averaged power. The duty factor is pulse duration over pulse period, and 1.0 means continuous [4]. Treat an RFID reader that is always inventorying as 1.0 unless controlled.

Far-field formula can be used near the antenna for a conservative prediction [4], although far-field equations are not accurate there. Very close, in the reactive zone (about lambda/2pi: 5.2 cm at 915 MHz, 2.0 cm at 2.4 GHz, 0.73 cm at 6.5 GHz, computed), rely on SAR or full-wave analysis.

## Worked MPE distance: UHF RFID reader at 915 MHz

Computed from the formula, boresight, duty 1.0, general public.

| EIRP | FCC, no reflection | FCC, 4 x reflection | ICNIRP whole body | ICNIRP local |
|---|---|---|---|---|
| 36 dBm (4 W) | 23 cm | 46 cm | 26 cm | 12.5 cm |
| 39 dBm (8 W) | 32 cm | 64 cm | 37 cm | 18 cm |
| 42 dBm (16 W) | 46 cm | 91 cm | 53 cm | 25 cm |

Occupational at 36 dBm: FCC 10 cm and ICNIRP 12 cm.

Example at 36 dBm: EIRP = 3.98 W; FCC limit 6.1 W/m2; R = sqrt(3.98 / (4 pi x 6.1)) = 0.228 m.

Interpretation: ICNIRP whole-body limits are averaged over the body [1], so applying the on-axis density to a person is conservative beside a narrow beam. A 25 cm patch has its far field beyond about 38 cm at 915 MHz (2 D^2 / lambda, computed), so these close distances are conservative estimates rather than measurements.

Practical guidance: keep people 50 cm or more from 4 W EIRP antennas, for example by mounting dock-door antennas out of reach. Portals where people stand close, and shelf antennas, need real checks at the actual power. Reducing EIRP by 3 dB shortens distance by 29%.

## Worked MPE distance: UWB tag

FCC 15.519 limits UWB handheld transmitters to -41.3 dBm/MHz average EIRP, and 0 dBm peak in 50 MHz [5]. Across 499.2 MHz this is -14.3 dBm, or 37 microwatts (computed).

Power density at distance, continuous transmission at the limit (computed):

| Distance | S | Share of 39.5 W/m2 ICNIRP local | Share of 10 W/m2 FCC |
|---|---|---|---|
| 2 cm | 0.0074 W/m2 | 0.02% | 0.07% |
| 10 cm | 0.0003 W/m2 | 0.0007% | 0.003% |
| 20 cm | 0.00007 W/m2 | 0.0002% | 0.0007% |

So even touching the body, exposure is orders of magnitude under the limit. The MPE distance is below 1 mm, inside the reactive zone, which is meaningless as a distance. A real tag transmits a few percent of the time, reducing this further.

FCC exempts any single source with no more than 1 mW available maximum time-averaged power, at any separation distance [6]. At 37 microwatts, a compliant UWB tag meets this, so no routine evaluation is normally needed. Check that the chip's conducted power at the antenna is also under 1 mW. This exemption concerns routine evaluation, not all rules: exposure limits still apply.

## FCC exemption thresholds and device classes

47 CFR 1.1307(b)(3) exempts single sources from routine evaluation if any of these holds [6]:

1. Time-averaged power is at most 1 mW.
2. Time-averaged power or ERP, whichever is greater, is at most a threshold Pth, for 0.5-40 cm and 0.3-6 GHz. Take the formula from the rule text rather than from memory.
3. ERP is under a Table 1 limit at separation R in metres and f in MHz, with R at least lambda/2pi: 0.0128 R^2 f watts for 300-1500 MHz; 19.2 R^2 watts for 1500 MHz to 100 GHz.

Worked: a 36 dBm EIRP reader is 2.43 W ERP. At 915 MHz, R must be at least sqrt(2.43 / (0.0128 x 915)) = 46 cm for that exemption (computed). This matches the 4 x reflection case above. 39 dBm needs 64 cm and 42 dBm needs 91 cm.

Device classes: a portable device is used with its radiating structure within 20 cm of the body [3]. A mobile device normally keeps at least 20 cm [7]. Portable devices need SAR evaluation unless exempt, by laboratory measurement or validated computational modelling [3]. Mobile devices are evaluated against MPE when above the limits [7]. Multiple sources sum their fractions of each threshold, and the total must be at most 1 [6].

## What requires testing

Decision guide for a design (rule of thumb, follow the rule text for the actual product):

| Product | Likely route |
|---|---|
| UWB tag at the emission limit | Exempt under 1 mW in the US; state in documentation |
| BLE beacon, ceiling mounted | MPE calculation or Table 1 exemption |
| Body-worn BLE or WiFi tag | Pth threshold; SAR test above it |
| Fixed RFID reader or portal | MPE calculation; state an installation distance |
| Handheld RFID reader near 1 W | SAR test is expected |
| Several radios in one device | Sum of fractions, including all that overlap in time |

EU products use harmonised standards under RED, for example EN 50566 for hand-held and body-mounted devices from 30 MHz to 6 GHz, and EN 50385 and EN 50401 for base stations [8]. A product standard for RFID, EN 50364, also exists; its current edition was not confirmed here.

The averaging time and source-based time averaging can allow duty cycle credit, but only if the device enforces it. An RF module's grant usually assumes a stated antenna gain and distance. Changing them may require new evaluation.

Simulated SAR supports a design decision; it is not a compliance test unless it is a fully validated model with the rule's procedure, and a lab measurement usually remains necessary.

## Applying this in Sense Sim

design_sar gives a SAR pre-check for an antenna template, with `template`, `f0`, `gap_mm` (default 5), `power_mw` (default 100), `axis` and `tissue`. It reports 1 g and 10 g SAR against 1.6 W/kg (FCC) and 2 W/kg (ICNIRP). These limits match the sources. Set power_mw to time-averaged power. The result supports decisions only: say that a lab must test. Tissue defaults cover about 900 MHz and 2450 MHz bands; outside these its assumptions say to give a matching tissue. Read `most_power_mw` for the power that reaches each limit. UWB above 6 GHz is outside SAR limits, so use power density there.

For MPE, get tx_power_dbm and antenna_gain_dbi from rtls_list_profiles, then use the formula above; antenna_calc converts "power" and "erp_eirp". After rtls_set_region, EIRP may be capped, so use the capped value.

rtls_coverage does not check exposure. The built-in `rfid-reader` (39 dBm EIRP) and `rfid-portal` (42 dBm EIRP) profiles give 64 cm and 91 cm for the FCC exemption. Flag them if a layout places people closer, and remind the user that these profile powers exceed common regulatory limits. Antenna gain from antenna_results can replace profile gain in the same formula.

## Sources

1. ICNIRP Guidelines for limiting exposure to electromagnetic fields (100 kHz to 300 GHz) — ICNIRP, Health Physics 118(5), 2020 — https://www.icnirp.org/cms/upload/publications/ICNIRPrfgdl2020.pdf
2. 47 CFR 1.1310, Radiofrequency radiation exposure limits — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/1.1310
3. 47 CFR 2.1093, Radiofrequency radiation exposure evaluation: portable devices — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/2.1093
4. OET Bulletin 65, Edition 97-01, Evaluating Compliance with FCC Guidelines — FCC, 1997, University of Utah copy — https://my.ece.utah.edu/~ece1270/FCCpwrLimits.pdf
5. 47 CFR 15.519, Technical requirements for hand held UWB systems — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.519
6. 47 CFR 1.1307, Actions that may have a significant environmental effect — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/1.1307
7. 47 CFR 2.1091, Radiofrequency radiation exposure evaluation: mobile devices — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/2.1091
8. Radio Equipment Directive 2014/53/EU, summary list of harmonised standards — IBF Solutions, generated 7 September 2026 — https://www.ibf-solutions.com/fileadmin/dateidownloads/amtsblaetter/radio-equipment-directive-2014-53-eu-summary-list-of-harmonised-standards.pdf
