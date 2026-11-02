---
title: UWB anchor placement (DOP, spacing, height, clearance, TDoA sync)
domain: uwb
tags: uwb anchor placement, gdop, hdop, vdop, pdop, dop, dilution of precision, cramer-rao, crb, fisher information, anchor spacing, mounting height, metal clearance, convex hull, geometry, anchors per area, tdoa sync backhaul, 3d positioning, redundancy, rack mounting
aliases: đặt anchor, bố trí anchor, vị trí lắp anchor, khoảng cách giữa các anchor, chiều cao lắp đặt, mật độ anchor, bao nhiêu anchor, số anchor cho nhà kho, trạm neo UWB, hình học anchor, đồng bộ anchor, how many anchors, where to put anchors
verified: 2026-11-02
---
# UWB anchor placement

## Dilution of precision (DOP) math for UWB ranging
For range measurements with equal noise sigma, the position covariance is sigma^2 * inv(H^T H), where each row of H is the unit vector from anchor to tag. HDOP = sqrt(trace(inv(H^T H))) over x and y, so position error (rms) = HDOP * sigma.

With N anchors evenly spaced around the tag in a plane, H^T H = (N/2) * I, giving HDOP = 2 / sqrt(N) (derived):

| Anchors evenly around the tag | HDOP |
|---|---|
| 3 | 1.15 |
| 4 | 1.00 |
| 6 | 0.82 |
| 8 | 0.71 |

For a circular error, p95 is about 1.73 * HDOP * sigma (derived from a Rayleigh distribution). Example: to reach 0.30 m p95 with HDOP 1, per-range noise plus unmodelled bias must stay at or below 0.17 m. When anchors are nearly collinear with the tag, H^T H becomes nearly singular and HDOP grows without limit.

## TDoA geometry rule: opposing anchor pairs
For TDoA the Cramér-Rao analysis of one anchor pair gives Fisher information trace = 2 * (1 - cos(dtheta)) / (sigma_i^2 + sigma_j^2), where dtheta is the angle between the directions from the tag to the two anchors. Position variance has a lower bound of 4 / trace [3].

Information is greatest at dtheta = 180 degrees, so each anchor pair should lie on opposite sides of the tag area. With equal arrival noise sigma and m anchors in m/2 opposing pairs, position rms is at least 2 * sigma / sqrt(m) (derived from [3]). Example: 6 cm arrival noise and 4 anchors gives a 6 cm bound.

Pairs on the same side of the tag have dtheta near 0 and give nearly no information. This explains why TDoA degrades fast outside the anchor hull. Pozyx also warns that accuracy and stability decline quickly outside the convex hull [1]. Optimising placement with this kind of bound lowered 2D RMSE to about 0.058 m, 42 to 51% better than trivial layouts, in a 6 x 7 m room with a cabinet [4].

## Anchor geometry rules for UWB
Rules from installation guidance and from measurements:

- Keep tags inside the convex hull of the outermost anchors [1].
- Never place anchors in one line; it amplifies range errors [1][2].
- Corridors and aisles are the hard case, because anchors there are nearly collinear with the tag. Accuracy across an aisle is weak by geometry even with many anchors (derived from the HDOP formula); confirm by computing it.
- Give each point at least 3 anchors, ideally 4 for TWR, so one bad range can be identified [11].
- Trade geometry against blocking. The optimiser in one cluttered-room study explicitly preferred anchor positions that avoided NLOS paths, particularly near metal [4].
- Edges have the worst geometry: a Cramér-Rao map can flag those regions before installing, and lower bounds generally matched better experimental results [5].

Changing anchor placement alone changed RMSE noticeably in experiments, for example from 21 cm to 16 cm between two four-anchor layouts [5].

## UWB anchor spacing and anchors per area
No opened source gives a density per area directly. Pozyx asks that each anchor see at least two neighbours at 15 to 40 m, depending on the environment and regulations [1]. The table derives counts from a square grid with spacing s: floor area per anchor is s^2, and the farthest cell centre from its 4 nearest anchors is 0.71 * s.

| Grid spacing | Area per anchor | Anchors per 10,000 m2 | Distance to 4 nearest at cell centre |
|---|---|---|---|
| 15 m | 225 m2 | 44 | 10.6 m |
| 20 m | 400 m2 | 25 | 14.1 m |
| 30 m | 900 m2 | 11 | 21.2 m |
| 40 m | 1600 m2 | 6 | 28.3 m |

Use the low end of the range for racks and machines, and the high end only for open halls. For one spare link, a fifth anchor is at 1.58 * s from the cell centre, so s = 20 m needs a 32 m link (derived). Compute the real site with obstacles, since racks can split it into effectively separate aisles.

## UWB anchor mounting height and 3D positioning
Pozyx advises mounting anchors high on ceilings or walls and in line of sight of the tags, without a metre figure [1][2]. One published setup used anchors at 2.65 m and evaluated its placement map at 1.5 m [5]. Mean error also grows for low tags: 0.34 m at 2 m height versus 0.65 m at 0.3 m in a 14 x 40 m lab [7].

Height spread sets vertical accuracy. Each anchor contributes sin^2(elevation) to vertical information, so VDOP is about 1 / sqrt(sum(sin^2(el))) (derived, ignoring coupling with x and y). Example with 4 anchors, 10 m away horizontally, tag at 1.2 m:

| Anchor heights | VDOP (computed) | z error at 0.1 m sigma |
|---|---|---|
| All at 2.6 m | 3.6 | 0.36 m |
| Two at 5.0 m, two at 0.5 m | about 2 | 0.2 m |

If tags are coplanar with the anchors, vertical information is zero and above and below cannot be told apart. For 3D, maximise height spread, mount a cube of anchors around the zone, and twist the top ring 45 degrees relative to the bottom, which shrinks the hull but stops a body blocking two anchors at once [1]. Larger vertical separation improved results in one study: RMSE 16 cm at 5.5 m separation with 3 agents versus 21 cm at 2.65 m with 5 agents [5]. Flat-floor vehicles remain weak in z [10]; constrain z to a known height where possible.

## Anchor clearance from metal, walls and water
| Item | Guidance | Source |
|---|---|---|
| Metal near the antenna | Keep 20 cm clear | [2] |
| Within a few cm of the antenna | No metal at all, including screws; same for tag antennas | [1] |
| Walls | At least 20 cm from the anchor | [1] |
| Water and wires | Keep away, especially from the antenna | [1] |
| Orientation | Vertical, antenna at the bottom, connectors on top | [1] |

These are vendor rules and depend on the antenna pattern. Mounting directly on a steel column or a rack upright conflicts with them unless a standoff bracket is used. Mount above rack height wherever possible so that the path to aisles is clear: a ceiling anchor sees down an aisle, a low anchor sees only one row (rule of thumb). The same limits apply to tags, so a tag flush on a steel forklift mast or on a metal pallet can lose range and accuracy. Test the mounted tag, not a bare one.

## TDoA synchronisation backhaul and anchor connectivity
TDoA anchors must share a timebase, so the anchor layout is also a radio network. Requirements found:

- Anchors need line of sight to each other, not only to tags, when UWB distributes the clock: each anchor sees at least two neighbours [1].
- A radio-synchronised system reached 0.2 ns 1-sigma across 10 anchors with TDMA between anchors [8]. In another test, accuracy fell from 92% to 60% of fixes within 20 cm as sync age grew from 100 ms to 500 ms [9].
- Only one anchor needs a wired gateway link to the location engine in the radio-synced design; others need only power [8].
- Pozyx estimates a radio cell of roughly 1000 m2 sharing one update budget [11]. Very large sites need multiple cells and a handover scheme.

Racks that block anchors from each other can split the sync network. Check inter-anchor paths at anchor height. Sync traffic also consumes air time, which is one reason the theoretical tag capacity is rarely reached. Wired clock distribution avoids this, though no performance number for it was verified.

## Survey and commissioning of UWB anchors
A plan is only as good as the installed coordinates:

- Anchor positions from blueprints were 22 cm RMSE off the surveyed positions in one multi-room deployment, because construction differed from the drawings [5].
- Survey with a total station or laser. The UTIL dataset used a millimetre-accurate total station [6]. A cheaper route is self-calibration: anchors range to one another round-robin, plus a few ground-truth references to fix the frame [8].
- A warehouse pipeline deployed 12 anchors and estimated positions and range biases from a short calibration drive; raw constant biases reached 1.19 m [10].

After installation, walk the worst-case points, compare with survey marks at the real tag height, and record per-anchor bias.

## Applying this in Sense Sim
Sense Sim's planner uses summed Fisher information (the Cramér-Rao bound), not DOP, per device and with per-link noise, dropout and NLOS. Y is up.

- One shot: `rtls_auto_deploy` with `uwb: true`, `target_cm`, optional `max_anchors`, `heights`, `dim3`, `redundancy`, `min_spacing_m`, `head_end`, `refine`.
- Incremental: `rtls_optimize_anchors` with `tech: "uwb"`, `count`, `mount: "structure"` to use ceiling, walls and columns, `target_p95_m`, `target_coverage`, `heights`, `apply`. Default minimum spacing is 1.5 m; the default target is 0.3 m p95.
- Check: `rtls_coverage` with `tech: "uwb"`, `height`, `cell` (defaults 1.2 m and 0.5 m), then `rtls_run` to compare predicted and simulated error.
- Failure test: `rtls_update_device` with `enabled: false`, or plan with `redundancy: true`.
- Tag height: pass `heights: [0.3, 1.2, 1.8]` for mixed fleets. Add `dim3: true` only with real height spread.
- Clearance is not modelled: mount candidates ignore metal standoff, so state the 20 cm rule to the user. For TDoA, an unsynced note means the anchor layout must change.
- `uwb-anchor` mounts at 2.6 m by default. If racks are taller, place above rack height or use `mount: "structure"`.

## Sources
1. Choosing the anchors' locations, Pozyx installation documentation (vendor) — checked 2026-11-02 — https://installation-docs.pozyx.io/choosing-the-anchors-locations
2. Where to place the anchors, Pozyx Academy (vendor) — checked 2026-11-02 — https://pozyx.io/pozyx-academy/where-to-place-the-anchors
3. Optimal Geometry for Ultra-wideband Localization using Bayesian Optimization, W. Zhao, M. Vukosavljev, A. P. Schoellig — IFAC, 2020 — https://www.dynsyslab.org/wp-content/papercite-data/pdf/zhao-ifac20.pdf
4. Finding the Right Place: Sensor Placement for UWB Time Difference of Arrival Localization in Cluttered Indoor Environments, W. Zhao, A. Goudar, A. P. Schoellig — arXiv, 2022 — https://ar5iv.arxiv.org/html/2204.04508
5. Ultra-wideband Time Difference of Arrival Indoor Localization: From Sensor Placement to System Evaluation — arXiv, 2024-2026 — https://arxiv.org/html/2412.12427v2
6. UTIL: An Ultra-wideband Time-difference-of-arrival Indoor Localization Dataset, W. Zhao et al. — arXiv, 2022 — https://arxiv.org/abs/2203.14471
7. Evaluation and Comparison of Ultrasonic and UWB Technology for Indoor Localization in an Industrial Environment, A. L. Cretu-Sircu et al. — Sensors, 2022 — https://vbn.aau.dk/ws/files/482474789/sensors_22_02927.pdf
8. Scalable, Low-Cost Indoor Localization System using TDoA and UWB, A. Acton and V. Kulkarni — ARIN Technologies, 2017 — https://www.microsoft.com/en-us/research/wp-content/uploads/2016/11/ARINTech_Acton.pdf
9. Robust and Scalable Techniques for TWR and TDoA based localization using Ultra Wide Band Radios, R. Ramesh et al. — arXiv, 2020 — https://arxiv.org/abs/2008.04248
10. Deployment-Ready UWB Localization for Industrial Ground Robots with Automatic Anchor Calibration and Terrain-Aware Fusion, A. Raab et al. — arXiv, 2026 — https://arxiv.org/abs/2607.15807
11. TDOA and TWR positioning protocols explained, Pozyx Academy (vendor) — checked 2026-11-02 — https://pozyx.io/pozyx-academy/positioning-protocols-explained
