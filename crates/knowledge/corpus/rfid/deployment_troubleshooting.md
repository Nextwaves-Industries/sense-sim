---
title: RFID deployment troubleshooting and site survey
domain: rfid
tags: RFID troubleshooting, missed reads, low read rate, tags not read, stray reads, cross reads, unread tags, interference, reader collision, read accuracy, benchmark, site survey, spectrum analyzer, noise floor, RSSI, ARC, Auburn RFID Lab, RAIN Alliance, ISO 18046, pilot test, tag population, dead zone, write failure, antenna fault, VSWR
aliases: khắc phục sự cố RFID, thẻ không đọc được, tỉ lệ đọc thấp, đọc nhầm thẻ, khảo sát hiện trường RFID, nhiễu đầu đọc, too many tags read rate falls, reader picks up tags from the next door lane, tags are missed at the gate, RFID does not work reliably, why some tags are never read
verified: 2026-11-02
---
# RFID deployment troubleshooting and site survey

## RFID symptom to cause to fix table
Quick triage for a passive UHF RFID site. Change one thing at a time and re-test on real goods.

| Symptom | Likely cause | First fix |
|---|---|---|
| Some tags never read | Tag on or behind metal or liquid; tag in null | Move tag, use metal-rated tag, add a viewing angle |
| Range far below budget | Attachment loss, cable loss, wrong region or wrong power setting | Measure a tag in air, then on product |
| Reads collapse with many tags | Session or population wrong; S0 repeats crowd out weak tags | Single target with S2 or S3; correct population setting |
| Tags at neighbouring door read | Power too high, wide beam, reflections | Reduce power, narrow and re-aim, gate with sensor |
| Reads drop when a second reader starts | Reader interference | Dense mode, or time-slice readers |
| Reads fine but writes fail | Write needs more power | Move closer; check chip write sensitivity |
| Reads vary by position, not by distance | Multipath dead zones | Add antennas, move antenna, keep goods moving |

## RFID tags not read on metal shelves or liquid goods
When a specific product or location fails while others read, suspect the tag's surroundings first. Attachment to metal or liquid causes impedance mismatch and antenna gain reduction [1]. Test in this order: read the same tag in free air, then on the product, and record the power needed in each case. A big jump points to placement or tag choice. Auburn's guide says tags must not be in front of, behind or touching metal, foil, liquid or glass, must not be on the bottom of a product, and must not sit very close to other tags [2]. Move the tag, use a metal-mount tag on metal surfaces, or use a flag design above a liquid level. A box on a metal rack with the tag on its underside is a common failure, and a small offset or a metal-rated tag addresses it [3]. More reader power rarely rescues this, and regional limits cap it anyway.

## RFID read rate falls with too many tags in the field
When read rate per tag falls as population grows, separate reader capacity from protocol settings. Reader maximums are shared: the R700's up to 1100 reads per second applies to the whole reader, in mode 0 and an FCC-like region only [4]. Check the following in order.

1. Session and target: S0 or dual target keeps every strong tag answering. Single target S2 or S3 silences read tags so weak ones get slots [5].
2. Population or Q setting: Zebra's profiles use 300 for dock doors and 5 for conveyors, and a wrong seed wastes slots or causes collisions [6].
3. RF mode: a fast mode loses tags when other readers are near. Try a dense M4 or M8 mode; note it reduces rate [4].
4. Dwell: antennas sharing a reader each get a fraction of its time.
5. Unique count: report unique tags, not total reads, because a high read count can hide a low unique count.

## Stray reads and cross reads at RFID portals
Tags read from the next dock door or from nearby stock are stray or cross reads. The first step is to lower power until the wanted load only just reads, since raising read accuracy also widens the zone [7]. Then narrow the beam, aim into the lane, and check for reflecting metal doors or racking. A 50-degree beam with -20 dB rear rejection is one example of containing a lane [8]. Add operational gating so readers transmit only when a motion sensor triggers, and keep one dock active at a time where traffic allows [7]. Software rules that ignore briefly seen tags reduce false positives, but discard real data if set too tight [7]. Metal shielding or an enclosed space is the physical option [7], but expect it to reduce cross-reads rather than end them. Compare reads with the shipping manifest where one exists, since reconciling tags against expected goods catches errors RF cannot.

## RFID reader interference and missed reads with several readers
A neighbouring reader's carrier is far stronger than a tag's reflected reply, so it can swamp that reply in the same channel. If read rate or range drops only when another reader is on, this is the likely cause. Use a dense interrogator mode so tag replies fall beside the reader channels, as Gen2 Annex G and ETSI EN 302 208 both describe [5][9]. In ETSI's lower band, only four high-power channels exist, so large sites must share them [9]. Time-slice readers where dense mode is not enough, a simple TDMA method named in the portal survey [7]. Readers should also be kept away from electromagnetic noise sources: Zebra lists generators, pumps, converters, UPS units, AC switching relays and light dimmers [6]. Finally, check that all readers are configured for the same region. A misconfigured unit can occupy channels you do not expect.

## RFID benchmark read rates and acceptance targets
Benchmarks describe a controlled test, not your site, so state conditions next to any number. A label vendor reports many operations reaching 99.5% or higher after tuning, with close to 100% at 1 mph and about 90% at 10 mph for pallet moves, and notes water-filled cases can fall to 25% in harsh conditions; those figures lack named studies, so treat them as anecdotal [3].

Reader specifications use defined methods: Impinj reports sensitivity by the RAIN Alliance and ISO/IEC 18046-2:2020 method with 90% packet success [4]. Tag performance is commonly tested with ARC specifications from the Auburn RFID Lab, with spec letters on inlay datasheets [10]. The ARC method documents themselves were not accessible, so do not quote ARC thresholds without the current spec. Set acceptance as: required unique read percentage, on the worst real product, at real speed, with the worst orientation, over a stated number of passes.

## RFID site survey checklist before deployment
A site survey establishes what the RF design must cope with. Steps, from vendor deployment checklists [11] and the sources noted:

1. Confirm the region, band and legal EIRP, and that reader hardware matches it.
2. Measure ambient RF noise with a spectrum analyser across the operating band, and log interference sources [11].
3. Map metal structures, roller doors, racking, liquids and anything that reflects or blocks RF.
4. Note power, network and mounting points, and cable runs. Keep the reader away from noise sources [6].
5. Define portal dimensions from the item size and movement [11].
6. Test real tags on real products, at real speeds, while recording read rate and missed-read count [11].
7. Tune power, angle and settings, then re-test with the neighbouring zones running.

No published numeric limit for acceptable noise was found, so use the reader vendor's value.

## RAIN Alliance and ARC guidance status
RAIN RFID is the market name datasheets use for the Gen2 and ISO 18000-63 protocol [12]. The Impinj R700 datasheet cites RAIN Alliance and ISO/IEC 18046-2:2020 recommendations for receive sensitivity testing [4]. No RAIN Alliance deployment guide with numeric limits could be opened for this note, so treat claims attributed to RAIN Alliance with care and check rainrfid.org. For ARC, inlay datasheets declare compliance with lettered specs: Avery Dennison lists F, G, I, K, M, Q, N, L and W1-W6 for Eagle U9, for example [10], and Paragon's sheet for EX5030 does similar. Use those as a shortlist, and ask the label vendor which spec covers the product category. A retailer's requirement often names a spec, which then takes priority over this general advice.

## Applying this in Sense Sim
Diagnose RFID in Sense Sim with these steps. Call `rtls_list_devices` to see positions, then `rtls_coverage` with `tech` "rfid" and the tag `height` to see margin and blocked areas. Call `rtls_run` with `seconds` and `reset` true, then read `rfid_reads_per_s` and per-tag `rfid_reads`; `rtls_status` gives live values. `rtls_auto_deploy` with `rfid` true reports `rfid_unread` and notes for reader conflicts and stray reads. Simulate a failed reader with `rtls_update_device` and `enabled` false.

Fix by moving with `rtls_update_device` (`position`, `yaw_deg`) or adding through `rtls_optimize_anchors` with `tech` "rfid". The tag card shows each reader's RSSI for a selected tag. The simulator can show geometry and blocking problems. It cannot reproduce detuning, orientation, polarisation, session effects, reader-to-reader interference on a live read, or deep multipath, so never present a clean result as proof for those.

## Sources
1. Multipath Fading Measurements for Multi-Antenna Backscatter RFID at 5.8 GHz — J. Griffin, G. Durgin, Georgia Tech, 2009 — https://propagation.ece.gatech.edu/Archive/PG_CP_090426_JDG/PG_CP_090426_JDG.pdf
2. Tagging Location Guide — RFID Lab, Auburn University — https://rfid.auburn.edu/alec/tagging_location_guide.php
3. RFID in corrugated environments: moisture, metal and real-world challenges — Digital Print Inc — https://www.digitalprint.com/dpi-news/rfid-in-corrugated-environments-moisture-metal-and-real-world-challenges
4. Impinj R700 Series RAIN RFID Readers Datasheet v2.0 — Impinj, 2023 — https://www.cisper.com/datasheets/impinj/Impinj_R700_Series_RAIN_RFID_Readers_Datasheet_2023.pdf
5. EPC Radio-Frequency Identity Generation-2 UHF RFID Standard, Release 3.0.1 — GS1, Feb 2026 — https://ref.gs1.org/standards/gen2
6. FX Series RFID Fixed Reader Integration Guide — Zebra Technologies, 2024 — https://www.zebra.com/content/dam/zebra_new_ia/en-us/manuals/rfid/fxseries-ig-en.pdf
7. Survey on UHF RFID portals — Borisenko, Li, Bolic, University of Ottawa, 2010 — https://ruor.uottawa.ca/bitstreams/0bf8d884-d6a2-4203-bd06-1e6af5f6cec3/download
8. Times-7 Portal RAIN UHF RFID system — WIoT Group product page — https://wiot-group.com/think/en/products/times-7-portal-rain-uhf-rfid-system/
9. ETSI EN 302 208 V3.3.1 — ETSI, 2020 — https://www.etsi.org/deliver/etsi_en/302200_302299/302208/03.03.01_60/en_302208v030301p.pdf
10. AD Eagle U9 inlay datasheet — Avery Dennison — https://rfid.averydennison.com/content/dam/rfid/en/products/rfid-products/data-sheets/datasheet-Eagle-U9.pdf
11. RFID Portal Deployment Checklist: Survey to Calibration — RFID4UStore — https://rfid4ustore.com/rfid-blog/rfid-portal-deployment-checklist-survey-to-calibration/
12. Impinj M800 Series product brief specification table — Impinj, 2024 — https://www.impinj.com/getmedia/42a028ff-b05f-468e-83af-ddd6534b57f1/Impinj-ProductBrief-M800-Series-SpecTable-120224.pdf
