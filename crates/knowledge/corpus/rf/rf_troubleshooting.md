---
title: RF troubleshooting by symptom
domain: rf
tags: troubleshooting, diagnosis, symptom, dead zone, weak signal, no signal, NLOS bias, dropouts, tags under 3 anchors, RFID read failure, RSSI noise, interference, coverage hole, blocked link, wrong floor, coverage mismatch
aliases: khắc phục sự cố, chẩn đoán, mất sóng, sóng yếu, vùng chết, không đọc được thẻ, sai vị trí, nhảy vị trí, thiếu anchor, tín hiệu chập chờn, sai tầng
verified: 2026-11-02
---
# RF troubleshooting by symptom

## Diagnosis order for any RF problem

Work from cheap checks to expensive changes.
1. Confirm which technology and device: rtls_list_devices, rtls_status.
2. Separate no link from a bad link: compare blocked_links, weak_links, success_pct, nlos_pct, min_dbm and nlos_bias_cm.
3. Look at geometry: rtls_coverage at the tag height for that tech.
4. Run a budget by hand, with material loss for the actual frequency.
5. Change one thing, and rerun with rtls_run with the same seed set through rtls_control.

Fix order: move a device, raise a mount, add a device, then consider higher antenna gain. In a real building add a survey step, because the simulator has no multipath or interference. Never increase power first; regulation usually limits it, and it does not fix geometry.

## Symptom: large dead zone behind a wall or room

Likely causes, in order: heavy wall on all paths, too few devices in that room, or a wall modelled as thicker than reality.

Check: for 10 cm concrete use 15-25 dB at 6.5 GHz and 8-23 dB at 2.4 GHz [1][2]. Sum the budget: UWB has only 32 dB after the first metre, so one such wall allows about 2-7 m.

Fix: give each concrete-walled room its own infrastructure. Do not depend on through-wall links for UWB. Use rtls_optimize_anchors with tech, count, height and mount structure, then re-run rtls_coverage. Confirm the wall material on site: drywall loses about 1 dB per sheet [1], and hollow block about 11-18 dB [1], so modelling errors alone can create or hide a dead zone.

## Symptom: dead zones behind racks and machinery

Likely cause: metal shadows. Metal is effectively opaque, and energy arrives only by diffraction, reflection or gaps [2]. Antennas mounted below rack height leave ray and Fresnel zone obstructed.

Check: first Fresnel radius at midpoint over 10 m is 0.91 m at 920 MHz, 0.56 m at 2.44 GHz and 0.34 m at 6.49 GHz [3]. Compare it to clearance over the rack tops. A ray that just grazes a top already loses 6 dB [3].

Fix: raise mounts above clutter, which P.1238 advises for attaining line of sight [4]. Place devices in aisle ends so rays run along aisles, where corridors show exponents near 1.8 [4]. In 3GPP factory data, moving an antenna above clutter cuts the exponent from about 3.6 to about 2.2 [5]. Use rtls_optimize_anchors with mount structure, or specify a higher height.

## Symptom: UWB position biased or jumping behind obstacles

Likely cause: NLOS first path arriving late. Material delay alone gives about 13 cm per 10 cm concrete (computed from P.2040 permittivity [2]), and a missed first path adds more, as the receiver locks to a reflection.

Check: nlos_pct and nlos_bias_cm in rtls_status, per-anchor bias_cm and outliers, and nlos_links in rtls_list_devices.

Fix: add LOS anchors so at least 3 remain clear (4 for full 3D). Enable robust solving with rtls_control using robust true. If one anchor shows large bias, move it first. Do not correct a fixed offset: bias depends on the path. When many anchors are NLOS, geometry improvement outperforms any filter. Kalman smoothing only hides jumps and adds lag.

## Symptom: tags heard by fewer than three anchors

The RF summary tags_under3 counts UWB or BLE tags with fewer than three heard links, so they have no reliable 2D fix.

Likely causes: spacing too large, tag height differing from the planned height, or NLOS exceeding max_walls for UWB. For UWB, wall_eq above max_walls (default 2) rejects a link regardless of distance, so one 20 cm concrete wall (wall_eq 4) blocks it, and metal or liquid blocks it at any thickness.

Fix: re-run rtls_coverage with the actual tag height. Plan several heights with rtls_auto_deploy heights, or rtls_optimize_anchors heights. Use redundancy true when fix continuity through one device failure matters. Test by setting a device enabled false with rtls_update_device, then rtls_run.

## Symptom: RSSI distance is noisy or wrong

This is mostly physics, not a bug. Distance error grows with distance: 1-sigma error is about d x sigma x 2.303 / (10 n). At d = 10 m, sigma = 4 dB and n = 2.2, it is about 4.2 m, and at sigma = 6 dB with n = 3 about 4.6 m (computed from the log-distance model). Small-scale nulls add up to 20 dB in NLOS spots [6], and half a wavelength at 2.44 GHz is only 6 cm.

Fix: do not read one RSSI sample as a distance. Average across time and BLE advertising channels, use zone-level logic instead of metric positioning, or change technology. For accuracy finer than about 3 m, use BLE AoA, WiFi RTT or UWB. Calibrate rssi_at_1m in the real tag and enclosure, since each 1 dB of offset causes about 11% range error at n = 2.2 (computed).

## Symptom: UHF RFID tags not reading or reading intermittently

Check forward link first, as passive tags are usually power-limited. Default rfid-reader forward budget is 59 dB (39 dBm EIRP + 2 dBi tag - (-18 dBm)). Look at angle: gain falls 3 dB at the beam edge, and more outside it.

Likely causes:
- Tag beyond antenna beam, or wrong yaw_deg.
- Metal or liquid on the path. Sim metal costs 48 dB; on real tags detuning also matters.
- Fading nulls. At K = 7 dB allow 6-10 dB [6]; the sim uses only 2 dB.
- Many tags sharing the field, reducing reads per tag.

Fix: adjust yaw_deg with rtls_update_device, add a portal pair, or add readers using rtls_optimize_anchors with tech rfid. Add a second antenna angle for tagged goods in depth. Do not raise power above the regional EIRP limit [7].

## Symptom: coverage good in simulation but poor on site

Expect this when margin is thin, as the simulator omits reflections, diffraction, noise floor, interference and body blockage.

Likely causes:
- Real walls heavier than modelled, such as coated glass at about 24 dB [5] or foil-backed insulation.
- People blocking LOS, 6-8 dB at 1.7 GHz [4].
- Interference in 2.4 GHz [8].
- Antenna enclosures, cable loss, and detuning.
- A material name mapped to the wrong class, or unknown objects taken as generic.

Fix: add 6-10 dB of design margin and re-plan, survey with real radios at 5-10 points, and compare with predicted rtls_coverage values. Calibrate a key material using the FDTD Use in the floor map action in the UI, then rerun coverage.

## Symptom: dropouts or update gaps with strong signal

If avg_dbm and min_dbm look healthy in practice but packets vanish, suspect interference rather than coverage. On 2.4 GHz, WiFi outpowers BLE up to 23 dBm against 10 dBm, and beacons collide with each other [8]. Static links can also show bursty fading from moving people [4].

Check: compare hourly patterns with WiFi load. Try a spectrum capture during shift peaks.

Fix: separate BLE gateways from WiFi antennas, move data traffic to 5 GHz, reduce beacon density or update rate, and add more receiving gateways so packets have more chances. Note that the simulator has only a fixed dropout rate per profile and no collisions, so a simulation cannot reproduce or rule out this symptom. Say so plainly when reporting results.

## Symptom: tag appears on the wrong floor or room

Likely cause: signal leaks through the slab or an opening, and reports a stronger signal from a device on another level. Floor loss is limited: P.1238 office values are 14 dB for one floor at 2.4 GHz, 16 dB at 5.2 GHz, 22 dB at 5.8 GHz [4], and multiple floors saturate because signals find outside paths [4].

With sigma 4-6 dB shadowing, a 14 dB floor does not guarantee correct floor selection when a same-floor device is 10 m away and an upper device is directly overhead.

Fix: ensure at least 3 same-floor devices are nearer than any other-floor device, restrict solving per floor in application logic, use 3D fixes with dim3 true in rtls_auto_deploy, and check doorways or atriums. A slab must exist as a scene object for the simulator to attenuate between floors.

## Applying this in Sense Sim

Standard diagnostic sequence: rtls_list_profiles to read the radio parameters, rtls_list_devices for per-tag links, nlos_links and errors, and rtls_status for RF summary and per-anchor rows (in_range, heard, used, success_pct, avg_dbm, min_dbm, nlos_pct, outliers, bias_cm). Anchor status is idle, no signal, weak or ok. Then rtls_coverage with tech, height and cell for the map; a finer cell costs more time.

Then rtls_optimize_anchors (profile, tech, count, height, heights, target_coverage, target_p95_m, mount, redundancy, apply) to propose changes before applying. Confirm with rtls_run (seconds, reset). For repeatable comparisons set rtls_control seed first. Tools cannot edit wall_loss_db or sensitivity directly: report the needed value and ask the user to change the device JSON.

## Sources

1. NISTIR 6055, Electromagnetic Signal Attenuation in Construction Materials — W. C. Stone, NIST, 1997 — https://nvlpubs.nist.gov/nistpubs/Legacy/IR/nistir6055.pdf
2. Recommendation ITU-R P.2040-4, Effects of building materials and structures on radiowave propagation in the range of 1 MHz to 450 GHz — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.2040-4-202509-I!!PDF-E.pdf
3. Recommendation ITU-R P.526-16, Propagation by diffraction — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.526-16-202511-I!!PDF-E.pdf
4. Recommendation ITU-R P.1238-13, Propagation data and prediction methods for the planning of indoor radiocommunication systems — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.1238-13-202509-I!!PDF-E.pdf
5. 3GPP TR 38.901 v18.0.0, Study on channel model for frequencies from 0.5 to 100 GHz — ETSI/3GPP, 2024 — https://www.etsi.org/deliver/etsi_tr/138900_138999/138901/18.00.00_60/tr_138901v180000p.pdf
6. Rayleigh fading — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Rayleigh_fading
7. RAIN RFID Global Radio Spectrum Allocations — RAIN Alliance, 2026 — https://therainalliance.org/wp-content/uploads/2026/04/RAIN-RFID-Spectrum-Allocation-042826-FINAL-4.pdf
8. SWRA475A, Bluetooth low energy beacons, coexistence section — Texas Instruments, 2016 — https://www.ti.com/document-viewer/lit/html/SWRA475A/coexistence-swra4755506
