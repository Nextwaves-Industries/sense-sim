---
title: UWB ranging methods (SS-TWR, DS-TWR, TDoA, PDoA)
domain: uwb
tags: uwb ranging, ss-twr, ds-twr, sds-twr, two-way ranging, tdoa, uplink tdoa, downlink tdoa, dl-tdoa, ul-tdoa, blink, pdoa, aoa, clock drift, clock skew, ppm, reply time, antenna delay calibration, capacity, update rate, aloha, tag density, anchor sync
aliases: đo khoảng cách hai chiều, đo khoảng cách UWB, sai lệch đồng hồ, trôi đồng hồ, hiệu chuẩn độ trễ ăng-ten, chênh lệch thời gian đến, đồng bộ anchor, số lượng tag tối đa, tốc độ cập nhật, time of flight, time difference of arrival, two way ranging
verified: 2026-11-02
---
# UWB ranging methods

## Choosing between TWR and TDoA for UWB positioning
| Criterion | DS-TWR | Uplink TDoA (tag blinks) | Downlink TDoA (tag listens) |
|---|---|---|---|
| Tag messages per fix | At least 3 per anchor [2]; 12 total for a 2D fix in [5] | 1 blink | 0 transmitted |
| Anchors need clock sync | No [6] | Yes [6] | Yes [5] |
| Tag power | High [6] | Very low, "few years at 1 Hz" [6] | Receive-heavy (computed below) |
| Tag count | Scales poorly [6] | Shared blink budget [6] | Not limited by tag count [9] |
| Position computed by | Tag or anchor | Central engine [4] | Tag [9] |
| Fast-moving tags | Ranges collected over time [4] | One transmission per fix [4] | Sequential anchor frames [5] |

Decision rule (rule of thumb): many low-rate assets favour uplink TDoA, few high-rate vehicles or peer-to-peer ranging favour DS-TWR, and devices that must know their own position without a network, in large numbers, favour downlink TDoA. Choose on capacity and power first: the sources here do not establish that either method is more accurate in harsh sites.

## SS-TWR formula and clock drift error
Single-sided two-way ranging: the initiator measures round-trip time T_round; the responder reports its reply time T_reply.

ToF = (T_round - T_reply) / 2, range = c * ToF.

The two clocks tick at different rates, so SS-TWR is biased. The expected ToF error is 0.5 * e * T_reply, where e is the relative clock skew [1]. Range bias in metres (computed from that formula with c = 0.2998 m/ns):

| Relative skew e | T_reply 0.35 ms | T_reply 1 ms |
|---|---|---|
| 4 ppm | 0.21 m | 0.60 m |
| 10 ppm | 0.52 m | 1.50 m |
| 40 ppm (two untrimmed +/-20 ppm crystals) | 2.10 m | 6.00 m |

The DW3000 crystal is specified at +/-20 ppm before on-chip trim [3]. In a static DW1000 test, a frequency difference of about 5 ppm with no correction produced about 1 m of error over the long reply used [7].

Guidance: never use raw SS-TWR unless the clock offset is estimated and removed, and keep the reply as short as the processor allows. In return for that work SS-TWR needs only two frames per range.

## DS-TWR formula, drift cancellation and noise penalty
Double-sided two-way ranging adds a third message so that each side has a round-trip and a reply time [2].

Asymmetric form: ToF = 0.5 * (t41 - (t64 / t53) * t32). Here t41 is the initiator's round trip, t32 the responder's reply, and t64 / t53 scales the reply by the clock ratio measured over the second exchange. Using that ratio removes the skew bias of SS-TWR, and the expected error is zero to first order [1].

The cost is noise. Variance is R * (1 + a + a^2), where R is the SS-TWR noise variance and a = t32 / t53 [1].

| t32 | t53 | Standard deviation versus SS-TWR (computed) |
|---|---|---|
| 0.35 ms | 1.9 ms | x 1.10 |
| 0.35 ms | 0.35 ms | x 1.73 |

Measured on DWM1000 modules 1.5 m apart: about 2.8 cm for SS-TWR and about 2.5 cm for the DS-TWR limit. A 2 ms reply gave an order of magnitude more noise than 0.35 ms [1]. With 7.2 ms of computer processing per range and t32 of 0.35 ms, the variance-minimising t53 was about 1.9 ms [1].

## UWB antenna delay calibration
The measured time includes delays in the antenna and RF path, so uncalibrated radios range with a constant offset. Reported sizes differ widely:

| Observation | Value |
|---|---|
| DW3000 ranging accuracy, LOS, after calibration | about +/-6 cm [3] |
| DW3000 without calibration | about +/-15 cm [3] |
| DW1000 hardware offset in one test | 0.3 m [7] |
| DWM1000 anchor-tag pair, linear fit | offset 0.847 m, slope 0.96 [5] |
| Warehouse AMR, raw constant range bias | up to 1.19 m [8] |

Practical procedure (rule of thumb): range between devices at surveyed distances, fit a constant offset per device or per pair, and write it to the delay registers or subtract it in software. Qorvo's APS014 note covers production procedures with a reference device, but its numbers were not verified for this knowledge base. Sources disagree on size because board, antenna and chip differ.

Signal level also shifts the timestamp: a study of DW1000 reports that this bias vanishes at about -80 to -75 dBm [7]. Calibrate at a representative distance and do not place reference devices so close that the receiver saturates.

## Uplink TDoA principle and anchor clock synchronisation
A tag transmits one blink. Anchors timestamp it, and the difference between two anchors defines a hyperbola: d_ij = |p - a_i| - |p - a_j| = c * (t_i - t_j). Two unknowns need 2 differences from 3 anchors in principle; 3 differences from 4 anchors is the practical minimum. The tag needs no accurate clock, but the anchors must share a timebase.

Clock drift is severe without tracking. Two DWM1000 nodes drifted 26 ns/s relative to each other, about 8 m of error per second [5]. Reported synchronisation results:

| Result | Value |
|---|---|
| Wireless sync, standard 30 ppm DWM1000 clock | 0.2 ns 1-sigma, about 6 cm (computed) [4] |
| Same system, TDoA position precision | 20 cm 1-sigma [4] |
| Fixes within 20 cm, sync age 100 ms / 300 ms / 500 ms | 92% / 85% / 60% [5] |

Guidance: plan for a sync message at least every 100 to 300 ms when not filtering clock states, and require each anchor to hear a sync source with line of sight (see UWB anchor placement). Wired timing backhaul avoids the radio link, but no number for it was verified here.

## Downlink TDoA and the tag power trade-off
In downlink TDoA, anchors transmit in a schedule and tags only listen and compute their own position, so the number of tags does not load the channel. One lab system with 6 wall anchors reached up to 67 Hz with absolute errors below 20 cm, using 3 simultaneous tags [9]. The claim of unlimited tags is architectural and was not demonstrated at scale [9]. Time multiplexing lowers the fix rate as anchors are added [5].

Reception costs more than a blink. From DW3000 current profiles (78 mA in RX for 170 us per frame [3]), hearing 6 anchors costs about 80 uC per fix (computed), versus about 9 uC for an uplink blink (see UWB chips). Downlink TDoA fits powered or rechargeable devices, not multi-year coin-cell tags at high rate.

## PDoA and angle of arrival on UWB
Phase difference of arrival (PDoA) uses two antennas on one device to measure a bearing: angle = asin(lambda * dphi / (2 * pi * d)), with antenna spacing d no more than lambda/2 to avoid ambiguity, which is 23.1 mm on channel 5 and 18.8 mm on channel 9 (computed). The DW3120 and DW3220 have a second port for this [3].

| DW3000 PDoA variant figure | Value |
|---|---|
| PDoA accuracy | +/-10 degrees |
| AoA accuracy, typical | about +/-5 degrees |
| RX sensitivity penalty | about 1.4 dB |
| TX power penalty | about 1 dB |
| Needed crystal offset between devices | more than 5 ppm in magnitude |

Lateral error is range times the angle error in radians: +/-5 degrees at 10 m gives about +/-0.87 m (computed). PDoA suits one-anchor range-plus-bearing, not centimetre tracking. All figures [3].

## UWB network capacity and tag update budget
Air time limits tags per radio cell. Pozyx calculates a theoretical maximum of 1376 blinks/s for ALOHA TDoA, assuming shortest packets, no guard times and a shared cell of roughly 1000 m2, which gives 100 tags about 13 Hz each [6]. A real wireless-sync system demonstrated 20 blinks/s on 10 anchors, and its authors expected about 100 blinks/s after tuning [4]. The gap of over 10 times is the honest planning range.

Pure ALOHA, widely established: P(blink survives) = exp(-2 * rate * T_frame). With T_frame = 151 us (computed):

| Tags at update rate | Blinks/s | Survive |
|---|---|---|
| 100 at 1 Hz | 100 | 97% |
| 200 at 2 Hz | 400 | 89% |
| 500 at 5 Hz | 2500 | 47% |

DS-TWR with 4 anchors needs 12 frames: at 170 us [3] plus two 0.35 ms gaps per anchor, that is about 4.8 ms per fix (computed), or about 208 fixes/s for all tags at best. Measured overhead is often larger: one setup spent 7.2 ms of processing per range [1], and another reached only 12 Hz for one tag [5].

## Applying this in Sense Sim
- Choose the method with the tag profile: `uwb-tag` (`mode` twr, 10 Hz) or `uwb-tag-tdoa` (`mode` tdoa, 5 Hz). `rtls_optimize_anchors` plans TDoA only if all UWB tags in the scene blink, and the planner then needs 4 anchors per point instead of 3.
- Check with `rtls_run` (`seconds`, `reset`): it reports RMSE and CEP50/95. Use `rtls_control` `robust` for outlier rejection in TWR and `kalman` for tracking.
- TDoA noise comes from `sync_sigma_ns` (default 0.15 ns, about 4.5 cm) plus half `range_sigma_m`. Published wireless sync is 0.2 ns [4]. For a design margin, use a custom profile with a larger value.
- `rtls_auto_deploy` (in `notes`) and `rtls_optimize_anchors` (in `note`) report when anchors "can't reach the others for clock sync". Treat that as a blocking problem for TDoA.
- The simulator does not model air time, collisions or clock drift: `clock_ppm` is not read, and `update_hz` costs nothing. A plan can show perfect accuracy at 500 tags at 10 Hz, so check the capacity table separately and tell the user that this check is manual.
- PDoA on UWB is not simulated. Only BLE has an AoA method.

## Sources
1. Reducing Two-Way Ranging Variance by Signal-Timing Optimization, M. A. Shalaby et al. — IEEE TAES (preprint), arXiv, 2022 — https://arxiv.org/abs/2211.00538
2. An Overview of UWB Standards and Organizations (IEEE 802.15.4, FiRa, Apple), D. Coppens et al. — IEEE Access, 2022 — https://arxiv.org/abs/2202.02190
3. DW3000 Datasheet, version 1.1 — Decawave (Qorvo), 2020 — https://www.mouser.com/datasheet/2/412/DW3000_Data_Sheet-3532945.pdf
4. Scalable, Low-Cost Indoor Localization System using TDoA and UWB, A. Acton and V. Kulkarni — ARIN Technologies, Microsoft Indoor Localization Competition, 2017 — https://www.microsoft.com/en-us/research/wp-content/uploads/2016/11/ARINTech_Acton.pdf
5. Robust and Scalable Techniques for TWR and TDoA based localization using Ultra Wide Band Radios, R. Ramesh et al. — arXiv, 2020 — https://arxiv.org/abs/2008.04248
6. TDOA and TWR positioning protocols explained — Pozyx (vendor), checked 2026-11-02 — https://pozyx.io/pozyx-academy/positioning-protocols-explained
7. Decawave UWB clock drift correction and power self-calibration, J. Sidorenko et al. — arXiv, 2019 — https://arxiv.org/abs/1902.11085
8. Deployment-Ready UWB Localization for Industrial Ground Robots with Automatic Anchor Calibration and Terrain-Aware Fusion, A. Raab et al. — arXiv, 2026 — https://arxiv.org/abs/2607.15807
9. Scale up to infinity: the UWB Indoor Global Positioning System — arXiv, 2021 — https://arxiv.org/abs/2112.01950
