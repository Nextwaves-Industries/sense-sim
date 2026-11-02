---
title: RFID reader antenna selection and installation
domain: rfid
tags: reader antenna, circular polarization, CP, RHCP, LHCP, linear polarization, dBic, dBil, axial ratio, gain, beamwidth, front to back ratio, VSWR, return loss, dock door, portal, doorway, gate, conveyor, tunnel, shelf antenna, smart shelf, overhead antenna, cable loss, coax, LMR, multiplexing, antenna hub, antenna ports, dwell time, stray reads, cross reads, RF shielding, read zone, motion sensor, TDMA
aliases: anten đầu đọc RFID, phân cực tròn, phân cực tuyến tính, cổng RFID, cửa kho, anten kệ, suy hao cáp, đọc nhầm thẻ, che chắn RF, reader picks up tags from the next door lane, antenna reads too far, how to mount an RFID antenna, gate antenna layout
verified: 2026-11-02
---
# RFID reader antenna selection and installation

## Circular versus linear polarisation for RFID reader antennas
Choose circular polarisation (CP) when tag orientation around the beam axis is unknown, and linear when it is controlled. CP reads a tag at any rotation, but a linear tag can capture at best half the power, which is 70% of the possible range [1]. Linear antennas give that power back on an aligned tag, yet lose the tag entirely when it is cross-polarised. Zebra's portal antenna is "multi-linear", and the Impinj xPortal alternated vertical and horizontal polarisation, both using diversity instead of pure CP [2][3]. Compare gains on the same basis: dBic for circular and dBil for linear differ by about 3 dB for a perfect CP antenna, and the difference depends on axial ratio [1]. A low axial ratio matters: Kathrein promotes one to reduce the dependence of results on tag rotation [4]. Sense Sim has no polarisation model, so this choice must be argued in text.

## RFID antenna gain and beamwidth examples
Antenna gain concentrates power, so high gain trades coverage width for reach, and regulation can cap it. Real products cover a wide range.

| Antenna | Polarisation | Gain | Horizontal beam | Notes |
|---|---|---|---|---|
| Times-7 Portal | RHCP | 10 dBic, 7 dBi equivalent | 50 deg | Front-to-back -20 dB [5] |
| Kathrein WRA 7070 | RHCP | 8.5 dBic typical | 65 deg half-power | Axial ratio 1 dB, front-to-back over 18 dB [4] |
| Times-7 A8060 door frame | Linear | 5 dBi | 110 deg horizontal, 30 deg vertical | 8 mm thick [6] |
| Zebra Multi-Linear Wave | Multi-linear | 8.1 dBi | 180 deg | Dock portals [2] |

For a narrow lane, a beam of 50 degrees reduces reads in neighbouring lanes [5]. For ETSI at full power, horizontal beamwidth must be 90 degrees or less [7]. In the USA gain above 6 dBi reduces permitted conducted power 1 dB for 1 dB [8], so higher gain narrows the beam without adding legal EIRP.

## RFID dock door and doorway portal design
Most dock door portals use two free-standing structures on opposite sides of the door, facing each other, each holding two to four antennas [3]. More antennas raise read accuracy, but also extend the reach, which makes cross-reading between portals worse [3]. Integrated examples show scale: Zebra's D800 is 244 cm tall for pallets, and its D500 is 158 cm tall [2]. Times-7 steers selection by opening: wider openings and taller loads use a full system, narrow openings a tower, and low pallets or dense goods a base unit [5]. One vendor guide suggests antennas at about 1.5 m and 2.5 m, angled 15-45 degrees inward (commonly 30), at 25-33 dBm, tuned down until the zone is contained [9]; this is vendor advice, not a standard. Use a motion or door trigger: ETSI lower band requires trigger support [7], and Zebra's dock door profile uses S2, target A with a motion sensor [10].

## RFID conveyor, shelf and overhead read zone designs
Conveyor: expect a short tag dwell and few tags in view. Zebra's conveyor profile sets population 5, S2, and 25 ms dwell per antenna [10]. One vendor layout uses four antennas (top, bottom, left, right) about 30-40 cm from the belt centre, near 25 dBm, with RF-absorbing sides to prevent cross-reads [9]. Measure actual tag speed, since speed sets the available reads.

Smart shelf: short-range antennas along the shelf illuminate tags from close by. Metal shelving and neighbouring levels reflect energy, so read across or along the edge with low power, and test reads at each level.

Overhead: ceiling antennas give wide zone coverage for presence but read through less of the stock, because stacked goods shadow tags lower down. No primary source with numeric guidance for overhead or shelf spacing was accessible, so derive coverage from the link budget and validate with a test read.

## RFID antenna spacing, aim and separation from metal
Aim the main beam at the tag path, not across open floor. Mount so the rear lobe faces away from the area you do not want to read: a -20 dB front-to-back ratio limits radiation behind the antenna [5]. Keep antennas off large metal surfaces, since one vendor guide calls for at least 50 mm of standoff [9]. Adjacent antennas should overlap for redundancy, because a tag at a null for one antenna may read from another, and multipath creates dead zones within nominal range [1]. Sources disagree on a universal minimum antenna separation, and no primary value was found, so do not quote one; instead, test with the reader's actual power and channel plan. Keep people clear per the antenna's regional RF exposure distance: Zebra instructs that antennas be positioned according to the specified requirements for the regulatory region [10].

## RFID antenna cable loss, connectors and match
Cable loss comes straight off forward power, and again off the backscatter on the receive path. Zebra FX readers take cable loss in dB per 100 ft and cable length (defaults 10 dB per 100 ft and 10 ft), and add the calculated loss to the configured port power [10]. Values must be accurate, because an overstated loss pushes radiated power above the intended level. Zebra caps conducted power at 30 dBm at the antenna input for US and Canada, at the cable end, not at the reader port, and limits gain including cable loss to 6 dBiL [10]. Keep runs short: one vendor guide suggests under 8 m of LMR-400 [9]. A good match matters at both ends: the R700 lost 5 dB of receive sensitivity from typical to challenging conditions, tied to return loss and cable delay [11], and Zebra asks for antenna VSWR of at most 1.4 [10]. Power off before connecting antennas, and never disconnect them while the reader is on, or the reader may be damaged [10].

## Reader multiplexing, antenna ports and dwell time
One reader radio serves its antennas in turn, so adding antennas shares reader time rather than adding throughput. The Impinj R700 has four monostatic RP-TNC ports, lists 32 read zones as its maximum, and is rated at up to 1100 reads per second for the whole reader [11]. Eight antennas on one reader therefore have one-eighth of the time each if dwell is equal. Antenna dwell and session settings together decide whether a moving tag gets read: short dwell suits conveyors [10], and single-target S2 prevents old tags consuming time. A second reader adds real capacity, but needs dense mode or time separation so they do not interfere. Where a read must be attributed to one door, one reader per door with antennas time-sliced is a simple structure.

## RFID stray reads and cross reads: mitigation
Stray reads are tags read outside the intended zone, and cross reads are tags read at the wrong portal. The University of Ottawa survey notes the core conflict: raising read accuracy also widens the zone, which increases cross-reading [3]. Mitigations, roughly in order of cost:

1. Lower port power until the zone just contains the load. Both vendors and surveys agree on tuning power down [9].
2. Narrow the beam and aim into the lane, for example a 50-degree beam with a strong rear rejection [5].
3. Activate readers only when needed: motion sensors and an on/off protocol that keep one dock door active at a time [3].
4. Filter in software: ignore tags seen only briefly, such as for one second [3]. This risks discarding real reads, so set it with care.
5. Metal shielding between portals or an enclosed metal space [3]. RF boundaries are never perfectly sharp, so shielding reduces rather than eliminates reads through reflections.

A reader's read record should therefore never be taken as proof of location by itself.

## Applying this in Sense Sim
Sense Sim's reader model is a single cosine-style pattern with `antenna_gain_dbi` and `beam_deg`, aimed by `yaw_deg` from `rtls_add_device` or `rtls_update_device`. Use `rfid-portal` (12 dBi, 40 degrees, 1.5 m, 8 m range) for doors and `rfid-reader` (9 dBi, 70 degrees, 2.2 m, 12 m range) elsewhere. `rtls_auto_deploy` with `rfid` true places a facing pair at each door-like opening, with both antennas at one height; sources usually use two to four per side, so add more for tall pallets. It also picks shelf aims and warns of reader pairs in each other's beam and of stray reads over 30% of the floor outside zones.

Define read areas with `rtls_add_zone` before `rtls_auto_deploy`, which scores reads inside zones and penalises reads outside them. `rtls_optimize_anchors` (`tech` "rfid", `count`, `height`, `target_coverage`, `mount`, `apply`) scores floor coverage only and receives no zones, so do not expect it to contain a read zone. Check with `rtls_coverage` and `rtls_run`. Each device is its own radio, so model antennas on one real reader with care, and cable loss is not modelled: subtract it in a custom profile's `tx_power_dbm`.

## Sources
1. Antennas and Propagation in UHF RFID Systems — P. Nikitin, K.V.S. Rao, IEEE RFID 2008 — https://people.ece.uw.edu/nikitin_pavel/papers/RFID_2008.pdf
2. Zebra Integrated RFID Portals fact sheet — Zebra Technologies — https://www.zebra.com/content/dam/zebra_dam/en_gb/fact-sheet/zebra-rfid-integrated-portal-fact-sheet-en-gb.pdf
3. Survey on UHF RFID portals — Borisenko, Li, Bolic, University of Ottawa, 2010 — https://ruor.uottawa.ca/bitstreams/0bf8d884-d6a2-4203-bd06-1e6af5f6cec3/download
4. WRA 7070 Antenna Unit datasheet — Kathrein — https://cisper.com/datasheets/kathrein/Kathrein_WRA_7070_Antenna_Unit_datasheet.pdf
5. Times-7 Portal RAIN UHF RFID system — WIoT Group product page — https://wiot-group.com/think/en/products/times-7-portal-rain-uhf-rfid-system/
6. SlimLine A8060 door frame antenna specification — Times-7 — https://rfid4ustore.com/content/data_sheet/A8060-71416%20Doorframe%20Antenna-speccsheet.pdf
7. ETSI EN 302 208 V3.3.1 — ETSI, 2020 — https://www.etsi.org/deliver/etsi_en/302200_302299/302208/03.03.01_60/en_302208v030301p.pdf
8. 47 CFR 15.247 — US Code of Federal Regulations, Cornell LII — https://www.law.cornell.edu/cfr/text/47/15.247
9. Ideal standard blueprint for warehouse logistics using choke-point portal architecture — Jewel Traker / Barcode Technologies Ltd, 2026 — https://www.jewel-traker.com/insight/ideal-standard-blueprint-for-warehouse-logistics-for-pallets-and-boxes-uses-choke-point-portal-architecture-with-fixed-mount-circularly-polarized-uhf-rfid-antennas-this-focuses-reliable-high-read-rate-identification-at-controlled-transition-points-rathe
10. FX Series RFID Fixed Reader Integration Guide — Zebra Technologies, 2024 — https://www.zebra.com/content/dam/zebra_new_ia/en-us/manuals/rfid/fxseries-ig-en.pdf
11. Impinj R700 Series RAIN RFID Readers Datasheet v2.0 — Impinj, 2023 — https://www.cisper.com/datasheets/impinj/Impinj_R700_Series_RAIN_RFID_Readers_Datasheet_2023.pdf
