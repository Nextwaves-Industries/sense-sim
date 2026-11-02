---
title: Passive UHF RFID link budget and read range
domain: rfid
tags: link budget, read range, tag range, forward link, reverse link, backscatter, Friis, free space path loss, FSPL, EIRP, ERP, tag sensitivity, chip sensitivity, tag IC, Impinj M700, M730, M750, M800, M830, M850, NXP UCODE 8, UCODE 9, reader sensitivity, Impinj R700, R420, CISC, packet success rate, modulation loss, polarization loss, dBic, dBil, fade margin, multipath, dead zones, write range, write sensitivity
aliases: quỹ đường truyền RFID, khoảng cách đọc thẻ, độ nhạy thẻ, độ nhạy đầu đọc, công suất phát, đường xuống đường lên, tán xạ ngược, how far can a passive tag be read, how far does an RFID tag read, reading distance, why the tag does not wake up, tag not powered
verified: 2026-11-02
---
# Passive UHF RFID link budget and read range

## Forward link and reverse link in passive RFID
A passive RFID read needs two links to close. The forward link is reader to tag: the tag must collect enough RF power to run its chip. The reverse (backscatter) link is tag to reader: the reader must detect the weak reflected signal. Nikitin and Rao call these the tag-power-limited case and the reader-sensitivity-limited case [1]. In the forward case the reader hears the tag as soon as it powers up; in the reverse case the tag stays powered at distances the reader can no longer detect it [1]. Current fixed readers mostly leave the forward link as the limit, as the worked examples below show, so extra reader sensitivity rarely adds range unless a very sensitive tag or a noisy site shifts the balance. Backscatter crosses the path twice, so reverse-link power falls 2 dB for every 1 dB of one-way path loss.

## Tag read range equation in free space
Free-space tag read range from Nikitin and Rao, forward-link limited [1]:

r = (lambda / 4 pi) x sqrt(EIRP x G_tag x p x tau / P_th)

Terms are lambda (wavelength in m), EIRP (W), G_tag (linear tag gain), p (polarisation efficiency), tau (chip-antenna power transfer coefficient) and P_th (chip threshold in W) [1].

In dB form: FSPL(dB) = 20 log10(4 pi d / lambda). This gives 31.73 dB at 1 m and 920.5 MHz, and 31.68 dB at 915 MHz. Every doubling of distance adds 6 dB, so 6 dB of extra loss halves range and 6 dB of gain doubles it.

Worked check from the same paper: 36 dBm EIRP and a tag needing -14 dBm incident power gives a 50 dB path limit, or 27 ft (8.2 m) [1]. That tag was a -12 dBm chip of its era; today's chips are about 12 dB better.

## Tag IC read and write sensitivity values
Published tag chip sensitivities, best case on a matched dipole. Chip figures do not describe a finished tag on a product.

| Chip or inlay | Read (dBm) | Write (dBm) | Source note |
|---|---|---|---|
| Impinj M830, M850 | -25.5 | not listed | up to 2 dB better with Gen2X Performance [2] |
| Impinj M730, M750 in Paragon EX5030 | -24.0 | -21.0 | inlay datasheet [3] |
| NXP UCODE 9 | -24 | -22 | on 2.15 dBi antenna [4] |
| NXP UCODE 8 | -23 | -18 | on 2.15 dBi antenna [5] |

The Avery Dennison Eagle U9 sheet notes improved read and write sensitivity for UCODE 9 over UCODE 8 [6]. No published figure was found for the Quanray Qstar-7U chip named in Sense Sim's default tag, so ask the vendor. Chip sensitivity gain has limits as a selection factor: a poor antenna or a bad attachment surface easily costs more than the 1-2 dB between current generations.

## Datasheet tag sensitivity versus chip threshold convention
Tag sensitivity and chip threshold are different quantities. Nikitin and Rao define tag power sensitivity as the signal strength needed at the tag location, a function of chip threshold, tag gain and match [1]. NXP footnotes its sensitivity as "tag sensitivity on a 2.15 dBi gain antenna" [4][5]. Read that way, the -24 dBm figure already includes antenna gain and should be compared directly with EIRP minus path loss. Treating it as the power at the chip and then adding tag gain as well is more optimistic by about 2 dB. Impinj just says "read sensitivity" under a spec table with no antenna note [2]. For design, use the conservative reading: incident power = EIRP - FSPL, required to be at least the datasheet figure. Then add margins for the real tag, and confirm with a measured tag if range matters.

## Reader receive sensitivity values and how they are measured
Reader sensitivity depends on RF mode and on the antenna match and self-jammer leakage, so a single number is misleading. The Impinj R700 uses a RAIN Alliance and ISO/IEC 18046-2:2020 method with a CISC tag emulator and 90% packet success, in Dense Reader M8 at 30 dBm [7].

| Reader | Sensitivity | Condition |
|---|---|---|
| Impinj R700 / R720, ideal | -94 dBm | return loss 22 dB or better |
| Impinj R700 / R720, typical | -93 dBm | return loss 15 dB |
| Impinj R700 / R720, challenging | -88 dBm | return loss 12 dB, 38 ns delay |
| Impinj R420, R220, typical | -84 dBm | Impinj comparison table |

The 5 dB fall from typical to challenging shows why antenna match and cable quality matter [7]. Zebra requires antenna VSWR of at most 1.4 for optimum FX reader RF performance [8]. Sense Sim's built-in -85 dBm sits near the older readers and is 8 dB less sensitive than the R700's typical value.

## ERP, EIRP, dBi and dBic in an RFID budget
RFID budgets mix three references. EIRP is relative to an isotropic source and ERP to a half-wave dipole, so EIRP = ERP + 2.15 dB, or 1.64 times in power [9]. Example: 2 W ERP is 3.28 W EIRP (35.15 dBm).

Reader antennas are often quoted in dBic. Nikitin and Rao give the relation G(dBic) = G(dBil) + 3 + 20 log10((1 + 10^(-AR/20)) / 2) where AR is axial ratio in dB, and note that the common 3 dB difference holds only for perfect circular polarisation [1]. A linear tag aligned anywhere with a circular antenna gets polarisation efficiency of at best 0.5, which is 70% of the possible range [1]. Count this once: either use dBic and subtract 3 dB for polarisation, or use the linear equivalent gain and subtract nothing. Times-7 lists 10 dBic as 7 dBi equivalent [10]. Subtract cable and connector loss from conducted power too.

## Worked read range example at Vietnam, FCC and ETSI limits
Free space, aligned tag, no attachment loss, datasheet sensitivity used as incident power. Range is arithmetic from the equation in this file, not a measured result.

| Limit | EIRP | Freq | Tag | Max path loss | Free-space range | Range with 10 dB margin |
|---|---|---|---|---|---|---|
| Vietnam 500 mW ERP | 29.15 dBm | 920.5 MHz | -24 dBm | 53.15 dB | 11.8 m | 3.7 m |
| Vietnam 500 mW ERP | 29.15 dBm | 920.5 MHz | -25.5 dBm | 54.65 dB | 14.0 m | 4.4 m |
| ETSI 2 W ERP | 35.15 dBm | 866.9 MHz | -24 dBm | 59.15 dB | 24.9 m | 7.9 m |
| FCC 4 W EIRP | 36 dBm | 915 MHz | -24 dBm | 60 dB | 26.1 m | 8.2 m |

Atlas RFID quotes an average 2-5 m for low-cost passive labels, and a maximum around 30 m [11], which fits the margin column better than free space. Vietnam's limit costs about half the range of FCC. Plan with the margin column until a site measurement says otherwise.

## Reverse link check for a worked RFID example
Reverse-link check, using the Vietnam row at the forward limit: incident power -24 dBm at 11.8 m, path loss 53.15 dB, a 2 dBi tag and a 6 dBi linear-equivalent reader antenna. Modulation loss of 6 dB is a rule of thumb, and Sense Sim uses the same value.

Received power = incident (-24) + tag gain (2) - modulation (6) + tag gain (2) + reader gain (6) - path loss (53.15) = -73.15 dBm.

Margins: 10.85 dB against -84 dBm (R420 class), and about 20 dB against -93 dBm (R700 typical) [7]. So the forward link is the limit by a wide margin. Reverse-limited cases arise when the reader's sensitivity is degraded: strong self-jammer from a poor antenna match, reader-to-reader interference, or multiple reflected carriers. If a tag powers up near the reader but reads fail, look at the receive side and at interference, not at transmit power.

## Multipath fading and dead zones in UHF RFID
Free-space range is not a guarantee, because multipath both extends range and creates holes. For the 50 dB path budget above, ground reflection extends the free-space 27 ft to 37 ft at 5 ft height, or 48 ft at 6 ft, but also creates dead zones inside that range [1]. Backscatter fades harder than one-way links because the signal crosses the channel twice. Griffin and Durgin measured fades up to 40 dB in a non-line-of-sight backscatter channel at 5.79 GHz, with these fade margins [12]:

| Outage probability | Fade margin (dB), one tag antenna |
|---|---|
| 0.5 | 3.4-4.1 |
| 0.1 | 14-16 |
| 0.05 | 18-20 |
| 0.01 | 24-27 |

That is a different band and a rich-scattering room, so use it as an order of magnitude only. Remedies are frequency hopping, several antennas at different spots or polarisations, and moving tags through the zone.

## Tag write range versus read range
Writing needs more power than reading, so an encoding station must be closer. Nikitin and Rao put write sensitivity typically about 3 dB worse than read, and write range typically 70% of read range [1]. Datasheets show a spread: UCODE 9 is 2 dB worse at -22 dBm versus -24 dBm [4], M730 and M750 inlays are 3 dB worse at -21 dBm versus -24 dBm [3], and UCODE 8 is 5 dB worse at -18 dBm versus -23 dBm [5]. A 5 dB gap cuts range to 56% in free space. Do not validate an encode-at-distance workflow from read tests alone. Near-field encoders suit this job and also avoid programming a neighbouring tag by mistake.

## Applying this in Sense Sim
Sense Sim's link model (`rfid_link` and `rfid_margins` in src/rtls/radio.rs): forward = `tx_power_dbm` + reader gain + tag `antenna_gain_dbi` - FSPL - `wall_loss_db` x wall count, compared with `tag_sensitivity_dbm`. Reverse = forward - 6 + tag gain + reader gain - loss, compared with `sensitivity_dbm`. Reads are also cut off beyond `max_range_m`. It has no polarisation, tag attachment or tag orientation loss, and fading is only 2 dB and 1 dB sigma.

Defaults: 39 dBm EIRP, -18 dBm tag, 2 dBi. The free-space forward limit is about 23 m, so the 12 m `max_range_m` cap rules. Those 39 dBm exceed common limits, while -18 dBm is 6 dB worse than a -24 dBm chip; the two partly offset.

Use `rtls_coverage` with `tech` "rfid" and `height` for a heatmap coloured by margin. Require well above 0 dB margin, not only "covered": the planner counts a point at read chance 0.9, about 2.6 dB. Then confirm with `rtls_run`.

## Sources
1. Antennas and Propagation in UHF RFID Systems — P. Nikitin, K.V.S. Rao, IEEE RFID 2008 — https://people.ece.uw.edu/nikitin_pavel/papers/RFID_2008.pdf
2. Impinj M800 Series product brief specification table — Impinj, 2024 — https://www.impinj.com/getmedia/42a028ff-b05f-468e-83af-ddd6534b57f1/Impinj-ProductBrief-M800-Series-SpecTable-120224.pdf
3. EX5030 M730/M750 inlay data sheet — Paragon ID, May 2025 — https://www.paragon-id.com/system/files/2025-05/pid-ex5030-m730-m750-may-2025.pdf
4. SL3S1206 UCODE 9 product data sheet — NXP, 2025 — https://nxp.com/docs/en/data-sheet/SL3S1206.pdf
5. SL3S1205/15 UCODE 8/8m product data sheet — NXP — https://nxp.com/docs/en/data-sheet/SL3S1205_15.pdf
6. AD Eagle U9 inlay datasheet — Avery Dennison — https://rfid.averydennison.com/content/dam/rfid/en/products/rfid-products/data-sheets/datasheet-Eagle-U9.pdf
7. Impinj R700 Series RAIN RFID Readers Datasheet v2.0 — Impinj, 2023 — https://www.cisper.com/datasheets/impinj/Impinj_R700_Series_RAIN_RFID_Readers_Datasheet_2023.pdf
8. FX Series RFID Fixed Reader Integration Guide — Zebra Technologies, 2024 — https://www.zebra.com/content/dam/zebra_new_ia/en-us/manuals/rfid/fxseries-ig-en.pdf
9. Effective radiated power — Wikipedia — https://en.wikipedia.org/wiki/Effective_radiated_power
10. Times-7 Portal RAIN UHF RFID system — WIoT Group product page — https://wiot-group.com/think/en/products/times-7-portal-rain-uhf-rfid-system/
11. Types of RFID and How They Are Used — atlasRFIDstore — https://rfid.atlasrfidstore.com/hubfs/Content/Types_of_RFID_and_How_They_Are_Used.pdf
12. Multipath Fading Measurements for Multi-Antenna Backscatter RFID at 5.8 GHz — J. Griffin, G. Durgin, Georgia Tech, 2009 — https://propagation.ece.gatech.edu/Archive/PG_CP_090426_JDG/PG_CP_090426_JDG.pdf
