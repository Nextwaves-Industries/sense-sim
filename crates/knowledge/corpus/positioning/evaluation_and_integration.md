---
title: RTLS evaluation and integration - ISO/IEC 18305, RMSE, CEP, p95, latency, capacity, MQTT, webhook, UDP, OpenEMS, omlox
domain: positioning
tags: ISO/IEC 18305, EvAAL, IPIN competition, PerfLoc, SE95, RMSE, DRMS, CEP, CEP50, CEP95, R95, p95, third quartile, percentile, latency, update rate, capacity, MQTT, webhook, UDP, Modbus, OpenEMS, omlox, hub, recorder
aliases: đánh giá độ chính xác, sai số trung bình bình phương, phân vị 95, độ trễ, tốc độ cập nhật, dung lượng thẻ, tích hợp MQTT, tích hợp hệ thống, chuẩn thử nghiệm định vị, test method for RTLS accuracy, CEP conversion
verified: 2026-11-02
---
# RTLS evaluation and integration

## ISO/IEC 18305 test standard
ISO/IEC 18305:2016 identifies performance metrics and test and evaluation scenarios for localization and tracking systems, and guides how to present and visualize results, with a primary focus on indoor environments; edition 1.0 was published 2016-11-01 [1]. A second edition has been reported in draft (ISO/IEC DIS 18305), but I could not open the ISO page to confirm status. The standard text is paid, and I did not read it, so its detailed metric list and scenario tables are not reproduced here. Its use by NIST: the PerfLoc data repository, collected with four Android phones in four large buildings, was built to enable evaluation of apps under the standard [2]. The standard has been contested: Potorti et al. published a critical reading and the editor replied point by point, accepting some comments for revision and rejecting others [3]. Cite the edition when quoting "ISO 18305 compliant", and ask vendors which scenarios they ran.

## RMSE, CEP and p95 definitions
Let e_i be the horizontal error of fix i, in metres.
- RMSE, also called DRMS: sqrt(mean(e_i^2)).
- CEP50: radius containing 50% of fixes, the median of e_i.
- CEP95, also called R95 or p95: radius containing 95% of fixes.
- Third quartile: 75th percentile, the accuracy score in the EvAAL framework, which uses 2D Euclidean error for single-floor settings [4].
- SE95 and SE50: columns on the NIST PerfLoc leaderboard [5]. The page does not spell out the definition; they are read as 95th and 50th percentile errors.

Always state 2D or 3D, the number of samples, moving or static, and whether fixes that never arrived are counted. A system that drops fixes during hard moments gets a better percentile unless availability is reported alongside it. Means hide tails, so specify p95 for operational decisions and keep the median as a sanity check.

## CEP95 versus RMSE conversion
If errors are circular normal with per-axis sigma, radial error is Rayleigh, and the radius at probability F is sigma x sqrt(-2 ln(1 - F)) [6].

| Measure | In sigma | In RMSE (DRMS) |
|---|---|---|
| RMSE (DRMS) | 1.414 | 1.000 |
| CEP50 | 1.177 | 0.833 |
| p75 | 1.665 | 1.177 |
| p90 | 2.146 | 1.517 |
| p95 (R95) | 2.448 | 1.731 |

Useful factors [6]: R95 = 1.73 x DRMS and R95 = 2.08 x CEP50. The p75 and p90 rows follow from the same formula (arithmetic). Worked example: RMSE 0.20 m gives CEP50 about 0.17 m, and p95 about 0.35 m. These hold only for zero-mean, circular, Gaussian error. Elongated error ellipses and biases break them.

## Real positioning errors have heavier tails
The Rayleigh ratio p95 / p50 is 2.08 [6]. Measured systems are often worse. NIST PerfLoc leaderboard values, in metres [5]:

| Team | SE95 | SE50 | Ratio (arithmetic) |
|---|---|---|---|
| Rank 1 | 6.29 | 2.04 | 3.1 |
| Rank 2 | 9.04 | 2.82 | 3.2 |
| Rank 3 | 24.60 | 5.18 | 4.7 |

Silicon Labs Channel Sounding also shows a heavy tail: LOS PBR static p90 0.22 m but p95 0.61 m [7]. Action: do not estimate p95 from RMSE or a median when the system has NLOS or outliers, or when a vendor quotes only a mean. Ask for the distribution. In a design review, compute the percentile from simulated or measured errors. Use the factors only for rough checks and for converting specs on clean LOS systems.

## Designing a positioning accuracy test
Good tests match real use. Lessons from published evaluations:
- Test unfamiliar and realistic spaces. The Microsoft competition tested 22 solutions in one unfamiliar space over 2 days [8].
- Include motion modes. PerfLoc live tests covered normal walking, elevators, re-entering the building, pushing a cart, walking backwards and sideways, and crawling, in a 30,000 m2 building [9].
- Results do not generalise from one site: sensor types and anchor density vary across deployments [10].
- Survey ground truth points at an accuracy several times better than the target (rule of thumb).
- Log raw measurements so that solver changes can be replayed.
- Report by zone: open floor, aisle, near walls, outside the anchor hull.
- Repeat at different times, with people and stock present.

Report sample size, percentile, availability and update rate in the same table, or the numbers cannot be compared.

## Latency and update rate
End-to-end latency is the sum of tag interval wait, air exchange, solver, filter lag and transport. Average wait from update interval T is T / 2 (arithmetic). Position lag at speed v is v x latency: a 2 m/s forklift with 0.5 s latency is drawn 1 m behind, which matters more than a 0.2 m accuracy spec for collision warnings. Update rate must suit the motion, and rate trades directly against tag battery and airtime. Reference points: BLE advertising interval 20 ms to 10.24 s [11]; an FTM burst of 250 us to 128 ms [12]. A heavier Kalman filter smooths but lags, so measure filtered and raw latency separately. For zone events, also count the consecutive fixes required by your hysteresis.

## Capacity of positioning systems
Every technique shares a medium. Published anchors: one FTM example gives about 30 stations per second per AP at 30 ms per measurement [12], and FTM frames contend under CSMA/CA, adding latency in dense deployments [13]. In congested NLOS tests, 802.11az FTM frames failed 37-65% of the time [13]. BLE advertisers add a random 0-10 ms delay to reduce repeated collisions [11]. Channel Sounding needs a connection per ranging pair [14]. Quick check: total airtime share = tags x update rate x airtime per update, and also confirm how many anchors each tag must range with. Treat any capacity figure from a vendor as a claim until you know its update rate, tag density per zone and reliability target. Test at the planned peak, for instance when many tags gather at a shift change.

## Output protocols compared
| Output | Delivery | Best for | Risk |
|---|---|---|---|
| MQTT QoS 0 | at most once, via broker | live position streams | message loss if subscribers lag |
| MQTT QoS 1 | at least once | alarms, zone events | duplicates, so consumers must deduplicate |
| Webhook (HTTP POST) | batch request | enterprise integration and events | latency from batching; retry policy |
| UDP | no delivery guarantee | lowest latency local tools | loss, no back pressure |
| Modbus/TCP | polled registers | PLC integration | numeric summaries only |
| Recorder (JSONL) | local file | replay and evaluation | disk use |

Practical rules: publish alarms and positions separately, and use retained messages for state, not events. Keep the coordinate frame, timestamp and units in every payload. Run downstream consumers at a load that matches peak tags, since positions at 10 Hz over 1000 tags amount to 10,000 messages per second (arithmetic).

## omlox open standard
omlox is an open real-time locating standard within PI (PROFIBUS and PROFINET International), joined in April 2020 [15][16]. It has two parts: the core zone uses UWB with an open air interface so devices from different manufacturers can share one zone, and the hub is middleware that converts location data from different technologies into standardized global coordinates through a hub API [15][16]. Complementary zones use BLE, RFID, GPS or others [17]. In March 2025, PI and FiRa created a joint working group on UWB coexistence and interoperability [17]. The under-10 cm figure on partner pages is a marketing claim [16]. I could not access the specification, so check current API details with PI before designing to it. Use omlox when multi-vendor interoperability or several technologies under one coordinate system is a hard requirement.

## Applying this in Sense Sim
Metrics: `rtls_run` returns `rmse_m`, `cep50_m`, `cep95_m`, `max_error_m` from actual simulated errors, also per technology. `cep95_m` is an empirical percentile, not a converted value. Errors pool all technologies at the top level, so read the per-technology rows.

`rtls_coverage` returns `p95_predicted_error_m`: the 95th percentile across floor cells of a predicted 1-sigma-style error, not a per-point 95% radius. The per-point radius is about 1.73 x that cell error for circular errors. `quality` reports `pass_fraction` for `target_m` 0.3 and `min_share` 0.75: with P = 1 - exp(-(R/err)^2), a cell passes when err is under about 0.255 m.

Outputs: `rtls_configure_outputs` with `outputs` holding `mqtt` (`host`, `port`, `username`, `password`, `prefix`, `qos`, `subscribe`), `webhook` (`url`, `headers`, `batch_ms`, default 500), `udp` (`target`, `format` json or csv), `modbus` (`bind`), `recorder` (`path`), `openems` (`url`, `username`, `password`, `period_s`, `write`, `read`). MQTT topics: `<prefix>/tag/<id>/position`, `/range`, `/rssi`, `<prefix>/snapshot`, `<prefix>/alarm`; control on `<prefix>/control`. `rtls_status` shows `sent`, `dropped`, `rtt_ms`, `last_error`.

OpenEMS: `rtls_openems` (`action` start, stop, status) manages OpenEMS Edge, an energy management system. It is unrelated to openEMS, the FDTD field solver that the coverage and antenna features use. `write` maps metrics such as `rmse_cm`, `cep95_cm`, `zone:<name>`, `tag:<id>:x` to channels; `read` reads values back. No omlox output exists; bridge from MQTT and convert scene metres to the hub's coordinates. Tag airtime capacity is not modelled.

## Sources
1. ISO/IEC 18305:2016 catalogue entry — IEC Webstore, 2016 — https://webstore.iec.ch/publication/26165
2. PerfLoc (Part 1): An Extensive Data Repository for Development of Smartphone Indoor Localization Apps (Moayeri et al.) — NIST, 2016 — https://www.nist.gov/node/1578196
3. Response to comments on ISO/IEC 18305 (Moayeri), abstract — NIST, IPIN 2021 — https://www.nist.gov/node/1703016
4. EvAAL framework and criteria — EvAAL, accessed 2026 — https://evaal.aaloa.org/
5. Ranking of PerfLoc participating teams — NIST, frozen 2018-01-17 — https://www.nist.gov/ctl/pscr/ranking-perfloc-participating-teams
6. Circular error probable — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Circular_error_probable
7. Channel Sounding Performance Metrics, RTL library 8.2.0 — Silicon Labs (vendor), accessed 2026 — https://docs.silabs.com/rtl-lib/8.2.0/rtl-lib-channel-sounding-dev-guide/07-channel-sounding-performance-metrics
8. A Realistic Evaluation and Comparison of Indoor Location Technologies (Lymberopoulos et al.), abstract — Microsoft Research, IPSN 2015 — https://www.microsoft.com/en-us/research/?p=168083
9. PerfLoc Prize Competition winner announcement — NIST, 2018 — https://www.nist.gov/node/1835301
10. Towards Ubiquitous Indoor Positioning: Comparing Systems across Heterogeneous Datasets — arXiv 2109.09436, 2021 — https://arxiv.org/abs/2109.09436
11. Device Discovery (BLE get-started guide) — Espressif, ESP-IDF docs, accessed 2026 — https://docs.espressif.com/projects/esp-idf/en/latest/esp32c2/api-guides/ble/get-started/ble-device-discovery.html
12. Indoor Positioning with Wi-Fi Location: A Survey of IEEE 802.11mc/az/bk Fine Timing Measurement Research — arXiv 2509.03901, 2025 — https://arxiv.org/html/2509.03901v1
13. Performance comparison of 802.11mc and 802.11az Wi-Fi Fine Time Measurement protocols (Rajendran et al.) — arXiv 2511.17935, 2025 — https://arxiv.org/html/2511.17935v1
14. Bluetooth Core 6.0 feature overview — Bluetooth SIG, 2024 — https://www.bluetooth.com/core-specification-6-feature-overview/
15. omlox: the open locating standard — omlox, accessed 2026 — https://omlox.com
16. A new PI standard for location technology — FHI, accessed 2026 — https://fhi.nl/en/profibus/omlox/
17. omlox, PI and FiRa join forces for seamless UWB interoperability — PI, 2025 — https://www.profibus.com/newsroom/press-news/omlox-pi-and-fira-join-forces-for-seamless-uwb-interoperability
