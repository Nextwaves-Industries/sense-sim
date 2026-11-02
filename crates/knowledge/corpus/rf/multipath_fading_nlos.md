---
title: Multipath, fading, diffraction and NLOS indoors
domain: rf
tags: multipath, fading, Rayleigh, Rician, Rice, Nakagami-Rice, K-factor, delay spread, RMS delay spread, reflection, diffraction, knife-edge, Fresnel zone, NLOS, LOS, OLOS, shadowing, human body blockage, polarization, first path
aliases: đa đường, phản xạ, nhiễu xạ, vùng Fresnel, che khuất, không có tầm nhìn thẳng, trải trễ, suy giảm tín hiệu, fading nhanh, người chắn, non line of sight
verified: 2026-11-02
---
# Multipath, fading, diffraction and NLOS indoors

## Three layers of indoor signal variation

Indoor received power varies on three separate scales, and they need different responses.

| Layer | Spatial scale | Size | Response |
|---|---|---|---|
| Path loss | Whole building | Tens of dB | Spacing, frequency, power |
| Shadowing | Metres, room or object | sigma 3-8 dB | Margin, more infrastructure |
| Small-scale fading | About half a wavelength | Up to 20 dB or more | Averaging, diversity, bandwidth |

P.1238 attributes the impairments to reflection and diffraction within rooms, loss through walls and floors, channelling in corridors, and movement of people and objects [1]. Half-wavelength scales are 16 cm at 920 MHz, 6 cm at 2.44 GHz, 2.9 cm at 5.18 GHz and 2.3 cm at 6.49 GHz (computed). A tag moved by a few centimetres may change RSSI by many dB without any meaningful change in distance.

## Rayleigh fading fade depth and margin

Rayleigh fading applies when many scattered paths with no dominant component add. Envelope follows a Rayleigh distribution, and power is exponential: P(power < x times mean) = 1 - exp(-x) [2].

| Outage probability | Level below mean |
|---|---|
| 10% | 9.8 dB |
| 5% | 12.9 dB |
| 1% | 20 dB |

Values are computed from that distribution. These are small-scale margins, added to any shadowing margin, when a narrowband link cannot move or average. In a stationary narrowband link deep in NLOS, a spot can sit in a persistent null. A wideband signal, frequency hopping or a second antenna averages over nulls; that is why channel-hopping and antenna diversity help far more than a few dB of power at such spots.

## Rician fading and K-factor when a direct path exists

With a dominant path, power follows the Nakagami-Rice distribution; K is the ratio of dominant power to scattered power [1]. Levels below mean power at each outage (computed numerically from the distribution):

| K | 10% | 5% | 1% |
|---|---|---|---|
| Rayleigh (no direct path) | 9.8 dB | 12.9 dB | 20.0 dB |
| 0 dB | 8.6 dB | 11.7 dB | 18.7 dB |
| 3 dB | 7.0 dB | 9.7 dB | 16.2 dB |
| 7 dB | 4.4 dB | 6.0 dB | 9.9 dB |
| 10 dB | 3.0 dB | 4.0 dB | 6.2 dB |
| 15 dB | 1.6 dB | 2.1 dB | 3.0 dB |

TR 38.901 uses K mean 7 dB, standard deviation 4 dB for indoor office LOS, and mean 7 dB, standard deviation 8 dB for indoor factory LOS [3]. A typical indoor LOS narrowband link therefore needs about 6 dB for 95% and 10 dB for 99% of fading, while a person stepping into the path can sharply reduce K [1].

## RMS delay spread by indoor environment

RMS delay spread describes how far multipath energy trails behind. P.1238 reports these values, in ns, as 10%, median and 90% points, representing the largest rooms likely per environment [1].

| Frequency | Environment | 10% | Median | 90% |
|---|---|---|---|---|
| 1.9 GHz | Residential | 20 | 70 | 150 |
| 1.9 GHz | Office | 35 | 100 | 460 |
| 1.9 GHz | Commercial | 55 | 150 | 500 |
| 2.625 GHz | Factory | 51.5 | 69.2 | 87.2 |
| 3.7 GHz | Office | 30 | 38 | 45 |
| 5.2 GHz | Office | 38 | 60 | 110 |
| 5.2 GHz | Commercial | 135 | 190 | 205 |

A rough maximum delay is 3.3 ns per metre of room path [1]. Results depend on delay resolution and the threshold used, so compare only similar measurements [1]. Directional and circularly polarized antennas reduce LOS delay spread [1].

## Delay spread, bandwidth and resolving multipath

Time resolution sets whether multipath can be separated from the first arrival. A signal of bandwidth B resolves paths about 1/B apart (rule of thumb), which translates to c/B in path length.

| Signal | Bandwidth | Path resolution |
|---|---|---|
| BLE 1 Mbps | about 1 MHz | about 300 m |
| WiFi | 20 MHz | about 15 m |
| WiFi | 80 MHz | about 3.7 m |
| UWB | 499.2 MHz | about 0.6 m |

Values are computed. With a 100 ns delay spread, narrowband and WiFi signals receive overlapped echoes, while UWB can isolate a first path from reflections separated by more than about 60 cm. That is why UWB time-of-flight tolerates multipath well in LOS, and why RSSI and narrowband phase methods cannot. It does not make UWB immune: an attenuated first path may be missed and a later path detected instead.

## Reflection behaviour and polarization

Reflection depends on material permittivity, angle and polarization; indoor paths therefore arrive with mixed polarization [1]. Practical consequences:

- Metal and concrete surfaces produce strong reflections, so large halls and metal racks raise multipath.
- Circularly polarized LOS links suppress first-bounce reflections, because each reflection reverses handedness when incidence is below the Brewster angle; this does not hold where grazing incidence dominates, such as long hallways [1].
- A linear tag with a circular reader loses about 3 dB compared with a matched antenna (rule of thumb), so that loss should appear in any polarization-aware budget.
- Carpet and drapes reduce specular reflection mainly at millimetre-wave frequencies [1], so they are not a remedy in these bands.

Reflections can also help: they can keep NLOS links alive around metal blockers.

## Fresnel zone clearance and radius

The first Fresnel zone is the ellipsoid where indirect paths are within half a wavelength of the direct path. P.526 gives the radius R1 = 550 sqrt(d1 d2 / ((d1 + d2) f)) with d in km and f in MHz [4]. In metres and GHz: R1 = 0.55 sqrt(d1 d2 / ((d1 + d2) f)) (converted). Free-space behaviour is expected if no obstacle intrudes on this ellipsoid, and clearance of 0.6 R1 is the usual boundary of the diffraction zone [4].

| Path, midpoint radius | 920 MHz | 2.44 GHz | 5.18 GHz | 6.49 GHz |
|---|---|---|---|---|
| 10 m | 0.91 m | 0.56 m | 0.38 m | 0.34 m |
| 30 m | 1.57 m | 0.96 m | 0.66 m | 0.59 m |

Actionable: a rack top 30 cm below the ray is not clear at 920 MHz over 10 m. Mounting anchors higher clears Fresnel zones as well as direct rays.

## Knife-edge diffraction loss over obstacles

P.526 single knife-edge model: v = h sqrt((2/lambda)(1/d1 + 1/d2)), where h is obstacle height above the direct line, in consistent units. For v above -0.78, loss J(v) = 6.9 + 20 log10(sqrt((v - 0.1)^2 + 1) + v - 0.1) dB [4]. Key values, computed: 0 dB at v = -0.78; 6 dB when the ray just grazes (v = 0); 13.9 dB at v = 1; 20.9 dB at v = 2.4.

Example: an obstacle rising 1 m above the direct line, mid-way on a 20 m path, gives 14.6 dB at 920 MHz, 18.2 dB at 2.44 GHz and 22.2 dB at 6.49 GHz (computed). This idealized edge shows two facts. Grazing already loses 6 dB, and over-the-top paths lose modestly compared with passing through thick concrete. Use it to judge shelf and partition tops where the simulator ray merely hits or misses.

## NLOS effects on ranging and first path

NLOS has separate power and time effects. Passing through a material slows the wave: extra path length is thickness x (sqrt(e) - 1), with e the real permittivity. Using P.2040 values gives about 13 cm per 10 cm of concrete, 1.6 cm per 38 mm of wood and 0.8 cm per 12.5 mm of plasterboard (computed from [5]). This is a lower bound on range bias, since a weakened first path can be missed, and the receiver then locks to a later reflection or diffracted route. NIST noted that accurate through-material ranging requires compensating these delays [6].

The IEEE 802.15.4a model separates soft NLOS (n 3-4) from hard NLOS (n 4-7), which correspond to obstructed and blocked direct paths [7]. Use redundant LOS anchors rather than bias correction when accuracy matters. Bias is always positive on average, so an overdetermined solver can detect and reject it.

## Human body blockage and moving objects

At 1.7 GHz, a person stepping into an LOS path caused a 6-8 dB drop and a considerable reduction of K-factor, while in NLOS conditions people near antennas had no significant effect [1]. Static-link fading is bursty, caused by disturbed multipath or by people crossing the link [1]. P.1238 models a person as about 0.3 m wide [1].

Design implications: a wearable tag is often shadowed by its own wearer, so assume a body-side anchor may become NLOS and require coverage from at least two sides. The effect typically matters more for LOS-reliant links such as UWB. No opened source gives directly comparable body loss at 6.5 GHz, so test with people moving in a physical pilot rather than extrapolating 1.7 GHz numbers.

## Applying this in Sense Sim

Sense Sim casts one straight ray per link: there are no reflections, diffraction or delay spread. The result is LOS or crossed walls plus Gaussian noise. So it cannot show corridor gain, Fresnel loss near racks, or a null. Rack-top clearance is all-or-nothing, so judge marginal rays with the Fresnel and knife-edge sections above.

Noise applied: shadow_sigma_db for BLE and WiFi; RFID forward 2 dB and reverse 1 dB, which is smaller than the 6-10 dB Rician allowance for K = 7 dB, so add margin to RFID predictions. UWB NLOS adds bias_eq x nlos_bias_m plus an exponential term with mean nlos_sigma_m x bias_eq. Inspect nlos_pct and nlos_bias_cm in rtls_status, and nlos_links per tag in rtls_list_devices. Mount higher or add devices with rtls_optimize_anchors (tech, count, height, mount).

## Sources

1. Recommendation ITU-R P.1238-13, Propagation data and prediction methods for the planning of indoor radiocommunication systems — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.1238-13-202509-I!!PDF-E.pdf
2. Rayleigh fading — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Rayleigh_fading
3. 3GPP TR 38.901 v18.0.0, Study on channel model for frequencies from 0.5 to 100 GHz — ETSI/3GPP, 2024 — https://www.etsi.org/deliver/etsi_tr/138900_138999/138901/18.00.00_60/tr_138901v180000p.pdf
4. Recommendation ITU-R P.526-16, Propagation by diffraction — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.526-16-202511-I!!PDF-E.pdf
5. Recommendation ITU-R P.2040-4, Effects of building materials and structures on radiowave propagation in the range of 1 MHz to 450 GHz — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.2040-4-202509-I!!PDF-E.pdf
6. NISTIR 6055, Electromagnetic Signal Attenuation in Construction Materials — W. C. Stone, NIST, 1997 — https://nvlpubs.nist.gov/nistpubs/Legacy/IR/nistir6055.pdf
7. IEEE 802.15.4a channel model final report, doc 15-04-0535 — A. Molisch et al., IEEE, 2004 — https://www.ieee802.org/15/pub/04/15-04-0535-00-004a-tg4a-channel-model-final-report.pdf
