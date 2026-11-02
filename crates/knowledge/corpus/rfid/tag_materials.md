---
title: RFID tags on metal, liquid and cartons
domain: rfid
tags: tag detuning, on-metal tag, metal mount tag, anti-metal tag, foam spacer, standoff, liquid, water, glass, foil, carton tagging, corrugated, pallet tag, stretch wrap, flag tag, inlay selection, tag placement, tag orientation, tag to tag spacing, ARC spec, Auburn RFID Lab, AutoTune, HF RFID, NFC, LF RFID, 13.56 MHz, 125 kHz, 134 kHz, ISO 14443, material attachment loss, impedance mismatch
aliases: thẻ RFID trên kim loại, thẻ chống kim loại, thẻ dán trên chất lỏng, thẻ trên thùng carton, vị trí dán thẻ, lệch tần thẻ, chọn inlay, tags are not being read on metal shelves, tag does not read on a metal surface, label on a bottle will not read, where to stick the tag
verified: 2026-11-02
---
# RFID tags on metal, liquid and cartons

## Why metal and liquid degrade UHF RFID tags
Object attachment loss is the main reason a UHF tag that reads far in air fails on a product. Griffin and Durgin describe it as impedance mismatch plus reduced antenna gain when the tag is close to or attached to dielectric or conductive material [1]. Atlas RFID says passive UHF tags are very sensitive to liquids and metal because the waves reflect off, refract within or absorb into those materials [2]. A label's dipole-like antenna is tuned for one setting, so a nearby surface shifts its resonance away from the reader's band and the chip harvests less power. Georgia Tech's radio assay tested antennas against cardboard, wood, water containers, plastic, meat and metal for this reason: impedance, efficiency and pattern all change [3]. No universal dB table for materials was found in an accessible source, so measure with the real tag on the real product rather than assuming a loss.

## UHF RFID tags on metal: spacing and on-metal designs
A standard UHF label placed directly on metal generally does not work, so use a metal-mount tag or a real gap. Measured data show how quickly spacing matters: for one 80 x 44 mm tag on a 100 x 100 mm iron plate, the match stayed almost unchanged from 8 mm down to 4 mm of spacing, while at 0 mm a mismatch appeared around 840-880 MHz [4]. That is one design, not a general rule. Purpose-built tags use the metal as part of the antenna: Byondi and Chung's cavity tag, with a 10 mm cavity, reached 26 m measured at 920 MHz using a 4 W EIRP reader and a -20.5 dBm chip, with the metal object as the back conductor [5]. A corrugated-label vendor says keeping a tag about 1/4 inch from metal can help [6]. For on-metal needs, select a metal-rated tag and read its datasheet range on a plate.

## UHF RFID tags near liquid and water-filled goods
Water absorbs UHF energy, so tags on or behind liquid lose heavily. Nikitin, Rao and Lazar read a standard Gen2 tag inside a sports drink bottle with a far-field antenna in a near-field test, needing about 24 dBm at 890 MHz, equal to -36 dB of coupling for a -12 dBm chip [7]. A case-level report quoted by a label vendor found motor oil cases averaged 50% more successful reads than ketchup, and only about 25% of tags on water-filled cases under harsh conditions [6]; that article gives no citation, so treat it as anecdotal.

Auburn's tagging guide is the actionable rule: the tag must not be in front of, behind or touching liquid, and should be offset from it or placed above the liquid level [8]. Floral tags sit at the top above the water, and live plant tags above soil level [8]. A flag tag that stands off the container achieves the same goal.

## Tag placement rules from the Auburn tagging location guide
The Auburn RFID Lab tagging location guide gives these rules for retail items [8].

- Keep the tag away from metal, foil, liquid and glass: not in front of, behind or touching them. If it cannot be offset, ask the lab.
- Never on the bottom of a product. Hang tags must not sit toward the bottom.
- Tags must not be in very close proximity to each other when displayed or stored.
- Mylar and polybag items: tag toward the top.
- Aerosol can, side of cap: inlay about 10 mm above the bottom of the cap.
- Batteries on a pallet: at least two layers of hex comb separators between batteries, three preferred.
- One tag per product; one tag on one box for multi-box sets.

Stacked goods also shadow each other, so a tag facing inward on a pallet can be blocked by the contents, and the antenna design should reach all sides.

## RFID tag placement on cartons and pallets
For cartons, place the tag on the side away from metal or liquid contents and do not rely on the bottom face. A label vendor warns against tagging the underside of a box that will sit on a metal rack or pallet, and suggests a small offset or a metal-rated tag [6]. Pallet tags usually go on the stretch-wrapped area so they are thrown away with the wrap [9]. Reusable pallets instead take a tag behind a wood beam or moulded into a plastic base; metal pallets or metal contents may need a metal-mount tag and different placement [9]. If cartons are tagged, do not also apply a pallet-level tag to items on the pallet [9]. Speed matters too: the same vendor article reports close to 100% reads at 1 mph through a portal, and about 90% at 10 mph, without a named study [6]. Validate on a full pallet with the worst product at the centre.

## Choosing an RFID inlay and reading its datasheet
Pick an inlay for the material and orientation first, chip second. Two real examples: Paragon EX5030 has a 50 x 30 mm antenna with M730/M750 chips, with -24 dBm read and -21 dBm write [10]; Avery Dennison Eagle U9 has a compact 44 x 28 mm antenna with UCODE 9, aimed at apparel and retail [11]. Datasheet range plots apply to named materials such as free air, cardboard, PVC or fabric under lab conditions [10], so read the curve for the product's material, not the free-air one. Self-tuning chips help: Impinj AutoTune and NXP's automatic adjustment reduce the effect of mild detuning [12][13], yet they do not make a label an on-metal tag. Datasheets may list ARC specification letters such as F, G, I and W1-W6 [11], the Auburn ARC programme's test categories; the mapping of letters to product types was not verifiable, so confirm with ARC.

## Tag orientation sensitivity of linear inlays
Most RFID inlays are linearly polarised, dipole-like antennas with nulls off the ends. A circular reader antenna removes the polarisation angle problem at the price of about 3 dB, which is 70% of maximum range [14], but a tag whose axis points at the reader remains in the null. Orientation-sensitivity plots on inlay datasheets show this effect [10][11]. In a mixed pallet, tags point in many directions, so use several antennas with different viewing angles rather than extra power. Impinj's Core3D antenna is marketed for omnidirectional reading on M800 inlays [12]; verify it on real goods if orientation is uncontrolled. When items can be oriented on purpose, as on a conveyor, align the tag so its broad side faces the antenna.

## HF, NFC and LF RFID as alternatives to UHF
When metal, water or a short deliberate read zone dominate, compare HF and LF with UHF.

| Band | Typical read range | Materials |
|---|---|---|
| LF 125-134 kHz | A couple of cm to about 50 cm ideal [2] | Waves penetrate metal and water [2] |
| HF 13.56 MHz | A few cm to about 1 m by setup [2] | Passes most materials except water and dense metal; thin metals work [2] |
| NFC (13.56 MHz) | Up to 10 cm [15] | Interoperable with phones |
| Passive UHF | Average 2-5 m, maximum around 30 m [2] | Very sensitive to liquids and metal [2] |

TI's comparison rates multi-tag reading as tens for LF, hundreds for HF and thousands for UHF [15]. So LF or HF suits animals, liquids, tools and single-item identification; UHF wins for bulk inventory and portals. Sense Sim only simulates UHF RFID, so give LF and HF advice qualitatively.

## Applying this in Sense Sim
Sense Sim's RFID tag model has no material, detuning or orientation term. The `rfid-tag` profile uses `tag_sensitivity_dbm` -18 and `antenna_gain_dbi` 2 anywhere; only objects between reader and tag add loss through ray casting and `wall_loss_db`. A tag glued to a metal rack therefore looks fine in `rtls_coverage` and `rtls_run`, so warn the user that such a result does not validate tag choice.

To test a difficult product, add a JSON profile in `%APPDATA%\SenseSim\devices` with a worse `tag_sensitivity_dbm` by the loss you measured, place those tags with `rtls_add_device`, and compare `rtls_run` results. `rtls_auto_deploy` with `rfid` true puts tags on rack edges and goods, then lists unread tags in `rfid_unread`. Place tags at realistic faces rather than inside objects, since ray casting treats the object as a blocker.

## Sources
1. Multipath Fading Measurements for Multi-Antenna Backscatter RFID at 5.8 GHz — J. Griffin, G. Durgin, Georgia Tech, 2009 — https://propagation.ece.gatech.edu/Archive/PG_CP_090426_JDG/PG_CP_090426_JDG.pdf
2. Types of RFID and How They Are Used — atlasRFIDstore — https://rfid.atlasrfidstore.com/hubfs/Content/Types_of_RFID_and_How_They_Are_Used.pdf
3. The Radio Assay for Antennas — The Propagation Group, Georgia Tech, 2018 — https://www.propagation.gatech.edu/2018/10/31/the-radio-assay-for-antennas
4. Design and Experimental Evaluation of Modified Square Loop Feeding for UHF RFID Tags — Rokunuzzaman et al., PLoS ONE 10(7), 2015 — https://journals.plos.org/plosone/article?id=10.1371%2Fjournal.pone.0132530
5. Longest-Range UHF RFID Sensor Tag Antenna for IoT Applied for Metal and Non-Metal Objects — Byondi, Chung, Sensors 19(24), 2019 — https://pmc.ncbi.nlm.nih.gov/articles/PMC6960939/
6. RFID in corrugated environments: moisture, metal and real-world challenges — Digital Print Inc — https://www.digitalprint.com/dpi-news/rfid-in-corrugated-environments-moisture-metal-and-real-world-challenges
7. An Overview of Near Field UHF RFID — P. Nikitin, K.V.S. Rao, S. Lazar, IEEE RFID 2007 — https://people.ece.uw.edu/nikitin_pavel/papers/RFID_2007.pdf
8. Tagging Location Guide — RFID Lab, Auburn University — https://rfid.auburn.edu/alec/tagging_location_guide.php
9. Where do I put pallet RFID tags on my shipments — SimplyRFID — https://www.simplyrfid.com/faq/how-do-i-tag-pallets-with-rfid/
10. EX5030 M730/M750 inlay data sheet — Paragon ID, May 2025 — https://www.paragon-id.com/system/files/2025-05/pid-ex5030-m730-m750-may-2025.pdf
11. AD Eagle U9 inlay datasheet — Avery Dennison — https://rfid.averydennison.com/content/dam/rfid/en/products/rfid-products/data-sheets/datasheet-Eagle-U9.pdf
12. Impinj M800 Series product brief specification table — Impinj, 2024 — https://www.impinj.com/getmedia/42a028ff-b05f-468e-83af-ddd6534b57f1/Impinj-ProductBrief-M800-Series-SpecTable-120224.pdf
13. SL3S1206 UCODE 9 product data sheet — NXP, 2025 — https://nxp.com/docs/en/data-sheet/SL3S1206.pdf
14. Antennas and Propagation in UHF RFID Systems — P. Nikitin, K.V.S. Rao, IEEE RFID 2008 — https://people.ece.uw.edu/nikitin_pavel/papers/RFID_2008.pdf
15. Navigating your way through the RFID jungle — K. Sattlegger, U. Denk, Texas Instruments — https://ti.com/lit/pdf/slyy056
