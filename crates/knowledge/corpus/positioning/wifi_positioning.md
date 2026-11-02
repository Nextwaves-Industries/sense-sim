---
title: WiFi positioning - FTM and RTT, 802.11az, RSSI fingerprinting, accuracy, AP density
domain: positioning
tags: WiFi, Wi-Fi, WLAN, 802.11mc, FTM, fine timing measurement, RTT, round trip time, 802.11az, NGP, next generation positioning, 802.11bk, RSSI fingerprinting, AP density, access point placement, Android WifiRttManager, calibration offset, NLOS
aliases: định vị WiFi, định vị Wi-Fi, đo thời gian khứ hồi, lấy dấu vân tay WiFi, mật độ access point, mật độ AP, điểm truy cập, Wi-Fi location, Wi-Fi RTT ranging
verified: 2026-11-02
---
# WiFi positioning

## WiFi FTM and RTT ranging basics
Fine Timing Measurement (FTM), from IEEE 802.11mc and carried in IEEE 802.11-2016, measures round-trip time between a client and an access point, and is exposed on Android as Wi-Fi RTT [1]. Multilateration needs three or more APs and Android documents a typical position accuracy within 1-2 m [1]. Timing needs nanosecond resolution: a 0.1 ns timestamp error is about 3 cm of distance [2]. Burst duration can be 250 us to 128 ms and burst period is set in 100 ms units [3]. The AP must be an FTM responder, and its location must be known. On Android 9 the app supplies AP positions itself; from Android 10, APs supporting LCI/LCR can report their location [1]. Check AP support first, because a site's existing WiFi may not offer FTM at all.

## Measured FTM accuracy and bandwidth
Published figures, by bandwidth and condition:

| Source | Setup | Result |
|---|---|---|
| Android docs [1] | 3+ APs, general | typically 1-2 m position (vendor claim) |
| Survey 2025 [3] | 802.11mc, 20-80 MHz | 1-2 m |
| Survey 2025 [3] | 802.11az, 20-160 MHz | under 1 m; 0.4 m listed for LOS |
| Survey 2025 [3] | 802.11bk, 20-320 MHz | under 0.1 m LOS, a target |
| arXiv 2112.14138 [4] | office 85 x 55 m, 10 APs, 40 MHz, up to 5 used | average 1.24 m without sensors |
| Rajendran et al. [5] | enterprise APs, ch 36, LOS, 80/160 MHz | meter-level "consistently" |
| same [5] | NLOS | about 5 m |

Wider channels help, but not always: [4] saw similar ranging at 40 and 80 MHz and no clear positioning gain from 80 MHz. Use the 1-2 m range for LOS designs and assume several metres in NLOS.

## FTM calibration offsets are the largest error
FTM devices have hardware-dependent ranging bias that must be calibrated. Measured examples: at a true 6 m, an uncalibrated responder reported 71.89 m (20 MHz), 47.89 m (40 MHz) and 66.07 m (80 MHz), while a calibrated one reported 10.96 m, 7.45 m and 6.36 m [5]. Another study measured bias of 6.6 m at 20 MHz, 4.4 m at 40 MHz and 3.4 m at 80 MHz [4]. A survey names device-dependent bias, blocker-dependent bias in NLOS, multipath fluctuation and outliers as the error sources, and reports clock offset up to 20 ppm causing up to 30 cm [3]. Action: calibrate per device model and bandwidth at known distances before multilaterating, and repeat after firmware changes. A residual constant bias does not average out, so filtering alone will not fix it.

## FTM in NLOS and congestion
NLOS raw FTM distances tend to overestimate the true distance, while LOS success rate stayed above 95% at all tested distances; success falls with distance as RSS nears the noise floor, faster at wider bandwidth [4]. In heavily congested NLOS tests, 802.11az packet failure rates were 50%, 37%, 55%, 50% and 65% at 2, 4, 6, 8 and 10 m [5]. FTM frames still obey CSMA/CA, which adds latency in dense deployments [5]. The same authors say sub-1-metre accuracy is hard in difficult RF environments [5]. A survey notes its underlying literature is LOS-heavy: 55% LOS, 18% NLOS, 27% mixed, so published averages can flatter real sites [3]. Action: treat range outliers with robust rejection, use a filter that trusts LOS measurements more, and design for at least four APs at each point so one can be dropped.

## 802.11az Next Generation Positioning
IEEE 802.11az-2022 was approved 2022-12-03 and published 2023-03-03 as Amendment 4, Enhancements for Positioning; its scope includes higher accuracy, reduced medium use and power, scaling to dense deployments, and security features [6]. IEEE lists the 2022 document as superseded, so check the current base standard. Security additions include AES-128 encryption of the long training field (LTF), LTF randomization against man-in-the-middle attacks, and 64-QAM rather than BPSK at the PHY [5]. Android 15 supports az non-trigger based (NTB) ranging, and capable devices range both mc and az APs in one request, preferring az [1]. Android's AP table lists some mc/az models from Cisco and Aruba [1]. Measured comparison: az was more consistent than mc in LOS and NLOS, most clearly in congested NLOS at 40 MHz and above, but one 2 m LOS case stayed poor, unexplained [5].

## WiFi RSSI and fingerprinting accuracy
RSSI-based WiFi needs no special AP features but gives lower accuracy. In one direct comparison, existing WiFi fingerprinting gave under 8.5 m at 95%, against under 2.6 m for dense BLE in the same 600 m2 area [7]. A robot test over 431 m2 using weighted averaging measured mean errors of 2.62 m static and 6.09 m moving, the worst of BLE, WiFi and UWB [8]. A vendor guide gives 5-15 m; treat that as a claim [9]. Sources disagree because they measure different things: density, phone, method and static versus moving all change the result. A survey of FTM work concludes that FTM generally beats RSS-based methods indoors, especially in LOS [3]. Fingerprint-based WiFi needs a site survey and resurveys on layout changes. Choose RSSI only when APs already exist and room-level accuracy is enough.

## WiFi AP density and placement
Published density guidance is thin. Cisco's 2015 location advice targets four APs heard above -75 dBm throughout the venue, calls AP count the largest contributor to location quality, and says APs should be within about 3 ft of their mapped position, with correct floor the minimum [10]. It also says a zone should be no more than twice the desired accuracy, for example no larger than 20 x 20 m for 10 m accuracy [10]. In an FTM office test, 10 APs on 85 x 55 m, about 468 m2 per AP (arithmetic), gave 1.24 m average at 40 MHz [4]. A smartphone method used four or more APs [3]. Rules of thumb: start from four audible APs per point. Place APs to surround the area rather than down one corridor, since geometry matters as much as count, and check a communications-only AP plan for location before reusing it.

## WiFi capacity and client constraints
FTM consumes airtime. As an example, if one measurement takes 30 ms, one AP can serve about 30 stations per second [3]. 802.11az aims at lower medium utilisation and denser scaling [6]. Passive RSSI methods depend on client probing, and Cisco notes that manufacturers typically set probe intervals of 90 s or more, so network-side tracking of phones may be slow; tags can be set to chirp, for example every 30 s [10]. On Android, RTT requires runtime permissions, location services and Wi-Fi scanning enabled, runs only in the foreground or a foreground service, and background apps are throttled [1]. Results expose `getDistanceMm`, `getDistanceStdDevMm` and success counts, so feed the reported standard deviation into your solver weights and discard any non-success status [1]. WiFi clients typically draw more power than BLE tags (rule of thumb), so rate-limit them.

## Choosing WiFi as a positioning layer
Use WiFi when the site already has APs, clients are phones or handhelds, and 1-2 m in LOS or room-level RSSI is acceptable. Prefer FTM over RSSI wherever AP hardware supports it. Move to UWB when sub-metre accuracy is required in NLOS or metal environments, since measured NLOS FTM reached about 5 m [5]. Do not rely on vendor "meter-level" and "sub-meter" statements from the Wi-Fi Alliance, which give no method or conditions [11]. Reliable reasons to choose WiFi are reuse of infrastructure and of clients, not peak accuracy. Before committing, run a small pilot: calibrate, measure p50 and p95 over a walked path including NLOS, then compare with target accuracy.

## Applying this in Sense Sim
Profiles: `wifi-ap-rtt` (5 GHz, `method` rtt, `range_sigma_m` 1, `nlos_bias_m` 1.5, `nlos_sigma_m` 1, `path_loss_exp` 3, `shadow_sigma_db` 5, `wall_loss_db` 8, `sensitivity_dbm` -82, `max_range_m` 30), `wifi-ap-rssi` (2.4 GHz, `path_loss_exp` 3, `shadow_sigma_db` 6, `wall_loss_db` 6, `sensitivity_dbm` -85, `max_range_m` 40), `wifi-client`. Deploy with `rtls_auto_deploy` and `wifi: true`, or `rtls_optimize_anchors` with `tech: "wifi"`, `profile`, `count`, `target_p95_m`, `apply`. Check with `rtls_coverage` (tech `wifi`) and `rtls_run`.

Model notes:
- RTT is heard like RSSI, then ranged. LOS RSSI reaches -82 dBm near 25 m, not the 30 m cap, and each wall (8 dB) cuts that by a factor of about 1.85 (arithmetic).
- RTT needs at least 3 heard APs and always uses the outlier rejection. Fewer falls back to proximity.
- There is no calibration bias, airtime load or bandwidth parameter, so results resemble a calibrated, uncongested system.
- Default NLOS values (1.5 m bias plus 1 m spread per wall-equivalent) are milder than the measured about 5 m NLOS figure [5]; treat NLOS predictions as optimistic.
- Default plan targets: RTT 2.0 m, RSSI 3.5 m. The RSSI target is optimistic for WiFi compared with [7].

## Sources
1. Wi-Fi location: ranging with RTT — Android Developers, accessed 2026 — https://developer.android.com/develop/connectivity/wifi/wifi-rtt
2. IEEE 802.11az Indoor Positioning with mmWave (Picazo-Martinez et al.) — arXiv 2303.05996 — https://arxiv.org/html/2303.05996v2
3. Indoor Positioning with Wi-Fi Location: A Survey of IEEE 802.11mc/az/bk Fine Timing Measurement Research — arXiv 2509.03901, 2025 — https://arxiv.org/html/2509.03901v1
4. Enhanced Wi-Fi RTT Ranging: A Sensor-Aided Learning Approach — arXiv 2112.14138, 2021 — https://ar5iv.labs.arxiv.org/html/2112.14138
5. Performance comparison of 802.11mc and 802.11az Wi-Fi Fine Time Measurement protocols (Rajendran et al.) — arXiv 2511.17935, 2025 — https://arxiv.org/html/2511.17935v1
6. IEEE 802.11az-2022 — IEEE Standards Association, 2023 — https://standards.ieee.org/ieee/802.11az/7226/
7. Location Fingerprinting With Bluetooth Low Energy Beacons (Faragher, Harle), abstract — IEEE JSAC, 2015 — https://www.cl.cam.ac.uk/~rkh23/site/publication/pub42
8. An Experimental Evaluation of Indoor Localization in Autonomous Mobile Robots (Khoshrangbaf et al.) — Sensors, 2025 — https://pmc.ncbi.nlm.nih.gov/articles/PMC11991048
9. Indoor positioning system guide — AirPinpoint (vendor), accessed 2026 — https://airpinpoint.com/tech/indoor-positioning-system
10. Accurate location starts with accurate data — Cisco blog, 2015 — https://blogs.cisco.com/networking/accurate-location-starts-with-accurate-data
11. Wi-Fi applications overview — Wi-Fi Alliance, accessed 2026 — https://www.wi-fi.org/discover-wi-fi/wi-fi-location
