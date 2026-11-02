---
title: Choosing an indoor positioning technology - UWB, BLE, WiFi, RFID and others
domain: positioning
tags: technology selection, decision table, UWB, BLE, AoA, Channel Sounding, WiFi, FTM, RTT, RFID, passive, active, ultrasound, visible light, cost per m2, tag cost, battery life, infrastructure, NLOS, metal, warehouse, hospital, factory, RTLS comparison
aliases: chọn công nghệ định vị, so sánh UWB BLE WiFi RFID, chi phí định vị trong nhà, giá mỗi mét vuông, giá thẻ, thời lượng pin, RTLS nào phù hợp, which RTLS should I use, indoor positioning comparison
verified: 2026-11-02
---
# Choosing an indoor positioning technology

## Decision table by required accuracy
| Required accuracy, p95 | First choice | Alternative | Note |
|---|---|---|---|
| Better than 0.5 m, tracking | UWB | Channel Sounding in clear LOS only | UWB stays near decimetres in measured robot tests [1] |
| 0.5-2 m | BLE AoA | WiFi FTM with LOS and calibration | AoA means of tens of cm in small rooms [2][3], FTM 1-2 m typical [4][5] |
| 2-5 m | BLE RSSI fingerprint, dense | WiFi FTM in NLOS | BLE under 2.6-4.8 m at p95 [6] |
| 5-15 m, room or zone | BLE RSSI sparse, WiFi RSSI | - | WiFi fingerprint under 8.5 m at p95 [6] |
| Item passes a point, or inventory | Passive UHF RFID | BLE proximity | Event, not continuous coordinates; RFID notes cover this |

Values are illustrative of research conditions, not guaranteed. Always verify with a site pilot or simulation; the table only narrows the choice. Do not pick a technology from a headline accuracy figure when the site has heavy NLOS.

## Measured side-by-side comparison
Few studies test technologies in the same space. A 2025 experiment over about 431 m2, three rooms and walls gave these mean errors [1]:

| Technology | Static | Moving robot | Method |
|---|---|---|---|
| UWB (Pozyx) | under 13.1 cm | 28.9 cm (conclusion says 29.9 cm) | TWR + trilateration |
| BLE (ESP32) | 191.7 cm | 340.1 cm | RSSI weighted average |
| WiFi (ESP32) | 261.5 cm | 608.7 cm | RSSI weighted average |

The paper's text and table disagree on anchor counts (nine vs four), so do not over-read the absolute numbers [1]. Moving tags did substantially worse than static tags for all three, by a factor of 1.8 to 2.3 (arithmetic). Static bench results therefore flatter a real tracking application. In a separate 600 m2 test, dense BLE fingerprinting beat existing WiFi fingerprinting, at under 2.6 m and under 8.5 m respectively at p95 [6]. A 2025 FTM survey tabulates UWB LOS accuracy under 0.1 m with LOS range under 15 m, against WiFi with LOS range over 50 m [5].

## Cost per square metre: vendor estimates only
I found no independent, peer-reviewed cost-per-m2 study. The table derives hardware-only ranges from one vendor guide's prices and densities; arithmetic is mine, and installation, cabling, software and licences are excluded [7]:

| Technology | Infrastructure price | Density claimed | Derived hardware cost | Tag |
|---|---|---|---|---|
| BLE beacons, room-level | $5-15 per beacon | 1 per 50-100 m2 | $0.05-0.30 per m2 | $5-30 |
| BLE beacons, precise | $5-15 | 1 per 5 m2 | $1-3 per m2 | $5-30 |
| UWB | $200-500 per anchor | 4-8 per 100-200 m2 | $4-40 per m2 | $30-100 |
| WiFi RTT | $200-500 per AP | not stated | not derivable | phone or 802.11mc device |
| Passive RFID | $1,000-5,000 per reader | not stated | not derivable | $0.05-0.50 |

Use these only for order-of-magnitude trade-offs and get quotes. Cost does not scale simply with area: tag count, cabling, and whether a network already exists can dominate. The surveyed literature agrees qualitatively that UWB costs more than BLE and WiFi [1][8].

## Battery life and tag constraints
| Technology | Vendor-claimed tag life [7] | Comment |
|---|---|---|
| BLE beacon | 1-5 years | Depends mainly on interval; modelled 2.4 years ideal at 1 s on a CR2032, 1.4-1.7 derated [9] |
| UWB tag | 3-12 months | Depends on update rate and motion; check datasheet |
| Active RFID | 3-7 years | Claim |
| Passive RFID | no battery | Powered by reader field |
| WiFi tag or phone | not given | Usually charged, not a coin-cell product |

Battery life is an update-rate decision: a tag at 10 Hz and a tag at 0.1 Hz are different products. Use motion-triggered updates wherever assets sit still for long periods. Ask for current measurements, not lifetime claims, and derate for cold, as in cold-chain sites [9]. For Channel Sounding, the required encrypted connection is an extra cost compared with broadcasting [10]; I found no reliable tag lifetime figure.

## Environment: NLOS, metal and liquids
Environment often decides more than headline accuracy. NLOS and multipath reduce ranging accuracy in time-based systems [11]. Measured degradation:

| Technology | Clear path | Obstructed or cluttered | Source |
|---|---|---|---|
| Channel Sounding PBR, p95 | 0.32-0.61 m | about 5 m (NLOS) | vendor test [12] |
| WiFi FTM | meter level at 80/160 MHz | about 5 m (NLOS) | [13] |
| BLE AoA, mean | 22 cm outdoors | 36.5 cm in a furnished room | small test [2] |

Rack aisles, machines and liquid-filled stock create NLOS. As rules of thumb, mount infrastructure high and in every aisle, since a clear line of sight beats a better chip, and put tags on a clear part of the asset or the upper body. RSSI fingerprints also suffer when people, metal objects or stock change the room [15]. For harsh metal sites, verify with a pilot before committing to any system; published results are mainly from offices and homes.

## Other technologies
| Technology | Published capability | Limits |
|---|---|---|
| Ultrasound | centimetre-level in text; range couple to tens of m [8] | heavily dependent on sensor placement [8] |
| Visible light | high potential accuracy, multipath-free [8] | needs line of sight, range affected by obstacles [8] |
| Acoustic on phones | about 0.9 m in 95% of tests, Beep system [8] | sound pollution, needs extra anchors [8] |
| ZigBee | suits sensor-network localization [8] | not on most user devices [8] |
| PDR and IMU | good for short gaps | drifts without an absolute reference |

Consider these only for a special reason, such as a radio-restricted area. They also lack mass-market tag ecosystems, so check supply before specifying them. Hybrids are often better: UWB in critical zones plus BLE elsewhere, or RFID at doors plus RTLS on vehicles. The omlox standard targets this hybrid case, with a UWB core zone and complementary BLE, RFID or GPS zones linked through a hub [14].

## Selection checklist before choosing
Ask these, in order:
1. What decision uses the location: coordinate, zone, passing event or inventory?
2. What error is acceptable at p95, and in what area?
3. Are tags people, vehicles, or goods? How many, how fast, and what update rate?
4. May tags be charged? If never, can they last the asset's lifetime?
5. Is existing WiFi or BLE infrastructure usable?
6. How much metal, liquid and stock changes?
7. Can power and data reach mounting points? Cabling can be a large cost (rule of thumb).
8. Is security needed? AoA CTE can be spoofed; Channel Sounding and 802.11az add protections [3][10][13].
9. Must vendors interoperate?

If the application asks for high accuracy only in a small part of the site, deploy precise infrastructure only there.

## Applying this in Sense Sim
Compare candidates on the same scene using `rtls_auto_deploy` with flags `uwb`, `ble`, `ble_aoa`, `wifi`, `rfid`, and read `plans` (`devices`, `coverage`, `predicted_p95_m`, `install_cost`, `targets_met`). `target_cm` applies to UWB. Other technologies use a fixed plan target: AoA 0.75 m and RTT 2.0 m, both relaxed by 1.5 during auto deploy; RSSI uses coverage only. Note the `target_applies_to` field in the result.

For each option, run `rtls_coverage` (`tech`, `height`, `cell`) and `rtls_run`, then compare per-technology `cep95_m`. Test fragility with `redundancy: true` and with `rtls_update_device` `enabled: false`.

`install_cost` is a relative mount and cable score (`cost_weight`, `head_end`, `colocate`), not money. Convert device counts to cost yourself, using quotes. Battery fields in profiles have no effect, and neither tag price nor capacity contention is modelled. Mention these limits when presenting a recommendation.

## Sources
1. An Experimental Evaluation of Indoor Localization in Autonomous Mobile Robots (Khoshrangbaf et al.) — Sensors, 2025 — https://pmc.ncbi.nlm.nih.gov/articles/PMC11991048
2. An Experimental Study on Direction Finding of Bluetooth 5.1: Indoor vs Outdoor — arXiv 2103.04121, 2021 — https://ar5iv.labs.arxiv.org/html/2103.04121
3. Dead on Arrival: An Empirical Study of The Bluetooth 5.1 Positioning System (Cominelli et al.) — arXiv 1909.08063, 2019 — https://ar5iv.labs.arxiv.org/html/1909.08063
4. Wi-Fi location: ranging with RTT — Android Developers, accessed 2026 — https://developer.android.com/develop/connectivity/wifi/wifi-rtt
5. Indoor Positioning with Wi-Fi Location: A Survey of IEEE 802.11mc/az/bk Fine Timing Measurement Research — arXiv 2509.03901, 2025 — https://arxiv.org/html/2509.03901v1
6. Location Fingerprinting With Bluetooth Low Energy Beacons (Faragher, Harle), abstract — IEEE JSAC, 2015 — https://www.cl.cam.ac.uk/~rkh23/site/publication/pub42
7. Indoor positioning system guide — AirPinpoint (vendor), accessed 2026 — https://airpinpoint.com/tech/indoor-positioning-system
8. A Survey of Indoor Localization Systems and Technologies (Zafari, Gkelias, Leung) — arXiv 1709.01015 — https://ar5iv.labs.arxiv.org/html/1709.01015
9. BLE Advertising Interval vs Battery Life (model) — Hubble (vendor guide), accessed 2026 — https://hubble.com/community/guides/ble-advertising-interval-vs-battery-life-how-to-model-the-tradeoff-before-you-build/
10. Bluetooth Core 6.0 feature overview — Bluetooth SIG, 2024 — https://www.bluetooth.com/core-specification-6-feature-overview/
11. NLOS Identification and Mitigation for Time-based Indoor Localization Systems (Nkrow et al.), abstract — ACM Computing Surveys, 2024 — https://scholars.cityu.edu.hk/en/publications/nlos-identification-and-mitigation-for-time-based-indoor-localiza/
12. Channel Sounding Performance Metrics, RTL library 8.2.0 — Silicon Labs (vendor), accessed 2026 — https://docs.silabs.com/rtl-lib/8.2.0/rtl-lib-channel-sounding-dev-guide/07-channel-sounding-performance-metrics
13. Performance comparison of 802.11mc and 802.11az Wi-Fi Fine Time Measurement protocols (Rajendran et al.) — arXiv 2511.17935, 2025 — https://arxiv.org/html/2511.17935v1
14. omlox, PI and FiRa join forces for seamless UWB interoperability — PI, 2025 — https://www.profibus.com/newsroom/press-news/omlox-pi-and-fira-join-forces-for-seamless-uwb-interoperability
15. Model-Based Localization and Tracking Using Bluetooth Low-Energy Beacons (Danis, Cemgil) — Sensors, 2017 — https://pmc.ncbi.nlm.nih.gov/articles/PMC5713487/
