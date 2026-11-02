---
title: UWB chips and tag hardware (Qorvo DW1000, DW3000, NXP Trimension, battery life)
domain: uwb
tags: qorvo, decawave, dw1000, dwm1000, dw3000, dw3110, dw3120, dw3210, dw3220, qm33120, nxp trimension, sr040, sr150, uwb chip, transceiver, sensitivity, link budget, range, tx power, current consumption, tag battery life, coin cell, cr2032, deep sleep
aliases: chip UWB, module UWB, dòng tiêu thụ, tuổi thọ pin tag, pin tag UWB, tầm phủ UWB, độ nhạy thu, công suất phát, chọn phần cứng UWB, UWB battery, how long does a UWB tag last, UWB range
verified: 2026-11-02
---
# UWB chips and tag hardware

## Qorvo DW3000 family variants
The DW3000 family (formerly Decawave) implements IEEE 802.15.4-2015 and 802.15.4z BPRF on channels 5 and 9, at 850 kbps and 6.8 Mbps, and supports TWR, TDoA and PDoA [1].

| Variant | Package | PDoA port | Notes |
|---|---|---|---|
| DW3110 | WLCSP, 52 balls | No | Highest TX output |
| DW3120 | WLCSP, 52 balls | Yes | About 1 dB less TX, about 1.4 dB less RX sensitivity |
| DW3210 | QFN, 40 pads | No | About 2 dB less TX than WLCSP |
| DW3220 | QFN, 40 pads | Yes | QFN loss plus PDoA switch loss |

Other figures: operating range -40 to +85 degrees C, packet up to 1023 bytes, 38.4 MHz crystal specified at +/-20 ppm with a +/-20 ppm on-chip trim range [1]. Choose WLCSP for maximum range, and PDoA variants only when a bearing is needed.

## DW3000 receive sensitivity and link budget
Typical sensitivity at 1% packet error rate, 25 degrees C, on DW3110/DW3210 [1]:

| Configuration | Channel 5 | Channel 9 |
|---|---|---|
| 850 kbps, 1024-symbol preamble | -100 dBm | -99.3 dBm |
| 6.8 Mbps, 64-symbol preamble, 64-symbol STS | -94.3 dBm | -93.2 dBm |
| No data, 64-symbol preamble, 64-symbol STS | -98.6 dBm | -98.0 dBm |

At -41.3 dBm/MHz with 0 dBi antennas and no losses, the datasheet quotes typical link budgets of 86 to 98 dB on channel 5 and 85 to 97 dB on channel 9 (the table's alignment in the copy read was unclear, so treat rows as approximate) [1].

Ideal free-space range from a budget L dB at 6489.6 MHz: d = 10^((L - 76.24 + 27.55) / 20) metres (computed from free-space path loss). A budget of 80.7 dB gives 40 m; 86 dB gives 73 m; 91 dB gives 130 m. Indoor range is far shorter because of body, shelf and antenna losses. Vendors plan anchor spacing of 15 to 40 m [3]. No primary range measurement in a warehouse was opened.

## DW3000 current consumption profile
All at 25 degrees C with supplies at 3.0 V [1]:

| State | Current |
|---|---|
| Deep sleep | 260 nA |
| Idle RC | 8 mA |
| Idle PLL, channel 5 / channel 9 | 18 / 32 mA |
| TX, lowest BOM, channel 5 | 48 mA (SHR), 40 mA (data) |
| RX, lowest BOM, channel 5 | 70 mA preamble hunt, 70 to 78 mA during a frame |
| Peak RX, channel 5 / channel 9 | 72 / 88 mA |

Time costs: oscillator start-up about 1000 us at 1.5 mA from deep sleep, 135 us SHR plus 16 us data for a 128-symbol preamble frame at 6.8 Mbps, 170 us per frame in DS-TWR. A high-efficiency regulator on VDD3 lowers peak currents significantly [1].

RX draws more than TX, and receiving is usually longer than the frame because the radio must be listening before it arrives. Tags that listen, including DS-TWR and downlink TDoA tags, spend most energy in receive. Battery life depends on reducing listen windows, not on transmit power.

## UWB tag battery life estimate from charge per fix
Charge per event from the DW3000 profile at 6.8 Mbps, channel 5, with a 128-symbol preamble (computed from [1], chip only, no MCU, sensors or regulator losses):

| Event | Charge |
|---|---|
| Wake up, from 1000 us at 1.5 mA plus PLL | about 2 uC |
| Uplink TDoA blink (135 us at 48 mA + 16 us at 40 mA) | about 9 uC total |
| DS-TWR with one anchor: 2 TX and 1 RX at 170 us, plus 2 x 0.35 ms idle | about 42 uC |
| DS-TWR fix with 4 anchors | about 170 uC |

Average current = charge per fix * rate + sleep current. Life = capacity / average current.

| Mode and rate | Average | 1000 mAh life (ideal) |
|---|---|---|
| TDoA 1 Hz | about 9 uA | over 10 years (self-discharge will dominate) |
| TDoA 5 Hz | about 45 uA | about 2.5 years |
| DS-TWR 4 anchors, 1 Hz | about 0.17 mA | about 8 months |
| DS-TWR 4 anchors, 10 Hz | about 1.7 mA | about 24 days |

A 64-symbol preamble nearly halves blink air time. Pozyx states a few years at 1 Hz for its TDoA tags and high consumption for TWR [3], consistent with the direction of this table. Add an MCU sleep current and motion-triggered update rates in a real estimate: stationary assets often blink rarely.

## Qorvo DW1000 and DWM1000 legacy radios
The DW1000 implements IEEE 802.15.4-2011 HRP. It is the earlier generation behind many deployed systems and research papers, including Pozyx hardware and the Bitcraze Loco Positioning System [2][4][5]. Key differences from DW3000:

| Item | DW1000 | DW3000 |
|---|---|---|
| Channel 9 | No [2] | Yes [1] |
| 802.15.4z BPRF, STS | No [2] | Yes [1] |
| Timestamp resolution | About 15.65 ps [6] | Not stated in the sections read |
| Compatibility | Interoperates with DW3000 on channel 5 only, at 850 kbps and 6.8 Mbps [1] | Same |

Guidance: DW1000 hardware cannot be used where only channel 9 is permitted, such as Vietnam's licence-exempt list or new China type approvals. Accuracy numbers from DW1000 papers remain a useful reference for DW3000 designs, but they should not be copied as a guarantee.

## NXP Trimension and other UWB chips
| Chip | Role and claims | Source |
|---|---|---|
| NXP SR040 | IEEE 802.15.4z, FiRa certified. Coin-cell tags and trackers; as a TDoA tag it sends blinks only | [7] |
| NXP SR150 | IEEE 802.15.4z, FiRa certified. Dual RX for AoA, fixed anchor role, 3D AoA capable | [8] |
| Apple U1 | 15.4z HRP, works with CCC, FiRa and Apple Nearby Interaction | [2] |
| 3db Access, Zebra | LRP PHY, not HRP-interoperable | [2] |

NXP's public product pages do not state current, sensitivity or channels; those need a datasheet behind login. A QM33120W data sheet from Qorvo was found but could not be opened for this knowledge base, and QM35-series specifications were not verified. Do not quote numbers for these parts from memory. Ask for the current datasheet.

## Practical UWB hardware selection checklist
Selection order for an RTLS project:

1. Region first: which channel is legal? Channel 9 is needed in Vietnam and for new approvals in China.
2. Method: DS-TWR, uplink TDoA, or downlink TDoA (see UWB ranging methods); tag battery budget follows from it.
3. Tag count and rate: check blink capacity before picking data rate. Shorter frames carry more tags, longer preambles reach further.
4. Security: require STS only where location grants access.
5. Environment: antenna and tag mounting near metal, and tag placement on a body or vehicle.
6. Interoperability: FiRa or Apple profile only if phones must take part.
7. Calibration: confirm the vendor supplies per-unit antenna delay, since an uncalibrated DW3000 is about +/-15 cm versus +/-6 cm after calibration in line of sight [1].

## Applying this in Sense Sim
- `rtls_list_profiles` shows `uwb-anchor`, `uwb-tag` and `uwb-tag-tdoa`. They are generic: pick values to match a real chip, not a brand.
- Link range is computed from `uwb_tx_dbm_mhz`, `sensitivity_dbm` (default -95 dBm), `freq_mhz` and any antenna gain, capped by `max_range_m` (40 m on `uwb-anchor`). The default budget is 80.7 dB, well inside the datasheet's 86 to 98 dB, which is a reasonable indoor margin.
- To model a long-preamble DW3000 design, add a profile JSON in `%APPDATA%\SenseSim\devices` with `sensitivity_dbm` -100 and a larger `max_range_m`, then use it via the `profile` argument of `rtls_optimize_anchors` or `rtls_add_device`.
- `rtls_auto_deploy` `anchor_tx_dbm_mhz` lowers transmit power, with -53.3 dBm/MHz giving a quarter of the range.
- Do not use `current_ma` and `battery_mah` from a profile as a battery answer: the simulator does not read them, and 0.08 mA for a 10 Hz TWR tag is far below the roughly 1.7 mA computed above. Give the user the estimate from charge per fix instead.

## Sources
1. DW3000 Datasheet, version 1.1 — Decawave (Qorvo), 2020 — https://www.mouser.com/datasheet/2/412/DW3000_Data_Sheet-3532945.pdf
2. An Overview of UWB Standards and Organizations (IEEE 802.15.4, FiRa, Apple), D. Coppens et al. — IEEE Access, 2022 — https://arxiv.org/abs/2202.02190
3. TDOA and TWR positioning protocols explained; Choosing the anchors' locations, Pozyx (vendor) — checked 2026-11-02 — https://pozyx.io/pozyx-academy/positioning-protocols-explained and https://installation-docs.pozyx.io/choosing-the-anchors-locations
4. Evaluation and Comparison of Ultrasonic and UWB Technology for Indoor Localization in an Industrial Environment, A. L. Cretu-Sircu et al. — Sensors, 2022 — https://vbn.aau.dk/ws/files/482474789/sensors_22_02927.pdf
5. UTIL: An Ultra-wideband Time-difference-of-arrival Indoor Localization Dataset, W. Zhao et al. — arXiv, 2022 — https://arxiv.org/abs/2203.14471
6. Robust and Scalable Techniques for TWR and TDoA based localization using Ultra Wide Band Radios, R. Ramesh et al. — arXiv, 2020 — https://arxiv.org/abs/2008.04248
7. Trimension SR040 product page — NXP, checked 2026-11-02 — https://www.nxp.com/products/wireless/secure-ultra-wideband-uwb/trimension-sr040-reliable-uwb-solution-for-iot:SR040
8. Trimension SR150 product page — NXP, checked 2026-11-02 — https://nxp.com/products/SR150
