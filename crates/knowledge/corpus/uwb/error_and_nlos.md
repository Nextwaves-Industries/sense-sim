---
title: UWB error sources and NLOS (first path, body blocking, measured accuracy)
domain: uwb
tags: uwb accuracy, nlos, non line of sight, los, first path, direct path, dp-nlos, ndp-nlos, multipath, cir, channel impulse response, body shadowing, human body blocking, metal, concrete, wood, cardboard, warehouse accuracy, factory accuracy, rmse, cep95, p95, outlier rejection, bias
aliases: sai số UWB, che khuất, không có tầm nhìn thẳng, đường đi đầu tiên, phản xạ kim loại, kệ kim loại, cơ thể người che, sai số trong kho, sai số nhà máy, độ chính xác thực tế, biết tag bị che, UWB error, shelf blocking, rack blocking
verified: 2026-11-02
---
# UWB error sources and NLOS

## First-path detection and why UWB NLOS errors are positive
UWB timestamps the first detected path, not the strongest one. The DW1000 finds a leading edge with a threshold computed from the noise in the early accumulator samples [9]. That creates two NLOS cases [3]:

| Case | What the receiver detects | Effect |
|---|---|---|
| DP-NLOS, direct path present but weakened | Direct path, slowed by material | Bias usually at most 2 ns, about 0.6 m |
| NDP-NLOS, no detectable direct path | A later reflection or diffraction | Several ns, a few metres |

Propagation through two 10 cm concrete walls adds about 1 ns, about 0.3 m [3]. Excess delay cannot be negative, so NLOS errors are skewed long: range outliers are not symmetric and a mean filter does not remove them. Some measured bias values can still be negative because DW1000 timestamps also carry a signal-level-dependent bias [2].

Guidance: never treat a fix with a direct path missing as a noisy LOS measurement. Reject or down-weight it.

## Measured NLOS bias per link category (DW1000, furnished apartment)
A DW1000 study with six wall-mounted anchors in a furnished apartment split links by first-path power and measured the time-of-arrival bias [2]:

| Category | First-path power | Mean bias | Standard deviation | Mean / std in metres (computed) |
|---|---|---|---|---|
| LOS | above -78.5 dBm | no NLOS bias applied | no NLOS bias applied | - |
| NLOS | -78.5 to -85 dBm | 0.49 ns | 1.39 ns | 0.15 / 0.42 m |
| Severe NLOS | below -85 dBm | 1.92 ns | 2.02 ns | 0.58 / 0.61 m |

Subtracting the mean bias and raising each link's variance reduced positioning error in most cases. The classifier was right on about 78% of links [2].

The standard deviation exceeds the mean in both categories, so a correction helps only partly. These thresholds fit that site and those radios: re-derive them on your own data. Warehouses with metal may be much worse than an apartment.

## Material effect on UWB ranging: metal, wood, thin materials
A TDoA dataset on DWM1000 radios tested cardboard, metal, wood, plastic and foam. Findings from the same group [6][7]:

| Material between anchor and tag | Observed effect |
|---|---|
| Foam, cardboard, plastic | Little influence |
| Fabric (device in a backpack) | Barely changed accuracy, 22 cm RMSE [8] |
| Wood | Measurable errors |
| Metal | Considerably larger errors than wood; link can drop out in severe cases |
| Water, wires near an antenna | Install guides say keep them clear [10] |

Important behaviour: metal does not simply remove a link. In these experiments the radio often still reports a measurement, with a large error from an indirect path, and with moving obstacles the errors become less predictable [6]. Static obstacles gave consistent, repeatable bias over trajectories, which is why site-specific bias correction can work [6].

Numeric per-material biases were not published in the text checked, so material weights in a simulator remain modelling assumptions.

## Human body blocking of UWB tags
A body between tag and anchor is a significant, moving NLOS source. Results for a DW1000 tag worn at the abdomen in an open industrial lab [4]:

| Body-tag-anchor angle | Median range error | p99 |
|---|---|---|
| Below 80 degrees, little shadowing | 4 cm | 32 cm |
| Above 160 degrees, strong shadowing | 32 cm | 1.53 m, outliers to 4.05 m |

Position error with 8 anchors: a Kalman filter went from 5 cm median and 10 cm p90 in LOS to 16 cm and 31 cm with shadowing. IMU-assisted methods reached 9 cm and 20 cm [4]. An older study, measuring 3 to 5.5 GHz, disagreed: it found strong attenuation but no significant range error [11]. Treat the result as dependent on hardware and thresholding.

Guidance: mount a person's tag high on the body and provide more anchors so the body blocks only some (rule of thumb). Pozyx recommends a 45-degree twist between upper and lower anchor rings for this reason [10].

## Detecting NLOS from the channel impulse response (CIR)
DW1000 and later radios expose diagnostics such as total received signal level (RSL), first-path signal level (FSL), first-path amplitudes and the full CIR [9]. Usable indicators:

- First-path power alone: thresholds of -78.5 and -85 dBm classified about 78% of links correctly at one site [2].
- RSL minus FSL: multipath-rich NLOS links have more total power relative to the first path. No vendor threshold was verified, so calibrate the threshold on site data.
- Statistics of the CIR: combinations of 3 chosen features separated LOS from NLOS with 93.7% average success, on about 19,000 DW1000 waveforms [3].
- Direct-path absence is harder: the best feature pair averaged 76% and misclassified many DP-NLOS waveforms [3].

Guidance: a detector cannot be relied on for every link. Combine it with geometric redundancy and robust solvers. Signal-level estimates also become unreliable when strong, since measured power matches true power only below about -85 dBm in one study [12].

## Measured UWB accuracy in warehouses and industrial sites
Real sites run several times worse than datasheet LOS values. Sources do not use the same metric; they should not be directly ranked.

| Site and system | Metric | Result |
|---|---|---|
| DW3000 datasheet, LOS, calibrated | Ranging accuracy | +/-6 cm [1] |
| Warehouse with metal, DecaWave (as summarised in [5]) | Mean / median | 0.49 / 0.39 m |
| Same site, BeSpoon / Ubisense AoA (as summarised in [5]) | Mean | 0.71 / 1.10 m |
| Metal warehouse, Pozyx Creator (as summarised in [5]) | Mean, mixed LOS/NLOS | 0.86 m, 0.51 m with improved solver |
| 14 x 40 m industrial lab, Pozyx Enterprise TDoA | Static mean at 2 m / 0.3 m height | 0.34 / 0.65 m [5] |
| Same lab | 90% of points within, 2 m / 0.3 m height | 0.77 / 1.3 m [5] |
| Same lab, robot at 0.5 m/s, 0.3 m high | Dynamic error | 0.3 to 0.4 m [5] |
| Multi-room, 16 anchors, TDoA + IMU | RMSE | 0.28 m overall, 0.34 m hallway [8] |

Vendors claim 10 to 30 cm for correctly installed systems [10]. A plan promising less than about 0.3 m in a racked metal warehouse needs line of sight or sensor fusion to support it.

## Tag height and anchor survey error
Two often-ignored error sources:

- Tag height: the 14 x 40 m lab study measured mean error of 0.34 m at 2 m height but 0.65 m at 0.3 m height, near floor clutter [5]. Robots and pallets run worse than helmets or forklift roofs.
- Anchor survey: in a multi-room deployment, surveyed anchor positions differed from the design by 22 cm RMSE because construction differed from blueprints [8]. Every anchor position error passes into the fix. One warehouse pipeline solves anchor positions automatically from ranges collected along a short drive [13]; another system runs anchor-to-anchor TWR with some ground-truth references for self-calibration [14].

Guidance: survey anchors with a total station or laser, or self-calibrate and verify. Do not trust the CAD model. Test at the real tag height, not at a convenient tripod height.

## Fusion, filtering and outlier rejection for UWB
Filters reduce noise and reject outliers, but cannot remove a persistent bias.

| Technique | Evidence |
|---|---|
| Add IMU and a particle filter under body shadowing | Median 23 cm down to 12 to 13 cm [4] |
| Chi-square gating with error-state Kalman filter | About 10 cm RMSE in obstacle-free flights, much worse with obstacles [6] |
| Mahalanobis outlier gate | Threshold raised from 5 to 10 for a multi-room site [8] |
| Bias-aware EKF with odometry on a warehouse AMR | Effective even with limited LOS and sparse anchors [13] |

Rejection needs redundancy: with exactly the minimum anchors, there is nothing to compare. Design for at least one spare link where blocking is common. In placement analysis, use a conservative noise assumption rather than LOS noise, as authors of a TDoA system recommend [8].

## Applying this in Sense Sim
The simulator's UWB link model, `uwb_range` in `src/rtls/radio.rs`, can be matched to these sources through a device profile.

| Profile field (default `uwb-anchor`) | Meaning | Source check |
|---|---|---|
| `range_sigma_m` 0.08 | LOS noise | Datasheet 1.5 cm is lower; 0.08 m is sensible with calibration residual |
| `nlos_bias_m` 0.35, `nlos_sigma_m` 0.25 | Added per typical obstruction | Same order as measured 0.15 to 0.58 m mean bias |
| `max_walls` 2 | Obstruction weight beyond which no link | Two 10 cm concrete walls weigh 4 and block; [3] still reports a direct path about 1 ns late |

- Run `rtls_list_devices` and read `links` and `nlos_links` per tag. Use `rtls_run` for RMSE and CEP95 and turn on `robust` with `rtls_control` for TWR.
- Metal and liquid objects return no measurement in the simulator, while real radios often return a late one. Real behaviour is worse, because a bad range is harder to reject than a missing one. Warn the user that results near racks are optimistic, and add margin to `target_cm`.
- Workers are not obstructions in the UWB formula: there is no body term, so body blocking is not predicted. For person tags use `redundancy: true` and quote the body-shadowing numbers above.
- The default target of 0.3 m p95 is a design goal, not a typical warehouse measurement.

## Sources
1. DW3000 Datasheet, version 1.1 — Decawave (Qorvo), 2020 — https://www.mouser.com/datasheet/2/412/DW3000_Data_Sheet-3532945.pdf
2. First Path Component Power Based NLOS Mitigation in UWB Positioning System, M. Kolakowski and J. Modelski — TELFOR, 2017 (arXiv 2024) — https://arxiv.org/abs/2403.19706
3. Detection of direct path component absence in NLOS UWB channel, M. Kolakowski and J. Modelski — MIKON, 2018 (arXiv 2024) — https://arxiv.org/html/2404.15314v1
4. Robust IMU-Based Mitigation of Human Body Shadowing in UWB Indoor Positioning, C. De Cock et al. — Sensors, 2023 — https://pmc.ncbi.nlm.nih.gov/articles/PMC10575093/
5. Evaluation and Comparison of Ultrasonic and UWB Technology for Indoor Localization in an Industrial Environment, A. L. Cretu-Sircu et al. — Sensors, 2022 — https://vbn.aau.dk/ws/files/482474789/sensors_22_02927.pdf
6. UTIL: An Ultra-wideband Time-difference-of-arrival Indoor Localization Dataset, W. Zhao et al. — arXiv, 2022 — https://arxiv.org/abs/2203.14471
7. Finding the Right Place: Sensor Placement for UWB Time Difference of Arrival Localization in Cluttered Indoor Environments, W. Zhao, A. Goudar, A. P. Schoellig — arXiv, 2022 — https://ar5iv.arxiv.org/html/2204.04508
8. Ultra-wideband Time Difference of Arrival Indoor Localization: From Sensor Placement to System Evaluation — arXiv, 2024-2026 — https://arxiv.org/html/2412.12427v2
9. DW1000 channel response and first-path diagnostics slides — Decawave, shared on Qorvo forum — https://forum.qorvo.com/uploads/short-url/qLFhTawxz5stQxZYk790YotpMud.pdf
10. Choosing the anchors' locations, Pozyx installation documentation (vendor), checked 2026-11-02 — https://installation-docs.pozyx.io/choosing-the-anchors-locations
11. The effect of human-body shadowing on indoor UWB TOA-based ranging systems — University College Cork research listing, 2012 — https://research.ucc.ie/en/publications/the-effect-of-human-body-shadowing-on-indoor-uwb-toa-based-rangin/
12. Decawave UWB clock drift correction and power self-calibration, J. Sidorenko et al. — arXiv, 2019 — https://arxiv.org/abs/1902.11085
13. Deployment-Ready UWB Localization for Industrial Ground Robots with Automatic Anchor Calibration and Terrain-Aware Fusion, A. Raab et al. — arXiv, 2026 — https://arxiv.org/abs/2607.15807
14. Scalable, Low-Cost Indoor Localization System using TDoA and UWB, A. Acton and V. Kulkarni — ARIN Technologies, 2017 — https://www.microsoft.com/en-us/research/wp-content/uploads/2016/11/ARINTech_Acton.pdf
