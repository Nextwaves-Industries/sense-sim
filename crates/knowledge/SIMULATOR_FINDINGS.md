# Where Sense Sim disagrees with the sources

Found while researching the knowledge base on 2026-11-02. Nothing here has been changed in the code. This file is outside `corpus/`, so the assistant does not retrieve it. Line numbers were correct when read and may have moved; `crates/antenna_core` was being edited at the time.

Regulatory rows come from reproduced copies of the texts, not official gazettes: confirm them with the regulator before relying on them.

## Regulation: decide first

These apply only when no region is selected. Since this was written, `rtls_set_region` and RTLS Settings → Regulatory region make the simulator use legal values; the built-in defaults themselves are unchanged.

| Location | Code value | Sourced value | Source |
|---|---|---|---|
| `assets/devices/generic.json:15`, planner default | UWB `freq_mhz` 6489.6 (channel 5) | Vietnam's licence-exempt list has no row from 4800 to 7238.4 MHz, so channel 5 is not exempt and channel 9 is. China has allowed only 7163-8812 MHz since 1 Aug 2025. | Circular 08/2021/TT-BTTTT Appendix 13: https://caselaw.vn/van-ban-phap-luat/401574-thong-tu-so-08-2021-tt-btttt-ngay-14-10-2021-cua-bo-truong-bo-thong-tin-va-truyen-thong-quy-dinh-ve-danh-muc-thiet-bi-vo-tuyen-dien-duoc-mien-giay-phep-su-dung-tan-so-vo-tuyen-dien-dieu-kien-ky-thuat-va-khai-thac-kem-theo ; https://gma.caict.ac.cn/en/update/128/522 |
| `generic.json:66-67` `rfid-reader` | 30 dBm + 9 dBi = 39 dBm EIRP (36.85 dBm ERP) | Vietnam 500 mW ERP (27 dBm ERP); FCC 36 dBm EIRP, with conducted power reduced above 6 dBi; ETSI lower band 33 dBm ERP. Over all three. If the gain is really dBic, it may meet FCC but is still about 6.85 dB over Vietnam's limit. | https://firesmart.vn/tin-tuc-thong-tu-so-08-2021-tt-btttt/ ; https://www.law.cornell.edu/cfr/text/47/15.247 |
| `generic.json:81-82` `rfid-portal` | 30 dBm + 12 dBi = 42 dBm EIRP | Same limits; 12.85 dB over Vietnam's | same |
| `generic.json:96-97` `rfid-desktop` | 33 dBm + 0 dBi = 33 dBm EIRP | 3.85 dB over Vietnam's limit | same |

## RF and RFID models

| Location | Code value | Sourced value | Source |
|---|---|---|---|
| `src/rtls/radio.rs:215`, `:275` | UWB link range ignores wall loss, so 10 cm concrete passes out to 40 m | About 32 dB budget after the first metre; a 15-25 dB wall leaves about 2-7 m | https://nvlpubs.nist.gov/nistpubs/Legacy/IR/nistir6055.pdf ; ITU-R P.2040 |
| `generic.json:71` vs `:47` | Wall loss 8 dB at 920 MHz, 6 dB at 2.44 GHz | Concrete and masonry loss rises with frequency, so this is inverted | NIST, P.2040 |
| `radio.rs:156`, `generic.json:71` | 20 cm concrete at 920 MHz: 32 dB | 7.5 dB (P.2040, computed), 8.7 dB (38.901), 22.5 dB (NIST) | as above; ETSI TR 138 901 |
| `radio.rs:253`, `:156` | 20 cm concrete at 6.5 GHz: 24 dB | 27 dB (P.2040, computed) to 54 dB (NIST) | as above |
| `generic.json:142`, `:157`, `radio.rs:298` | WiFi exponent 3.0 plus explicit wall loss | P.1238 coefficients already include walls, so walls are counted twice | ITU-R P.1238 |
| `generic.json:43-44`, `radio.rs:298` | BLE 0 dBm with `rssi_at_1m` -59; `tx_power_dbm` is ignored | Free-space loss at 1 m is 40.2 dB, so 0 dBm reads about -40 dBm (arithmetic). The setting has no effect. | computed |
| `generic.json:111`, `radio.rs:402` | Tag sensitivity -18 dBm plus 2 dBi | Current chips are -23 to -25.5 dBm; NXP quotes figures on a 2.15 dBi antenna, so added gain may double count | https://nxp.com/docs/en/data-sheet/SL3S1206.pdf |
| `generic.json:70` | Reader sensitivity -85 dBm | R700 typical -93 dBm; R420/R220 -84 dBm | Impinj R700 datasheet v2.0 |
| `radio.rs:401-404` | No polarisation, attachment or orientation loss; fading sigma 2 and 1 dB | Circular-to-linear about 3 dB; Rician K = 7 dB needs about 6 dB for 95%, 10 dB for 99% | Nikitin and Rao 2008; ETSI TR 138 901 |
| `radio.rs:436`, `sim.rs:764` | Each reader device has its own `read_rate_hz` | A radio's rate is shared across ports, so paired portal antennas are overstated | Impinj R700 datasheet v2.0 |
| `src/rtls/rf.rs:21` | UWB "weak" below -100 dBm | Below the -95 dBm sensitivity used elsewhere, so an inaudible link can show as OK | internal inconsistency |

## UWB

| Location | Code value | Sourced value | Source |
|---|---|---|---|
| `radio.rs:52-54`, `:235` | Metal or liquid gives no measurement | Usually a late measurement with a large error, up to a few metres. The simulator is optimistic: a bad range is harder to reject than a missing one. | https://arxiv.org/abs/2203.14471 ; https://arxiv.org/html/2404.15314v1 |
| `radio.rs:149-151`, `generic.json:11` | Two 10 cm concrete walls block the link; one adds about 1.2 m mean | Two such walls add about 1 ns (0.3 m) with the direct path present (furnished apartment, channel unspecified) | https://arxiv.org/html/2404.15314v1 |
| `src/rtls/placement.rs:57` | Default target 0.3 m p95 | Metal warehouses measured 0.49 m mean with one system, 0.86 m with another. Treat 0.3 m as a design goal. | https://vbn.aau.dk/ws/files/482474789/sensors_22_02927.pdf |
| `src/rtls/device.rs:140-141`, `radio.rs:257` | -41.3 dBm/MHz described as the DW3000's full power | Programmable to about -31 dBm/MHz; EU enhanced indoor class is -31.3 dBm/MHz at 5% duty | DW3000 datasheet; https://lawplayer.com/eu/act/32024D1467 |
| `generic.json:26` | `current_ma` 0.08 for a 10 Hz TWR tag | About 1.7 mA for 4 anchors, chip only (computed from the datasheet). The field is not read anywhere. | DW3000 datasheet |

Not modelled: `clock_ppm` is never read and `sim.rs:492` passes a clock offset of 0; no air-time or collision model (`sim.rs:417`), so many tags at high rates look fine; no body-blocking term in `uwb_range` (measured 32 cm median, outliers up to 4 m: https://pmc.ncbi.nlm.nih.gov/articles/PMC10575093/).

## BLE, WiFi and positioning

| Location | Code value | Sourced value | Source |
|---|---|---|---|
| `generic.json:122` | `aoa_fov_deg` 140 | Usable cone about +/-40 deg indoors, from a 2-element array, so indicative only | https://ar5iv.labs.arxiv.org/html/1909.08063 |
| `generic.json:139-140` | WiFi RTT NLOS bias 1.5 m, sigma 1.0 m | About 5 m in NLOS; uncalibrated devices can be off by tens of metres | https://arxiv.org/html/2511.17935v1 |
| `placement.rs:60` | `Method::Rssi` target 3.5 m, also used for WiFi RSSI | WiFi fingerprinting under 8.5 m at p95; BLE under 2.6-4.8 m | https://www.cl.cam.ac.uk/~rkh23/site/publication/pub42 |
| `generic.json:45` | BLE exponent 2.2 (WiFi uses 3.0 in the same band) | n = 2.8-3.0 residential and office, 2.2 commercial, near 2 GHz (secondary copy of an older ITU edition) | https://en.wikipedia.org/wiki/ITU_model_for_indoor_attenuation |

Behaviour to know: `rtls_add_device` sets yaw only (`src/rtls/tools.rs:250`), so a manual AoA locator faces horizontally while planners face it down (`planning.rs:823`). `p95_predicted_error_m` (`tools.rs:379`, `planning.rs:206`) is a percentile across floor cells of a 1-sigma error, not a 95% radius. `rtls_optimize_anchors` (`tools.rs:400-416`) passes no `zones`, so only `rtls_auto_deploy` penalises stray RFID reads.

## Antenna

| Location | Code value | Sourced value | Source |
|---|---|---|---|
| `crates/antenna_core/src/material.rs:94-95` | RO4003C 3.38, RO4350B 3.48 (process Dk) | Design Dk 3.55 and 3.66; a patch sized at 3.38 lands 57 MHz low at 2.45 GHz, 149 MHz low at 6.5 GHz | Rogers RO4000 data sheet |
| `material.rs:98` | RT/duroid 6010 Dk 10.2 | 6010.2LM design Dk 10.7 | Rogers |
| `material.rs:92`, `:32`, `src/rtls/fdtd/antenna.rs:132` | FR-4 4.4 / 0.02 "at 10 GHz"; RTLS code uses 4.3 | 370HR: 4.04 / 0.021 at 2 GHz, 3.92 / 0.025 at 5-10 GHz. The two code values also disagree. | Isola 370HR data sheet |
| `src/rtls/fdtd/antenna.rs:180` | Substrate `kappa` 0 (lossless) | Tutorial sets kappa from the loss tangent; FR-4 gain and efficiency are overstated | openEMS Simple Patch tutorial |
| `fdtd/antenna.rs:152`, `Setup::for_band` in `project.rs` | Stop at -40 dB | -60 dB called a good default; API recommends below 1e-4 | docs.openems.de |
| `fdtd/antenna.rs:190-192`, `antenna_core/src/csx.rs:243-244` | PML clearance 0.383 lambda0 at Normal with a 0.5-1.5 f0 sweep | More than lambda-max/4 (0.5 lambda0 there); only the low sweep edge is short | openEMS FDTD Mesh wiki |
| `antenna_core/src/templates.rs:642`, `fdtd/antenna.rs:137` | Probe at L/6 from centre | About 80 ohm by the cos-squared law (S11 about -12.7 dB); 50 ohm sits near L/7 to L/8 | McMaster Lecture 21 |
| `templates.rs:677` | `Redge = 90*er^2/(er-1)*(L/W)^2` | Within 5% for er 2.2 and up, singular at er = 1: foam at 1.07 gives 1244 vs 233 ohm | McMaster Lecture 21 |
| `src/rtls/fdtd/csx.rs:189-190` | Metal edges on mesh lines | Docs recommend the 1/3-2/3 rule; mainly affects trace and gap accuracy | docs.openems.de |
| `templates.rs:512` | Slot Z0 400 ohm | Babinet gives 486-546 ohm; only shifts the S11 reference | antenna-theory.com |

## Structure, plant and USD

| Location | Code value | Sourced value | Source |
|---|---|---|---|
| `src/scene.rs:1482`, generated parts and shell | `upAxis` Y on export | SimReady check UN-006 requires Z; Isaac Sim defaults to Z, and other up axes import rotated | https://nvidia.github.io/simready-foundation/2026.07.1/capabilities/core/units/requirements.html |
| `src/usd_load.rs:800` | Missing `upAxis` treated as Z | Fallback is Y | https://docs.nvidia.com/learn-openusd/latest/beyond-basics/units.html |
| `src/usd_load.rs:808` | Missing `metersPerUnit` becomes 1.0 if `upAxis` is authored | Fallback is always 0.01 | https://openusd.org/release/api/group___usd_geom_linear_units__group.html |
| `src/plant/parts.rs:812` | Physics joints without `kilogramsPerUnit`, rigid body or collision schemas | Required when physics is present; colliders and mass expected | https://docs.omniverse.nvidia.com/kit/docs/asset-requirements/1.1.6/indexes/requirements_index.html |
| `src/agent/tools.rs:920`, `structures.rs:273` | Ceiling at tallest object + 1.0 m, beams 0.6 m below it (0.4 m clearance); `prompt.rs` says +1.5 m | 0.457 m to sprinklers; practitioners report 0.9 m for ESFR | OSHA 1910.159; MeyerFire (forum) |
| `src/plant/tools.rs:173` | `diameter_m` default 0.05 as outer diameter | Nearer DN 40 (48.3 mm OD) than DN 50 (60.3 mm OD) | secondary (Wikipedia) |
| `src/plant/tools.rs:459` | Conveyor default 0.4 m/s | 0.17 m/s where people work on moving items; only matters at manual stations | https://www.ccohs.ca/oshanswers/ergonomics/conveyor_ergonomics.html |
