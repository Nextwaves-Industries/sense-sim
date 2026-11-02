---
title: BLE direction finding (AoA and AoD) and Bluetooth 6.0 Channel Sounding
domain: positioning
tags: BLE AoA, AoD, angle of arrival, angle of departure, direction finding, Bluetooth 5.1, CTE, constant tone extension, IQ samples, antenna array, ULA, URA, locator, Bluetooth 6.0, Channel Sounding, CS, PBR, phase-based ranging, RTT, round-trip timing, HADM, reflector, initiator
aliases: góc đến, góc đi, định vị theo góc Bluetooth, mảng anten, đo khoảng cách Bluetooth 6, BLE direction finding, angle-based positioning, phase ranging, secure ranging, digital key ranging
verified: 2026-11-02
---
# BLE direction finding and Channel Sounding

## AoA versus AoD in Bluetooth 5.1
Bluetooth Core 5.1 added optional direction finding. In Angle of Arrival (AoA), the transmitter has one antenna, such as an RTLS tag, and the receiver is a multi-antenna locator that samples IQ data while switching antennas, then derives direction from phase differences [1]. In Angle of Departure (AoD), the transmitter has the array and a single-antenna receiver, such as a phone, computes the angle [1]. The Bluetooth SIG positions AoA for RTLS and proximity and AoD for indoor positioning such as wayfinding [1]. Choice rule: track assets centrally with cheap tags, use AoA. Let many phones self-locate privately, use AoD; compute then lives on the phone, and the infrastructure only transmits.

## Constant Tone Extension parameters
The Constant Tone Extension (CTE) is appended to a packet so that the receiver can extract IQ data; it is a run of digital 1s, giving a constant frequency, and it is not whitened [2]. Key parameters:

| Parameter | Value | Source |
|---|---|---|
| CTE length | 16 to 160 us in 8 us steps | [3] |
| Guard period | 4 us | [4] |
| Reference period | 8 us, eight IQ samples at 1 us | [4] |
| Switch and sample slots | 2 us mandatory, 1 us optional | [4] |
| Connection-oriented CTE PHY | LE 1M or LE 2M | [3] |
| Connectionless CTE PHY | LE 1M | [3] |
| Coded PHY | not usable with CTE | [2] |

Connected direction finding appends the CTE to normal packets; connectionless direction finding appends it to periodic advertising [2]. Coded PHY exclusion means long-range coded links cannot be used for AoA.

## AoA antenna array geometry
Array shape decides what can be measured. A uniform linear array (ULA) gives one angle and suits 2D positioning; a uniform rectangular array (URA) gives two angles and suits 3D, for example a 4 x 4 array [3]. MathWorks examples use element spacing of 0.5 wavelength for a linear array and 0.4 and 0.3 wavelength for a rectangular one [3]. One wavelength at 2.44 GHz is about 12.3 cm, so half a wavelength is about 6.1 cm (arithmetic), and one measurement study used 6 cm [5]. Accuracy depends on antenna count, geometry and calibration. Practical rules of thumb: do not move away from a reference array design without re-measuring it, keep the array plane free of nearby metal, and record the installed yaw and tilt, since a mounting error becomes a direct bearing error.

## AoA usable field of view
Measured direction-finding accuracy falls off away from broadside. With a 2-element software-radio array and 6 cm spacing, angles from 0 to 10 deg relative to the array axis gave nearly random phase readings; the usable cone was at or above 45 deg outdoors (maximum error within 4 deg) and 50 to 130 deg indoors, meaning about +/-40 deg around broadside [5]. A second study, with a 6-antenna TI array, also excluded angles under 15 deg [4]. These are small testbeds, and larger arrays or better estimators may extend the cone, but I found no independent measurement of a commercial 2D array across its whole claimed field of view. Plan with overlap: do not rely on one locator near the edge of its cone. A tag directly below a ceiling locator is near boresight and well served; tags far off to the side are not.

## Measured AoA accuracy
Controlled measurements, not warehouse deployments:

| Study | Hardware | Environment | Angle error | Position error |
|---|---|---|---|---|
| arXiv 2103.04121 [4] | TI CC26X2R, 6-antenna array, 2 anchors | indoor 10 x 10 m | mean 1.83 deg, SD 1.1 deg | mean 36.5 cm |
| same [4] | same | outdoor open | mean 0.48 deg in the body, 0.28 deg in the abstract | mean 22 cm |
| Cominelli 2019 [5] | SDR, 2 antennas, 2 anchors | 4 x 2.7 m area | indoor mean well below 2 deg after averaging about 15 channels | under 85 cm for over 95% of positions; under 10 cm for about 15% |

The Bluetooth SIG quotes direction within +/-5 deg as typical [6], and elsewhere says RTLS could reach centimetre level in the right deployment [1]; treat both as claims. Multipath from desks and fixtures degraded indoor results [4]. I found no accessible peer-reviewed RMSE for commercial locators in a metal-racked warehouse, so verify on site before promising sub-metre performance.

## AoA bearing error to position error
Cross-range position error from one bearing is about sigma_theta (radians) x slant distance. At 3 deg (0.052 rad) this is 0.52 m at 10 m and 1.05 m at 20 m; at 5 deg it is 0.87 m at 10 m (arithmetic). Error therefore grows with range, which sets locator spacing more than radio range does. A ceiling locator with known tag height can produce a fix from a single bearing, since the ray meets a known height plane, but height error then becomes horizontal error. With several locators, the point closest to all bearing rays is solved by least squares, and rays crossing at wide angles give the best geometry. Prefer at least two locators in view of every working point so that one blocked path does not end tracking, and so that residuals can flag a bad bearing.

## AoA security and CTE spoofing
The CTE sits after the CRC and is not error-checked, so its content cannot be validated by the normal packet check [5]. In one demonstration, a transmitter that shifted CTE phase during the switch slot between -pi/6 and +pi/6 steered the reported angle through about 60 deg while staying still [5]. The authors suggest a hidden, non-deterministic antenna switching pattern as a countermeasure [5]. Do not use plain AoA as a security boundary for access control or safety interlocks. If location must be trusted, combine it with an authenticated ranging method such as Channel Sounding round-trip timing, or a separate access check.

## Bluetooth 6.0 Channel Sounding mechanics
Channel Sounding (CS) measures distance between an Initiator and a Reflector using Phase-Based Ranging (PBR), Round-Trip Timing (RTT), or both [7]. Facts from the Bluetooth SIG overview [7]:
- 72 channels of 1 MHz each, avoiding the LE primary advertising channels, with a channel map to exclude poor channels.
- PBR is ambiguous around 150 m at 1 MHz frequency separation; RTT resolves the ambiguity.
- Each subevent has 2 to 160 steps and a procedure at most 256 steps.
- An encrypted ACL connection must exist first.
- Antenna arrays are optional and can reduce multipath effects.
- The controller passes low-level data to the host; the application computes distance, so results depend on the algorithm.

Because CS is connection-based, it behaves differently from broadcast tags: each tag needs a link to each ranging partner, which affects capacity and battery. Multiple antenna paths and the anchor count are real design inputs.

## Channel Sounding measured performance
Vendor-reported results for an indoor office at 0.5 to 34 m, Silicon Labs boards, in metres, absolute error [8]:

| Environment | Method, mode | p90 | p95 |
|---|---|---|---|
| Conducted | PBR | 0.06 | 0.06 |
| LOS | PBR static | 0.22 | 0.61 |
| LOS | PBR real-time | 0.28 | 0.32 |
| LOS | RTT real-time | 3.95 | 4.92 |
| NLOS | PBR static | 3.59 | 5.01 |
| NLOS | PBR real-time | 4.25 | 5.09 |
| NLOS | RTT real-time | 14.76 | 15.50 |

Silicon Labs says CS is still in development and antenna group delay and pattern strongly affect results [8]. Sources disagree on the headline figure: Bluetooth SIG says typically +/-50 cm up to 100 m [6]; a 2025 survey lists 1-2 m LOS at 10 m [9]. A peer-reviewed study on commercial hardware finds CS well suited to secure keyless entry, with early promise for localization, but says outliers matter and single-antenna setups are not yet robust for high-precision localization [10]. I could not extract that study's numeric results.

## Choosing between RSSI, AoA and Channel Sounding
| Need | Suggested BLE method | Why |
|---|---|---|
| Room or zone presence, lowest cost | RSSI | Typical fingerprint errors of 2.6-4.8 m at p95 (see BLE RSSI notes) and battery-friendly |
| Continuous sub-metre to metre tracking with cheap tags | AoA | Indoor measured means of tens of cm in small rooms [5][4]; needs array locators and calibration |
| Secure distance to a door, vehicle or machine | Channel Sounding | Encrypted link, RTT cross-check against relay attacks [6][7] |
| Phones self-locating | AoD | Receiver does the computation [1] |

Expect the NLOS figures to dominate outcomes. PBR in NLOS reached about 5 m at p95 in vendor tests [8], so plan line-of-sight paths. Also check device support: not all phones or chips implement CTE or CS. A large disagreement between PBR and RTT should be treated with suspicion [7].

## Applying this in Sense Sim
AoA: profile `ble-aoa-locator` (`aoa_sigma_deg` 3, `aoa_nlos_deg` 10, `aoa_fov_deg` 140, `max_range_m` 20, `sensitivity_dbm` -92, mounted 3 m) with tag `ble-tag`. Deploy with `rtls_auto_deploy` and `ble_aoa: true`, or `rtls_optimize_anchors` with `tech: "ble"`, `profile: "ble-aoa-locator"`, `count`, `target_p95_m`, `apply: true`. These give the locator a facing direction. `rtls_add_device` only sets `yaw_deg`, leaving a locator facing horizontally, and the field-of-view check uses that facing, so prefer the planners for ceiling mounts.

Model behaviour: a bearing needs a tag inside +/-70 deg with no metal or liquid on the direct path; bearing noise combines `aoa_sigma_deg` and `aoa_nlos_deg` x wall-equivalents in quadrature. On the plane, a single bearing yields a fix, and AoA takes priority over RTT and RSSI when present. The 3 deg default falls between the measured 1.83 deg mean [4] and the SIG's +/-5 deg [6]. The 140 deg field of view exceeds the +/-40 deg cone measured on a 2-element array [5], so run a sensitivity check with narrower coverage in mind. AoA's plan target is 0.75 m. AoA fixes report a fixed DOP of 1. Multipath and spoofing are not modelled.

Channel Sounding: there is no shipped profile and no CS tool parameter; say so rather than implying the simulator models it.

## Sources
1. New AoA and AoD Bluetooth capabilities — Bluetooth SIG blog, accessed 2026 — https://www.bluetooth.com/blog/new-aoa-aod-bluetooth-capabilities/
2. Use Bluetooth 5.1-Enabled Platforms for Precise Asset Tracking and Indoor Positioning, Part 2 — DigiKey, accessed 2026 — https://www.digikey.com/en/articles/use-bluetooth-5-1-enabled-platforms-part-2
3. Parameterize BLE Direction Finding Features — MathWorks Bluetooth Toolbox, accessed 2026 — https://www.mathworks.com/help/bluetooth/ug/parameterize-ble-direction-finding-features.html
4. An Experimental Study on Direction Finding of Bluetooth 5.1: Indoor vs Outdoor — arXiv 2103.04121, 2021 — https://ar5iv.labs.arxiv.org/html/2103.04121
5. Dead on Arrival: An Empirical Study of The Bluetooth 5.1 Positioning System (Cominelli, Patras, Gringoli) — arXiv 1909.08063, 2019 — https://ar5iv.labs.arxiv.org/html/1909.08063
6. Bluetooth Channel Sounding: how it works — Bluetooth SIG blog, accessed 2026 — https://www.bluetooth.com/blog/bluetooth-channel-sounding-how-it-works/
7. Bluetooth Core 6.0 feature overview — Bluetooth SIG, 2024 — https://www.bluetooth.com/core-specification-6-feature-overview/
8. Channel Sounding Performance Metrics, RTL library 8.2.0 — Silicon Labs (vendor), accessed 2026 — https://docs.silabs.com/rtl-lib/8.2.0/rtl-lib-channel-sounding-dev-guide/07-channel-sounding-performance-metrics
9. Indoor Positioning with Wi-Fi Location: A Survey of IEEE 802.11mc/az/bk Fine Timing Measurement Research — arXiv 2509.03901, 2025 — https://arxiv.org/html/2509.03901v1
10. Performance Evaluation of Bluetooth Channel Sounding on Commercial Hardware (Wieme et al.), abstract — IEEE Access, 2025 — https://www.imec-publications.be/entities/publication/0f1d8b83-4f42-4084-a5a6-e00b04fc6fe1/full
