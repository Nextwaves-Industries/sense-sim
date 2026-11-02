---
title: Thermal noise, receiver sensitivity and coexistence
domain: rf
tags: thermal noise, kTB, noise floor, noise figure, NF, SNR, SINR, receiver sensitivity, processing gain, BLE sensitivity, WiFi sensitivity, MCS, interference, coexistence, 2.4 GHz, advertising channels, 37 38 39, UWB interference, 6 GHz, weak link threshold
aliases: nhiễu nhiệt, mức nhiễu nền, hệ số nhiễu, tỷ số tín hiệu trên nhiễu, độ nhạy máy thu, can nhiễu, nhiễu đồng kênh, cùng tồn tại, wifi nhiễu ble, noise floor
verified: 2026-11-02
---
# Thermal noise, receiver sensitivity and coexistence

## Thermal noise floor by channel bandwidth

Thermal noise power is kTB: at 290 K the density is about -174 dBm/Hz [1]. Receiver noise floor = -174 + 10 log10(B in Hz) + NF dBm, where NF is receiver noise figure [1]. Each tenfold increase in bandwidth raises the floor 10 dB, and each dB of NF raises it 1 dB [1].

| Signal bandwidth | kTB power |
|---|---|
| 125 kHz | -123 dBm |
| 1 MHz (BLE 1M) | -114 dBm |
| 2 MHz (BLE 2M) | -111 dBm |
| 20 MHz (WiFi) | -101 dBm |
| 40 MHz (WiFi) | -98 dBm |
| 80 MHz (WiFi) | -95 dBm |
| 499.2 MHz (UWB) | -87 dBm |

Values are computed, without NF. Wider signals gain ranging resolution and rate, but pay for it with a higher noise floor. Indoor links are usually interference-limited rather than noise-limited in busy unlicensed bands.

## Receiver sensitivity from noise figure and required SNR

Sensitivity = -174 + 10 log10(B) + NF + SNRmin dBm [1]. Sensitivity is only meaningful with its stated rate and error criterion.

Check of a real radio: the nRF52840 specifies -95 dBm at BLE 1 Mbps and -103 dBm at 125 kbps [2]. Against -114 dBm thermal in 1 MHz, -95 dBm implies NF + SNRmin of about 19 dB (computed), and the eight-fold lower rate is 8 dB more sensitive. Lower rate with coding is the main way to trade throughput for range.

Datasheet sensitivities are measured conducted at a stated temperature and supply, usually with an ideal source. In a product, antenna mismatch, enclosures and body proximity consume some of that figure. The nRF52840 brief also states +8 dBm maximum transmit power [2], so such a radio has a 103 dB link budget at 1 Mbps (computed).

## WiFi receiver sensitivity by rate

IEEE 802.11 Clause 17 minimum sensitivity at 20 MHz is -82 dBm for 6 Mbps and -65 dBm for 54 Mbps, as cited in IEEE working-group discussion [3]. Commercial radios usually beat it.

| Mode (20 MHz) | Standard minimum [3] | ATWILC3000 typical [4] |
|---|---|---|
| OFDM 6 Mbps | -82 dBm | -90 dBm |
| OFDM 24 Mbps | not checked | -82 dBm |
| OFDM 54 Mbps | -65 dBm | -73 dBm |
| 802.11n MCS 0 | not checked | -89 dBm |
| 802.11n MCS 7 | not checked | -70 dBm |

Bandwidth doubling costs about 3 dB, following kTB. For ranging with WiFi RTT, the management and FTM frames must be decoded, so reach tracks low-rate sensitivity; high-rate data requires a much stronger signal. Where data traffic shares the AP, plan positioning coverage to a stronger level than bare sensitivity; -75 dBm is the Sense Sim weak-link threshold and a reasonable starting point (rule of thumb).

## UWB receiver budget and processing gain

UWB receivers see far higher noise, since kTB in 499.2 MHz is -87 dBm before NF, yet can detect at lower levels than that. A -95 dBm sensitivity used as the Sense Sim default implies operation below the raw noise floor, so correlation processing gain must make up the difference (computed comparison). Exact sensitivity varies by preamble length, data rate and chip, and no manufacturer table was verified here, so confirm the value against the actual chip datasheet.

Ranging quality depends on first-path SNR, not total received power. Strong later reflections can make total power look healthy while the first path is weak. A first-path power well below total receive power is the common signature of NLOS, so a diagnostic should compare the two rather than only reading RSSI.

## SINR and when interference replaces noise

Decoding depends on SINR: signal power divided by noise plus interference power, in linear units. When interference exceeds noise, sensitivity no longer predicts range. A receiver sitting 10 dB above sensitivity fails if an interferer sits within the required C/I.

Calculate it from the same budget: received interferer power = its EIRP - path loss - walls, reduced by any bandwidth mismatch and channel separation. In dBm terms, an adjacent access point at 10 m and 20 dBm EIRP at 2.44 GHz gives about -40 dBm (computed from FSPL), far above a -95 dBm BLE sensitivity. Coexistence therefore relies on time and frequency separation, not power margin. Airtime sharing limits update rate rather than coverage, so symptoms show as dropouts, latency and uneven update intervals instead of reduced range.

## BLE and WiFi coexistence in 2.4 GHz

TI guidance: BLE advertising channels 37, 38 and 39 were chosen to avoid the three most common WiFi channels 1, 6 and 11 [5]. Coexistence problems can still occur, and the impact on overlapping channels depends on RSSI at the receiver [5]. WiFi may transmit up to 23 dBm against a 10 dBm maximum for BLE, and occupies wider spectrum, so a beacon directly next to a WiFi radio can be disturbed by its side emissions [5].

Beacons also collide with each other: even a small number of active beacons can lose packets [5]. No opened source gives a general packet-loss figure, so measure on site.

Actions:
- Keep at least some separation between BLE gateways and WiFi AP antennas.
- Prefer WiFi 5 GHz for traffic in BLE areas.
- Do not plan 2 Hz tag updates at high density without testing.
- Fixed advertising channels do not benefit from connection-mode channel hopping.

## UWB coexistence with narrowband systems

UWB emissions are extremely low density. At the -41.3 dBm/MHz limit, 10 m of free space at 6.49 GHz brings it to about -110 dBm/MHz, near the -114 dBm/MHz thermal level (computed). So a UWB anchor rarely disturbs a narrowband receiver at that distance, though a very close co-located radio may need testing.

The reverse is more important. A narrowband transmitter in band sits at far higher density and can desensitize a UWB receiver. In many regions, 6 GHz WiFi overlaps 6489.6 MHz channel 5; that overlap was not quantified against an opened source, so treat it as a survey item when WiFi 6E or 7 is present. Mitigations are a survey for active 6 GHz APs, physical separation of antennas, and considering another UWB channel in the product where available.

## Interference sources to survey on site

An RF survey before installation should list these.

| Source | Band | Note |
|---|---|---|
| WiFi 2.4 GHz | 2.4 GHz | Overlaps BLE; highest power, TI data [5] |
| BLE beacons, other vendors | 2.4 GHz | Collisions increase with density [5] |
| Microwave ovens and appliances | 2.4 GHz | Named as interferers by TI [5] |
| WiFi 6 GHz | 5.925 GHz upward where allowed | Possible overlap with UWB channel 5 |
| Other RFID readers | UHF | Dense-reader operation, hop sets |
| Cellular repeaters and DECT | Various | Region-specific; check spectrum plan |

Microwave oven duty and loss figures were not verified here. Use a spectrum analyser or sniffers at representative hours including shift peaks. A daytime survey in an empty building will underestimate the interference, so note when each sample was taken.

## Choosing thresholds for weak-link alerts

Sense Sim flags a heard link as weak below these levels, from rf.rs: UWB -100 dBm first path, BLE -85 dBm, WiFi -75 dBm, RFID -70 dBm backscatter. Compare with sensitivity defaults: BLE -95, WiFi -82 or -85, RFID -85, UWB -95.

So BLE weak leaves 10 dB, WiFi 7-10 dB and RFID 15 dB of margin, sensible for sigma 4-6 dB fade allowance. The UWB threshold sits below the -95 dBm sensitivity, which is inconsistent: a UWB link can be flagged OK at -96 dBm although a real receiver would be at or below sensitivity. For UWB, judge first-path values in rtls_status against sensitivity plus your fade allowance, not against the -100 dBm flag.

## Applying this in Sense Sim

Sense Sim models no explicit noise floor, SINR or co-channel interference. Hearing is a threshold test against sensitivity_dbm with Gaussian shadowing, plus dropout probability, and for UWB a range test. Therefore rtls_coverage predicts coverage in a clean spectrum, and real sites will do worse in busy bands.

To model a noisy site approximately, compare devices by margin, not presence: require margins above fade allowance using rf values returned by rtls_status, including avg_dbm, min_dbm, weak_links and per-anchor rows. Use rtls_update_device with enabled false to test loss of an interfered device, or rtls_optimize_anchors with redundancy true to plan for it. Check BLE density and update_hz in rtls_list_profiles; collisions are not simulated, so warn the user about them.

## Sources

1. What is noise floor for radio receivers — Electronics Notes, accessed 2026 — https://www.electronics-notes.com/articles/radio/radio-receiver-sensitivity/what-is-noise-floor.php
2. nRF52840 SoC product brief v2.3 — Nordic Semiconductor, via Rutronik, 2020 — https://www.rutronik.com/fileadmin/Rutronik/Supplier/Nordic/Micropage_2020/nRF52840_SoC_PB_2.3.pdf
3. IEEE 802.19 list discussion quoting 802.11 Clause 17 sensitivity — IEEE 802.19 mailing list — https://ieee802.org/19/arc/stds-802-19list/msg00650.html
4. ATWILC3000 IEEE 802.11 receiver performance — Microchip, accessed 2026 — https://onlinedocs.microchip.com/oxy/GUID-04DF72D6-FAA7-410A-A90B-E7F4B7F3D676-en-US-2/GUID-EA53B129-6FBC-49B5-B0F9-0C763D3DA312.html
5. SWRA475A, Bluetooth low energy beacons, coexistence section — Texas Instruments, 2016 — https://www.ti.com/document-viewer/lit/html/SWRA475A/coexistence-swra4755506
