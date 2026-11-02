---
title: Decibels, EIRP and link budgets for indoor radio
domain: rf
tags: dB, dBm, dBi, dBd, dBm/MHz, EIRP, ERP, link budget, FSPL, free space path loss, fade margin, range, transmit power, regulatory limit, FCC, ETSI, CEPT, UWB power, RFID power, RAIN
aliases: quỹ đường truyền, ngân sách đường truyền, công suất phát, công suất bức xạ, suy hao không gian tự do, độ lợi anten, biên độ dự phòng, giới hạn công suất, link margin, power budget
verified: 2026-11-02
---
# Decibels, EIRP and link budgets for indoor radio

## Decibel units used in radio link budgets

The dB is a power ratio: dB = 10 log10(P1/P2). dBm is absolute power against 1 mW: 0 dBm = 1 mW, 30 dBm = 1 W, -30 dBm = 1 uW. dBi is antenna gain against an isotropic radiator; dBd is gain against a half-wave dipole, and 0 dBd = 2.15 dBi.

| Change | Power ratio |
|---|---|
| +3 dB | about x2 |
| +6 dB | about x4 |
| +10 dB | x10 |
| +20 dB | x100 |

Rules for combining them: add gains and subtract losses in dB; never add two dBm values (two equal signals sum to only +3 dB). dBm/MHz is power spectral density, and total power = density + 10 log10(bandwidth in MHz). For a 499.2 MHz UWB channel that term is 26.98 dB (computed).

## Free-space path loss at the Sense Sim radio frequencies

Free-space path loss (FSPL): L = 20 log10(d_m) + 20 log10(f_MHz) - 27.55 dB. P.1238 uses the rounded form 20 log10(f_MHz) - 28 dB for its 1 m reference [1]. Doubling distance adds 6 dB; doubling frequency adds 6 dB.

| Frequency | Use | Wavelength | FSPL 1 m | FSPL 10 m |
|---|---|---|---|---|
| 920.5 MHz | UHF RFID | 32.6 cm | 31.7 dB | 51.7 dB |
| 2440 MHz | BLE, WiFi 2.4 | 12.3 cm | 40.2 dB | 60.2 dB |
| 5180 MHz | WiFi 5 GHz | 5.8 cm | 46.7 dB | 66.7 dB |
| 6489.6 MHz | UWB channel 5 | 4.6 cm | 48.7 dB | 68.7 dB |
| 8000 MHz | UWB upper range | 3.7 cm | 50.5 dB | 70.5 dB |

Values are computed from the formula. The frequency term assumes isotropic antennas at both ends, so a fixed-gain antenna captures less power at higher frequency. That is why UWB at 6.5 GHz starts 17 dB behind UHF RFID at 1 m.

## The link budget equation and link margin

Received power: Prx(dBm) = Ptx + Gtx + Grx - Lpath - Lwalls - Lcable - Lpol. Link margin = Prx - receiver sensitivity. A link needs margin at least equal to the fade allowance for the required reliability.

Calculation order an engineer should follow:
1. Take EIRP = Ptx + Gtx - Lcable and check it against the regional limit.
2. Compute path loss with a model suited to the site, not FSPL by default in cluttered space.
3. Subtract each wall on the direct path, using frequency-specific loss.
4. Subtract the shadowing and fading allowance.
5. Solve for the distance at which margin reaches zero.

Do not count losses twice: P.1238 distance coefficients already include an implicit allowance for walls and obstacles on the floor [1]. Use either a high exponent or explicit walls with a low exponent, never both.

## How much range a change in dB buys

Range scales as 10^(delta_dB / (10 n)), where n is the path-loss exponent (computed from the log-distance model).

| Extra link margin | n = 2 (free space, open hall) | n = 3 | n = 4 (obstructed) |
|---|---|---|---|
| +3 dB | x1.41 | x1.26 | x1.19 |
| +6 dB | x2.0 | x1.58 | x1.41 |
| +10 dB | x3.16 | x2.15 | x1.78 |
| -12 dB | x0.25 | x0.40 | x0.50 |

Practical reading: in open, line-of-sight space extra antenna gain is very effective. Across closed rooms the same 6 dB gains only about 40% range, so adding an infrastructure device usually beats adding power. A wall loss behaves like negative margin: a 12 dB wall cuts free-space reach to a quarter.

## Fade margin for a target coverage probability

Shadow fading is log-normal: Gaussian in dB with standard deviation sigma, and P.1238 gives this form for indoor loss [1]. The allowance for a location-reliability target is z x sigma, with z from the normal distribution.

| Target reliability | z | sigma 4 dB | sigma 6 dB | sigma 8 dB |
|---|---|---|---|---|
| 50% | 0 | 0 dB | 0 dB | 0 dB |
| 90% | 1.28 | 5.1 dB | 7.7 dB | 10.3 dB |
| 95% | 1.645 | 6.6 dB | 9.9 dB | 13.2 dB |
| 99% | 2.33 | 9.3 dB | 14 dB | 18.6 dB |

Values are computed. A mean-based range is a 50% reliability edge, so half the positions at that distance fail. Plan infrastructure spacing from the 90-95% figure. Small-scale fading is additional for narrowband links; see the multipath document for Rayleigh and Rician allowances.

## FCC power limits for 2.4 GHz and 902-928 MHz transmitters

US rule 47 CFR 15.247, checked 2026-11-02. For frequency-hopping systems in 902-928 MHz with at least 50 hopping channels, peak conducted output is limited to 1 W (30 dBm). In 2400-2483.5 MHz, hoppers with at least 75 non-overlapping channels may use 1 W, and other hoppers 0.125 W. If antenna gain exceeds 6 dBi, conducted power must drop dB for dB by the excess, with a separate relaxation only for fixed point-to-point links [2].

Result for a UHF RFID reader in the USA: maximum EIRP is 30 + 6 = 36 dBm (4 W). A 9 dBi antenna therefore requires at most 27 dBm conducted, and a 12 dBi antenna at most 24 dBm, before cable loss credit (computed from [2]). Using a higher-gain antenna improves receive sensitivity and beam shape, but it does not raise legal EIRP.

## UHF RFID power limits by region

The RAIN Alliance spectrum chart, 2026, checked 2026-11-02, is an industry summary, not law [3]. ERP converts as EIRP = ERP + 2.15 dB, so 2 W ERP = 35.2 dBm EIRP and 4 W ERP = 38.2 dBm EIRP.

| Region | Band (MHz) | Limit |
|---|---|---|
| USA, Canada | 902-928 | 4 W EIRP |
| Europe lower / upper | 865-868 / 915-921 | 2 W ERP / 4 W ERP |
| China | 920-925 | 2 W ERP |
| South Korea | 917-923.5 | 4 W EIRP |
| Japan | 916.7-920.9 | 4 W or 0.25 W EIRP |
| Australia | 918-926 | 4 W or 1 W EIRP |

Vietnam: the chart has no row for it. A 2017 compliance update reports Circular 46/2016 moving UHF RFID from 920-925 MHz to 918-923 MHz, with existing approvals valid until expiry [4]. The current Vietnamese power limit was not verified here; confirm it in the current Ministry circular before quoting it.

## UWB power limits and resulting transmit budget

US rules 47 CFR 15.517 (indoor) and 15.519 (handheld) limit 3100-10600 MHz emissions to -41.3 dBm/MHz average EIRP and 0 dBm peak in 50 MHz, checked 2026-11-02 [5][6]. The 15.517 rule also bars outdoor-mounted antennas and aiming emissions out of the building [5]. In Europe, ECC/DEC/(06)12 identifies 6-8.5 GHz for long-term UWB at -41.3 dBm/MHz mean and 0 dBm peak in 50 MHz [7]; check later ETSI and ECC updates for enhanced indoor categories, which were not verified here.

Total power across a 499.2 MHz channel: -41.3 + 26.98 = -14.3 dBm (computed). Against a -95 dBm first-path sensitivity at 6489.6 MHz this leaves 80.7 dB, of which 48.7 dB is spent in the first metre. The remaining 32 dB gives about 40 m in free space. A single 10 cm concrete wall consuming 15-25 dB therefore collapses that reach to about 2-7 m (computed; see the material attenuation document).

## Worked link budgets for BLE and UHF RFID

BLE with Sense Sim defaults: rssi_at_1m -59 dBm, exponent 2.2, sensitivity -95 dBm. The margin at 1 m is 36 dB, so mean reach = 10^(36/22) = 43 m, then capped by max_range_m at 30 m. For 95% reliability with sigma 4 dB, subtract 6.6 dB: 29.4 dB gives 22 m. Add a 12 dB wall: 17.4 dB gives 6 m (computed).

UHF RFID, rfid-reader to rfid-tag: reader 30 dBm + 9 dBi, tag 2 dBi, tag sensitivity -18 dBm. Forward budget = 30 + 9 + 2 + 18 = 59 dB; at 920.5 MHz this equals FSPL at about 23 m. Reverse condition: 46 - 2L >= -85 dBm, so L <= 65.5 dB, which is longer. The tag power-up link limits range. The profile cap of 12 m is hit first, leaving 5.7 dB forward margin at 12 m on boresight (computed).

## Applying this in Sense Sim

Call rtls_list_profiles to read tx_power_dbm, antenna_gain_dbi, sensitivity_dbm, rssi_at_1m, path_loss_exp, shadow_sigma_db and max_range_m before estimating any range. Calculate the budget by hand, then confirm with rtls_coverage (tech, height, cell) and rtls_run (seconds). rtls_auto_deploy accepts anchor_tx_dbm_mhz, capped at -41.3, to plan with reduced UWB power; 12 dB less gives a quarter of the free-space reach.

The RFID profiles rfid-reader (39 dBm EIRP) and rfid-portal (42 dBm EIRP) exceed the 36 dBm US figure and every limit in the region table. Treat their read ranges as optimistic and say so when reporting regulatory-sensitive designs. Tools select a profile by id; none edits radio parameters, so custom values require a change to the device JSON by the user.

## Sources

1. Recommendation ITU-R P.1238-13, Propagation data and prediction methods for the planning of indoor radiocommunication systems — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.1238-13-202509-I!!PDF-E.pdf
2. 47 CFR 15.247, Operation within the bands 902-928 MHz, 2400-2483.5 MHz and 5725-5850 MHz — US eCFR via Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.247
3. RAIN RFID Global Radio Spectrum Allocations — RAIN Alliance, 2026 — https://therainalliance.org/wp-content/uploads/2026/04/RAIN-RFID-Spectrum-Allocation-042826-FINAL-4.pdf
4. Regulatory update: Vietnam — Kiwa / Product Compliance Specialists, 2017 — https://careers.kiwa.com/globalassets/uk/pcs/regulatory-updates-2017/regulatory_update_vietnam_14-feb-17.pdf
5. 47 CFR 15.517, Technical requirements for indoor UWB systems — US eCFR via Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.517
6. 47 CFR 15.519, Technical requirements for hand held UWB systems — US eCFR via Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.519
7. ECC/DEC/(06)12, Supplementary regulatory provisions for UWB — CEPT ECC, 2011 amendment — https://docdb.cept.org/download/1597
