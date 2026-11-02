---
title: BLE RSSI positioning - channels, path-loss fit, fingerprinting, accuracy, density, battery
domain: positioning
tags: BLE, Bluetooth Low Energy, RSSI, beacon, iBeacon, advertising channels, 37 38 39, path loss exponent, log-distance, shadowing, fingerprinting, trilateration, proximity, beacon density, battery, CR2032, measured power, rssi_at_1m
aliases: định vị BLE, định vị Bluetooth, beacon Bluetooth, cường độ tín hiệu RSSI, số mũ suy hao, lấy dấu vân tay tín hiệu, mật độ beacon, thời lượng pin beacon, BLE beacon positioning, Bluetooth asset tracking
verified: 2026-11-02
---
# BLE RSSI positioning

## BLE advertising channels and timing
BLE has 40 channels, each 2 MHz wide, spanning 2402 to 2480 MHz; advertising uses primary channels 37, 38 and 39 [1]. The advertising interval runs from 20 ms to 10.24 s in 0.625 ms steps, and the advertiser adds a 0-10 ms random delay after each event so that two devices with the same interval do not keep colliding [1]. Update rate and battery are therefore set by the same parameter. Published experiments span this range: 10 Hz at -16 dBm with the paper noting that most beacons stay under 10 Hz to save power [2], and about 1 Hz in a home study [3]. For planning, the position update rate cannot exceed the advertising rate of the tag, and any averaging window in the receiver lowers it further.

## BLE RSSI differs per advertising channel
Each advertising channel has its own gain and multipath, so RSSI is really three signals. Measured per-channel means:

| Study | Condition | Ch 37 | Ch 38 | Ch 39 |
|---|---|---|---|---|
| Zhuang 2016 [2] | static phone, about 5 m | -71.9 dBm (SD 2.4) | -68.1 dBm (SD 0.8) | -78.8 dBm (SD 2.7) |
| Naghdi 2022 [4] | 0 deg from LOS | -52.7 dBm | -56.2 dBm | -58.7 dBm |
| Naghdi 2022 [4] | 45 deg from LOS | -46.8 dBm | -55.0 dBm | -49.8 dBm |

The spread between channels reaches about 6 to 11 dB (arithmetic from the table), and no channel is consistently strongest. A 10 dB step at path-loss exponent 2 reads as a distance factor of about 3.2. Action: if the receiver reports the channel, calibrate and filter each channel separately. Naghdi found the three channels used separately outperformed pooled values in their neural-network method [4].

## Log-distance RSSI model and fitting
The standard model is RSSI(d) = RSSI_1m - 10 n log10(d) - wall losses + X, where d is in metres, n is the path-loss exponent and X is zero-mean Gaussian shadowing in dB. Inverting gives d = 10^((RSSI_1m - RSSI) / (10 n)). A first-order error estimate is sigma_d = d x sigma_dB x ln(10) / (10 n), so the error grows linearly with distance. With n = 2.2 and sigma = 4 dB, sigma_d is about 0.42 d, or 4.2 m at 10 m (arithmetic). Fit by placing a tag at known distances from about 1 m to the working range, recording many samples per point, then least-squares fitting RSSI against log10(d) on your own hardware. Do not reuse a fit from another site: environment, obstruction and beacon orientation all change the 1 m reading [5].

## Which path-loss exponent to expect indoors
Published fits vary and depend on whether walls are counted inside n. ITU-R P.1238 expresses distance loss as a coefficient N = 10 n that includes the typical building clutter. Values reproduced from the Recommendation: 1.8-2.0 GHz residential N = 28, office N = 30, commercial N = 22 [6]. That means n = 2.2 to 3.0 near 2 GHz. An exponent near 2.2 is defensible only if walls are charged separately, as an explicit per-wall loss. I found no authoritative measured table for BLE beacons specifically, and one measured BLE study argues that RSSI histograms are too inconsistent with distance to support trilateration at all [3]. Note that P.1238 has newer editions (P.1238-13, 09/2025, is in force) whose model form may differ; the coefficients above come from a secondary reproduction of an older edition [6][7].

## What measured power (rssi_at_1m) means
Measured power, also called RSSI at 1 m, is a reference value in the advertising data. It tells a receiver what signal strength to expect at 1 m and does not change the radio's output power [5]. It should be set from a measurement in the actual setting, because environment, obstruction and beacon orientation all shift the reading [5]. Sanity check: free-space loss at 1 m and 2440 MHz is about 40.2 dB (20 log10(2440) - 27.55), so a 0 dBm transmitter with 0 dBi antennas would read about -40 dBm, and -59 dBm at 1 m corresponds to about -19 dBm effective radiated power (arithmetic). Lower readings are normal with low TX settings, small antennas and body loss, but check the value against your actual TX setting.

## BLE fingerprinting versus RSSI ranging accuracy
Measured results, all from research deployments, none independently certified:

| Study | Setup | Method | Result |
|---|---|---|---|
| Faragher, Harle 2015 [8] | 19 beacons, about 600 m2, 1 per 30 m2 | fingerprint, one-shot | under 2.6 m, 95% of the time |
| same [8] | 1 per 100 m2 | fingerprint, one-shot | under 4.8 m, 95% |
| same [8] | existing WiFi, same area | fingerprint, one-shot | under 8.5 m, 95% |
| Zhuang 2016 [2] | 60 x 40 m office, 20 beacons | fingerprint + EKF | under 3.04 m, 90% |
| same [2] | 8 beacons | fingerprint + EKF | under 4.94 m, 90% |
| same [2] | 20 / 8 beacons | propagation model + EKF | under 3.99 m / 8.00 m, 90% |
| Naghdi 2022 [4] | blind test points | uncorrected RSSI / fingerprint | RMSE 6.11 m / 3.49 m |
| Khoshrangbaf 2025 [9] | 431 m2, three rooms, weighted average | BLE static / mobile | mean 1.92 m / 3.40 m |

Takeaway: fingerprinting usually beats a path-loss model, and the gap widens as beacons get sparse [2].

## Fingerprint survey density and upkeep
Fingerprint accuracy depends directly on survey density. In a 6.66 x 5.36 m occupied living room with beacons at about 1 Hz, mean error was 0.66 m (median 0.52 m) with 50 fingerprint locations, 1.41 m with 15, and 2.01 m with 8 [3]. That room is much smaller than an industrial site, so use the trend, not the absolute values. The survey is also a snapshot: furniture, people, metal objects and other radios were named as likely interference sources in the same study [3]. Treat the survey as a maintained asset. Budget a resurvey when racks, stock or layouts change, and keep a holdout set of test points to detect drift. If the site changes daily, such as a warehouse with changing stock, prefer a model-based or direction-finding system over a one-time survey.

## BLE beacon density guidance
Measured density data for fingerprinting: 1 beacon per 30 m2 gave under 2.6 m (95%) and 1 per 100 m2 gave under 4.8 m (95%) in the same testbed [8]. In a 2400 m2 office, going from 20 beacons (about 120 m2 each, arithmetic) to 8 beacons (about 300 m2 each) moved the 90% error from under 3.04 m to under 4.94 m with fingerprinting, and from under 3.99 m to under 8.00 m with a path-loss model [2]. A vendor guide suggests 1 beacon per 50-100 m2 for room-level and 1 per 5 m2 for precise positioning; treat these as vendor claims [10]. Practical choice: start near 1 per 30-100 m2, require at least three beacons audible at every point for lateration (rule of thumb), and verify rather than assume, since results come from offices rather than metal-heavy warehouses.

## BLE beacon and tag battery life
Advertising interval is the primary lever. The vendor model below is calculated, not measured, for an nRF52832 on a CR2032 at 0 dBm, with about 1.5 ms per 3-channel event at about 5.3 mA, 1.5 uA sleep and 200 mAh usable [11]:

| Interval | Average current | Ideal life |
|---|---|---|
| 100 ms | about 81 uA | about 3.4 months |
| 1 s | about 9.4 uA | about 2.4 years |
| 10 s | about 2.3 uA | about 9.9 years, beyond self-discharge limits |

The same source advises derating to 0.6-0.7 of the ideal figure, giving about 1.4-1.7 years at 1 s, and notes that LDO instead of DC-DC regulation raises TX current about 60% [11]. Life = capacity / average current. Measure on real hardware with a power profiler before committing; a scanning receiver is a different case, and phone scanning has its own power cost [8].

## Applying this in Sense Sim
Profiles: `ble-beacon`, `ble-tag` (see `rtls_list_profiles`). Place with `rtls_add_device`, or plan with `rtls_optimize_anchors` (tech `ble`, `count`, `target_coverage`, `apply`) or `rtls_auto_deploy` with `ble: true`. Check with `rtls_coverage` (tech `ble`, `height`, `cell`) and `rtls_run`.

The simulator uses log-distance RSSI ranging, then weighted least squares; it has no fingerprinting model and no per-channel RSSI. Defaults: `path_loss_exp` 2.2, `shadow_sigma_db` 4, `wall_loss_db` 6, `rssi_at_1m` -59, `sensitivity_dbm` -95, `update_hz` 2. Consequences:
- RSSI is driven by the tag's `rssi_at_1m`; `tx_power_dbm` on BLE profiles does not affect it.
- Walls are not corrected: one 6 dB wall at n = 2.2 makes a tag look about 1.87 times further away.
- Fewer than 3 heard nodes falls back to proximity (the strongest node).
- `battery_mah` and `current_ma` have no effect on the simulation; compute battery life by hand, and airtime contention is not modelled.

Measured fingerprinting usually beats a path-loss model [2], so these predictions are not a forecast for a fingerprinting product. I could not tie the 4 dB shadowing default to a measurement, so treat tight RSSI predictions with caution. RSSI plans have a default target of 3.5 m.

## Sources
1. Device Discovery (BLE get-started guide) — Espressif, ESP-IDF docs, accessed 2026 — https://docs.espressif.com/projects/esp-idf/en/latest/esp32c2/api-guides/ble/get-started/ble-device-discovery.html
2. Smartphone-Based Indoor Localization with Bluetooth Low Energy Beacons (Zhuang et al.) — Sensors, 2016 — https://pmc.ncbi.nlm.nih.gov/articles/PMC4883287
3. Model-Based Localization and Tracking Using Bluetooth Low-Energy Beacons (Danis, Cemgil) — Sensors, 2017 — https://pmc.ncbi.nlm.nih.gov/articles/PMC5713487/
4. Combining Multichannel RSSI and Vision with Artificial Neural Networks to Improve BLE Trilateration (Naghdi, O'Keefe) — Sensors, 2022 — https://pmc.ncbi.nlm.nih.gov/articles/PMC9227766
5. What is iBeacon Measured Power? — BeaconZone (vendor blog), accessed 2026 — https://www.beaconzone.co.uk/blog/what-is-beacon-measured-power/
6. ITU model for indoor attenuation (reproduces ITU-R P.1238 tables) — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/ITU_model_for_indoor_attenuation
7. Recommendation ITU-R P.1238 edition list — ITU, accessed 2026 — https://www.itu.int/rec/R-REC-P.1238/en
8. Location Fingerprinting With Bluetooth Low Energy Beacons (Faragher, Harle), abstract — IEEE JSAC, 2015 — https://www.cl.cam.ac.uk/~rkh23/site/publication/pub42
9. An Experimental Evaluation of Indoor Localization in Autonomous Mobile Robots (Khoshrangbaf et al.) — Sensors, 2025 — https://pmc.ncbi.nlm.nih.gov/articles/PMC11991048
10. Indoor positioning system guide — AirPinpoint (vendor), accessed 2026 — https://airpinpoint.com/tech/indoor-positioning-system
11. BLE Advertising Interval vs Battery Life (model) — Hubble (vendor guide), accessed 2026 — https://hubble.com/community/guides/ble-advertising-interval-vs-battery-life-how-to-model-the-tradeoff-before-you-build/
