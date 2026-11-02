---
title: Indoor path loss models and exponents
domain: rf
tags: path loss, FSPL, log-distance, path loss exponent, ITU-R P.1238, site-general model, 3GPP TR 38.901, InH, InF, IEEE 802.15.4a, floor penetration, breakpoint, shadow fading, LOS, NLOS, office, warehouse, factory, corridor
aliases: suy hao đường truyền, mô hình suy hao, hệ số suy hao, số mũ suy hao, suy hao trong nhà, kho hàng, nhà xưởng, hành lang, suy hao qua sàn, propagation model
verified: 2026-11-02
---
# Indoor path loss models and exponents

## Choosing a path loss model for an indoor site

Use the simplest model that matches what is known about the site.

| Situation | Model |
|---|---|
| Open line of sight, short range | FSPL, or log-distance with n near 2 |
| Site known only by type | ITU-R P.1238 site-general or 3GPP InH / InF |
| Floor plan with walls known | Low exponent plus explicit per-wall loss |
| Exact fields near structure or metal | Ray tracing, or FDTD for a local detail |

P.1238 states that site-specific ray methods need detailed building data, and that including reflected and diffracted rays significantly improves accuracy [1]. Every statistical model predicts a median across many locations, so it can be several dB wrong at any one spot. Calibrate with measurements when a design has little margin.

## Log-distance path loss model and exponent meaning

Log-distance model: L(d) = L(d0) + 10 n log10(d/d0) + X, with reference d0 usually 1 m and X a zero-mean Gaussian in dB with standard deviation sigma. The P.1238 site-specific form writes N = 10 n, adds a floor term Lf(n), and takes free-space loss at 1 m [1].

Meaning of n:
- n near 2: free-space spreading.
- n below 2: guided energy in corridors and aisles.
- n 3 to 4: the path crosses walls or clutter, which adds loss that grows with distance.

The exponent is an empirical fit, so it depends on distance range and on how walls were treated. Never copy an exponent from one source and walls from another without checking what was already included. P.1238 says its distance coefficients carry an implicit allowance for walls and obstacles on the floor [1].

## ITU-R P.1238 general conclusions for 900-2000 MHz

P.1238-13 offers these conclusions, strongest for 900-2000 MHz, expressed as the distance coefficient N = 10 n [1].

| Environment | N | Equivalent n |
|---|---|---|
| Line of sight path | about 20 | 2.0 |
| Large open rooms: retail, arenas, open-plan factories and offices | about 20 | 2.0 |
| Corridors, grocery-style long aisles | about 18 | 1.8 |
| Around obstacles and through walls, closed-plan offices | up to about 40 | 4.0 |
| Beyond first Fresnel breakpoint on long clear paths | changes from 20 to about 40 | 2 to 4 |

Actionable reading: a warehouse aisle may outperform free space along its axis, then lose heavily across racks. Model aisle-parallel and cross-aisle coverage separately rather than using one exponent.

## ITU-R P.1238 site-general formula and coefficients

Median loss on one floor: L = 10 alpha log10(d) + beta + 10 gamma log10(f) + N(0, sigma), with d in metres (3D) and f in GHz [1]. Selected coefficients, for omnidirectional use:

| Environment | Path | alpha | beta | gamma | sigma dB | d range m |
|---|---|---|---|---|---|---|
| Office | LoS | 1.47 | 34.17 | 2.08 | 3.68 | 2-27 |
| Office | NLoS | 2.39 | 30.13 | 2.40 | 5.01 | 4-30 |
| Corridor | LoS | 1.57 | 29.46 | 2.24 | 3.77 | 2-160 |
| Corridor | NLoS | 2.78 | 28.62 | 2.54 | 7.58 | 3-94 |
| Industrial | LoS | 2.27 | 24.79 | 2.10 | 2.62 | 2-102 |
| Industrial | NLoS | 2.80 | 23.55 | 2.16 | 5.70 | 3-110 |

Validity: office from 0.3 GHz; corridor NLoS and industrial from 0.625 GHz; all extend far above the bands used here. For Monte Carlo NLoS use, P.1238 passes excess loss over free space through 10 log10(10^(0.1 A) + 1), with A normal around the median excess, which keeps the excess positive [1].

## Predicted indoor path loss at 10 m by band

Median path loss at 10 m, computed from the P.1238 site-general formula [1], with FSPL for comparison.

| Environment | 920 MHz | 2.44 GHz | 5.18 GHz | 6.49 GHz |
|---|---|---|---|---|
| FSPL | 51.7 | 60.2 | 66.7 | 68.7 |
| Office LoS | 48 | 57 | 64 | 66 |
| Office NLoS | 53 | 63 | 71 | 74 |
| Corridor LoS | 44 | 54 | 61 | 63 |
| Corridor NLoS | 56 | 66 | 75 | 77 |
| Industrial LoS | 47 | 56 | 62 | 65 |
| Industrial NLoS | 51 | 60 | 67 | 69 |

Read values as median dB. These fitted LoS values fall below FSPL at 10 m, so FSPL is not a guaranteed lower bound on measured loss indoors. At 20 m, industrial NLoS loss at 2.44 GHz reaches 68 dB against 66.2 dB for FSPL. The gap between NLoS and LoS grows with distance, so short-range tests hide it. Report the distance whenever quoting an exponent.

## 3GPP TR 38.901 indoor hotspot and indoor factory models

TR 38.901 v18 covers 0.5-100 GHz, and uses f in GHz and d in metres [2].

| Scenario | Median loss (dB) | sigma dB |
|---|---|---|
| InH office LOS | 32.4 + 17.3 log10 d + 20 log10 f | 3 |
| InH office NLOS | 17.3 + 38.3 log10 d + 24.9 log10 f, not below LOS | 8.03 |
| InF LOS | 31.84 + 21.5 log10 d + 19 log10 f | about 4 |
| InF sparse clutter, low BS | 33 + 25.5 log10 d + 20 log10 f | 5.7 |
| InF dense clutter, low BS | 18.6 + 35.7 log10 d + 20 log10 f | 7.2 |
| InF sparse clutter, high BS | 32.4 + 23 log10 d + 20 log10 f | 5.9 |
| InF dense clutter, high BS | 33.63 + 21.9 log10 d + 20 log10 f | 4.0 |

The InF models treat clutter density and antenna height as primary factors [2]. Dense clutter with a low antenna has an exponent near 3.6; raising the antenna above clutter drops it near 2.2. That matches the P.1238 siting advice to mount the base station as high as practical to obtain line of sight [1].

## UWB path loss exponents from IEEE 802.15.4a

The IEEE 802.15.4a channel model covers 2-10 GHz UWB channels from measurements [3]. Fitted parameters per environment:

| Environment | n LOS | n NLOS | sigma LOS dB | sigma NLOS dB |
|---|---|---|---|---|
| Residential | 1.79 | 4.58 | 2.22 | 3.51 |
| Office | 1.63 | 3.07 | 1.9 | 3.9 |
| Outdoor | 1.76 | 2.5 | 0.83 | 2 |
| Industrial | 1.2 | 2.15 | 6 | 6 |

The report adds that LOS exponents indoors range from about 1 in corridors to 2 in offices, NLOS 3 to 4 for soft NLOS, and 4 to 7 for hard NLOS [3]. The model is channel only, with antenna effects to be treated separately [3]. Note the industrial entry: its measurements span only about 2-8 m, with the NLOS case taken from the literature over 3-8 m [3], so do not extrapolate that low n across a large hall. A low n does not mean the first path survives; UWB ranging depends on first-path power, which the total-power exponent does not capture.

## Floor penetration loss between storeys

P.1238 floor loss Lf(n), for n floors, as used in the site-specific form [1].

| Frequency | Office | Residential | Commercial |
|---|---|---|---|
| 0.9 GHz | 9, 19, 24 dB (1, 2, 3 floors) | none given | none given |
| 1.8-2 GHz | 15 + 4(n-1) | 4n | 6 + 3(n-1) |
| 2.4 GHz | 14 (one value) | 10 apartment, 5 house | none given |
| 3.5 GHz | 18, 26 (1, 2 floors) | none given | none given |
| 5.2 GHz | 16 (1 floor) | 13 apartment, 7 house | none given |
| 5.8 GHz | 22, 28 (1, 2 floors) | none given | none given |

Isolation through many floors saturates, since signals find outside paths [1]. For ray-traced models at 5.2 GHz, P.1238 gives a reinforced concrete floor with false ceiling as 20 dB mean (sigma 1.5), rising to 30 dB with lighting fixtures and 36 dB with underfloor ducts [1]. Plan one floor at a time for positioning, and treat inter-floor leakage as a false-floor risk.

## Shadow fading sigma by environment

Shadowing sigma values from the models above, in dB [1][2][3].

| Source and environment | LOS | NLOS |
|---|---|---|
| P.1238 office | 3.68 | 5.01 |
| P.1238 corridor | 3.77 | 7.58 |
| P.1238 industrial | 2.62 | 5.70 |
| 3GPP InH office | 3 | 8.03 |
| 3GPP InF | about 4 | 4.0 to 7.2 |
| 802.15.4a office (UWB) | 1.9 | 3.9 |

Guidance: use 3-4 dB for clean LOS and 5-8 dB for cluttered NLOS. Simulator defaults of 4 dB for BLE and 5 to 6 dB for WiFi lie within these ranges. Higher sigma both reduces coverage and worsens RSSI ranging, since distance error grows in proportion to sigma divided by exponent.

## Breakpoint distance on long clear paths

On long unobstructed paths the exponent can steepen from about 2 to about 4 past a breakpoint tied to the first Fresnel zone [1]. TR 38.901 defines d_BP = 4 h1 h2 f / c using effective antenna heights above an environment height [2]. With heights 3 m and 1.2 m at 2.44 GHz, a simple estimate gives about 117 m, ignoring the effective-height correction (computed). At 920 MHz it is about 44 m; at 6.49 GHz about 311 m. Most single-hall links in positioning systems stay inside that distance, so the breakpoint rarely matters. It can matter at UHF over long aisles, where low antennas and low frequency bring the distance down to the range of interest.

## Applying this in Sense Sim

The simulator uses a different model per technology. BLE and WiFi RSSI: rssi_at_1m - 10 x path_loss_exp x log10(d) - wall_loss_db x wall_eq + gain_db + N(0, shadow_sigma_db). RFID and UWB first-path power use FSPL (n = 2) plus wall loss. Walls come from ray casts, so exponent should represent the open part only.

Consequence: the wifi-ap-rtt and wifi-ap-rssi exponent 3.0 combined with explicit walls double-counts compared with P.1238, whose coefficients include walls and whose LoS values are near 2 [1]. Treat WiFi predictions in open halls as conservative. Check any change with rtls_coverage for tech ble or wifi at the tag height and with rtls_run. There is no floor-specific term; floor loss appears only if a slab object lies on the ray.

## Sources

1. Recommendation ITU-R P.1238-13, Propagation data and prediction methods for the planning of indoor radiocommunication systems — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.1238-13-202509-I!!PDF-E.pdf
2. 3GPP TR 38.901 v18.0.0, Study on channel model for frequencies from 0.5 to 100 GHz — ETSI/3GPP, 2024 — https://www.etsi.org/deliver/etsi_tr/138900_138999/138901/18.00.00_60/tr_138901v180000p.pdf
3. IEEE 802.15.4a channel model final report, doc 15-04-0535 — A. Molisch et al., IEEE, 2004 — https://www.ieee802.org/15/pub/04/15-04-0535-00-004a-tg4a-channel-model-final-report.pdf
