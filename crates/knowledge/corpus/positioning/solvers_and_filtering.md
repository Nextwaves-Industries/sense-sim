---
title: Position solvers and filters - multilateration, TDoA, DOP, outliers, Kalman, particle filter, fusion
domain: positioning
tags: multilateration, trilateration, least squares, WLS, Gauss-Newton, Levenberg-Marquardt, TDoA, hyperbolic, Chan, Taylor, DOP, HDOP, PDOP, GDOP, outlier rejection, NLOS, robust, Kalman, EKF, UKF, particle filter, sensor fusion, PDR, zone presence, geofence, CRLB
aliases: giải vị trí, đa điểm, bình phương tối thiểu, hệ số suy giảm độ chính xác, lọc Kalman, lọc hạt, loại nhiễu ngoại lai, kết hợp cảm biến, phát hiện hiện diện trong vùng, position solver, tracking filter, geometry dilution
verified: 2026-11-02
---
# Position solvers and filtering

## Multilateration as nonlinear least squares
Multilateration finds the position p that minimises the sum over anchors i of w_i x (|p - a_i| - r_i)^2, where r_i is the measured range in metres and w_i = 1 / sigma_i^2. This is nonlinear in p, so it is solved iteratively. Gauss-Newton solves (J^T J) dx = -J^T r for the step, where J holds the unit vectors from each anchor to p [1]. The start must be near the minimum, and J must have full column rank, so anchors in a line or a plane can make height or one axis unobservable [1]. Levenberg-Marquardt adds a damping term, (J^T J + lambda D) dx = -J^T r, which is safer when Gauss-Newton diverges [1]. Minimum measurements: two unknowns plus redundancy. With a known tag height, use at least 3 ranges to avoid the two-circle ambiguity [2].

## TDoA solvers: Chan and Taylor
TDoA removes the tag clock by using differences of arrival time, so the measurements form hyperbolae. Pseudo-range multilateration needs m >= d + 1 signals for d dimensions, because the clock acts as one more unknown [2]; that is at least 3 anchors for 2D and 4 for 3D.

| Method | Type | Strength | Weakness |
|---|---|---|---|
| Chan-Ho 1994 [3][4] | closed form, two-step weighted LS | no starting point needed | assumption of independence can break, especially for distant sources; can yield multiple roots [4] |
| Taylor series [4] | iterative linearisation | generally more accurate when it converges, near the CRLB [4] | can diverge, more so at higher noise, and depends strongly on initial value [4] |

Practical pattern: use a closed-form or previous-epoch estimate to seed the iterative solver. Accuracy is best near the array centre and falls near and beyond the anchors [4]. The anchor network still needs clock synchronisation, a UWB detail covered elsewhere.

## Dilution of precision (DOP)
DOP maps ranging noise to position noise: position error is roughly DOP x range error [5]. It comes from Q = (A^T A)^-1, where each row of A is the unit vector from receiver to anchor; horizontal DOP is sqrt(Q_xx + Q_yy) and position DOP is sqrt(Q_xx + Q_yy + Q_zz) [5]. Use it as a geometry check before running noise simulations.

| DOP | Common GNSS rating [5] |
|---|---|
| under 1 | ideal |
| 1-2 | excellent |
| 2-5 | good |
| 5-10 | moderate |
| 10-20 | fair |
| over 20 | poor, discard |

Those ratings come from satellite navigation, so treat them as a guide indoors. Without a clock term, 2D range-based HDOP can fall below 1 with several surrounding anchors. Examples: 0.1 m ranging at HDOP 1.5 gives about 0.15 m; 1 m at HDOP 3 gives about 3 m. Tags outside the anchor hull get high DOP fast, so put anchors around the working area, not along one side.

## Outlier and NLOS rejection
NLOS and multipath reduce ranging accuracy indoors, and most studied work concerns WiFi and UWB channels; recent surveys group methods into single-measurement and hybrid-measurement identification and mitigation [6]. NLOS errors are not zero-mean: raw FTM distances in NLOS tend to read long [7], so least squares is biased. Common approaches:
- Hard gate: if the residual exceeds about 3 sigma and redundancy remains, remove the measurement that best explains the misfit. This is what greedy subset search does (rule of thumb).
- Down-weight: raise sigma_i for suspected NLOS instead of removing it, which keeps geometry.
- Consistency checks: compare two independent methods; the Bluetooth SIG advises distrusting large PBR and RTT mismatches [8].
- Learned weights: one study's model assigned lower standard deviations to LOS, cutting positioning error 17-30% against well-calibrated ranging [7].

Rejection needs redundancy: with exactly the minimum measurements, nothing can be identified, so design for at least one spare anchor everywhere.

## Kalman filter for tracking
The Kalman filter predicts x = F x and P = F P F^T + Q, then updates with innovation y = z - H x, S = H P H^T + R, gain K = P H^T S^-1, x = x + K y, and P = (I - K H) P [9]. It is optimal for linear models with Gaussian noise of known covariance, and otherwise the best linear estimator; unmodelled dynamics can make it diverge [9]. For positioning, a constant-velocity state [x, z, vx, vz] is typical with position fixes as z. Tune Q from expected acceleration, and R from measurement noise. High Q follows turns but passes more noise; low Q smooths but lags. Set R from fix quality rather than a constant, inflating it with residual and DOP. Gate innovations: a filter fed a large NLOS outlier will jump. Reset after a long gap instead of predicting blindly.

## EKF versus loose coupling
Two architectures exist. Loose coupling solves a fix first, then filters positions. It is simple and modular. Tight coupling, typically an extended Kalman filter (EKF), or an unscented filter, takes raw ranges or angles directly as nonlinear measurements [9][10]. Tight coupling can use fewer than the minimum anchors, because the prediction fills in the missing dimension, and can weight each link individually, so it helps most in sparse or heavily blocked coverage. It costs more tuning, and EKFs are approximations that linearise around the current estimate [10]. Measured evidence: path-loss with EKF and fingerprint with EKF gave under 3.99 m and 3.04 m at 90% in a 20-beacon office [11]. Rule: start with loose coupling, then adopt tight coupling if logs show frequent epochs with under 3 links.

## Particle filter for indoor maps
A particle filter represents the state posterior with weighted samples, reweights them by how well they match observations, and resamples when weights collapse onto a few particles [10]. It makes no assumption of linearity or Gaussian noise, which suits non-Gaussian RSSI, multimodal hypotheses and map constraints such as walls and aisles [10]. It is weaker in high-dimensional states [10], and cost scales with particle count: one BLE study used 10,000 particles in offline Python simulation [12]. Use a particle filter when a floor plan is available, the target walks through corridors or aisles, and measurements are coarse. Use a Kalman filter when measurements are good and compute is limited, such as on a tag or for thousands of tags.

## Sensor fusion with IMU and other sources
Inertial sensors bridge gaps and cut noise. In a WiFi FTM study on an 85 x 55 m office, combining a neural network with 100 Hz inertial data reduced raw ranging MAE 47-50% and positioning error 17-30% against well-calibrated ranging [7]. A survey cites RSS plus pedestrian dead reckoning reaching about 0.5 m positioning error [13]. Treat single-site results as optimistic. Fusion rules: use IMU for short-term motion and detect stationary periods to freeze the estimate, since this stops noise wander; use radio to bound long-term drift; timestamp everything, as a small time offset matters at speed; and hold height from a known mount or barometer rather than from poor-geometry radio. Vision can also help: adding vision to BLE multichannel RSSI gave RMSE 2.41 m against 6.11 m for uncorrected RSSI [14].

## Zone-based presence versus coordinates
Many use cases need "which zone", not coordinates. Cisco advises a zone no more than twice the desired accuracy, for example at most 20 x 20 m for 10 m accuracy [15]. Reliable rules: size a zone at least 2 x the p95 error for the location system if you need a dependable enter/exit event (rule of thumb, consistent with [15]). Prevent chatter near boundaries with hysteresis, such as requiring several consecutive fixes inside before raising an event, and a larger exit distance than entry distance (rule of thumb). For restricted-area alarms, tune which error is more costly, missed entry or false alarm, and test at the actual doorway. Walls do not stop radio, so tags may appear in a neighbouring room; use a map constraint or a dedicated reader at the door when room accuracy matters.

## Applying this in Sense Sim
`src/rtls/solver.rs` implements these with Levenberg-Marquardt:
- `solve_twr`, `solve_twr_robust` (greedy drop while residual exceeds the gate and it improves by at least 30%, gate 3 x smallest sigma), `solve_tdoa` (iterative, not Chan, seeded by prior or centroid), `solve_aoa`, `dop`, and a constant-velocity `Kalman` with accel sigma 1.5 m/s^2.
- Priority for BLE/WiFi: AoA, then RTT (3+), then RSSI (3+), then proximity fallback.
- BLE and WiFi solve on a plane at the tag's true height, so height error is not exercised, and AoA fixes report DOP 1.

Tool controls: `rtls_control` sets `kalman`, `robust`, `solve_3d`, `seed`. `robust` only changes UWB TWR; RTT always rejects outliers; RSSI never does. Run `rtls_run` (`seconds`, `reset`) with each setting and compare `rmse_m`, `cep50_m`, `cep95_m`, `max_error_m` and per-tag `dop`. Use `rtls_list_devices` to inspect `links`, `nlos_links`, `error_m`. Use `rtls_update_device` with `enabled: false` to test a failed anchor, or `redundancy: true` on `rtls_optimize_anchors`. Add `solve_3d: true` only with anchors at varied heights. Zones: `rtls_add_zone` (`name`, `min`, `max`, `restricted`, `capacity`); no hysteresis parameter exists. There is no EKF, particle filter or IMU model.

## Sources
1. Gauss-Newton algorithm — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Gauss%E2%80%93Newton_algorithm
2. Pseudorange multilateration — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Pseudorange_multilateration
3. A simple and efficient estimator for hyperbolic location (Chan, Ho), bibliographic record only — IEEE Trans. Signal Processing 42(8), 1994 — https://api.crossref.org/works/10.1109/78.301830
4. Robust TDOA Localization Using Weighted Least Squares with Cone Tangent Plane Constraint (Jin, Xu, Zhang) — Sensors, 2018 — https://pmc.ncbi.nlm.nih.gov/articles/PMC5876713
5. Dilution of precision — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Dilution_of_precision
6. NLOS Identification and Mitigation for Time-based Indoor Localization Systems (Nkrow et al.), abstract — ACM Computing Surveys, 2024 — https://scholars.cityu.edu.hk/en/publications/nlos-identification-and-mitigation-for-time-based-indoor-localiza/
7. Enhanced Wi-Fi RTT Ranging: A Sensor-Aided Learning Approach — arXiv 2112.14138, 2021 — https://ar5iv.labs.arxiv.org/html/2112.14138
8. Bluetooth Core 6.0 feature overview — Bluetooth SIG, 2024 — https://www.bluetooth.com/core-specification-6-feature-overview/
9. Kalman filter — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Kalman_filter
10. Particle filter — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Particle_filter
11. Smartphone-Based Indoor Localization with Bluetooth Low Energy Beacons (Zhuang et al.) — Sensors, 2016 — https://pmc.ncbi.nlm.nih.gov/articles/PMC4883287
12. Model-Based Localization and Tracking Using Bluetooth Low-Energy Beacons (Danis, Cemgil) — Sensors, 2017 — https://pmc.ncbi.nlm.nih.gov/articles/PMC5713487/
13. Indoor Positioning with Wi-Fi Location: A Survey of IEEE 802.11mc/az/bk Fine Timing Measurement Research — arXiv 2509.03901, 2025 — https://arxiv.org/html/2509.03901v1
14. Combining Multichannel RSSI and Vision with Artificial Neural Networks to Improve BLE Trilateration (Naghdi, O'Keefe) — Sensors, 2022 — https://pmc.ncbi.nlm.nih.gov/articles/PMC9227766
15. Accurate location starts with accurate data — Cisco blog, 2015 — https://blogs.cisco.com/networking/accurate-location-starts-with-accurate-data
