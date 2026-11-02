---
title: UHF RFID regional regulation, bands and power limits
domain: rfid
tags: UHF RFID regulation, frequency band, power limit, ERP, EIRP, ETSI EN 302 208, 865-868 MHz, 915-921 MHz, CEPT, EU 2018/1538, FCC Part 15.247, 902-928 MHz, 4 W EIRP, FHSS, frequency hopping, LBT, listen before talk, China 920-925 MHz, MIIT 76, SRRC, Japan ARIB STD-T106, STD-T107, 916.7-920.9, 916.7-923.5, Vietnam, 918-923 MHz, 918.4-923 MHz, 500 mW ERP, Circular 08/2021/TT-BTTTT, QCVN, Singapore, Malaysia, Thailand, South Korea, Australia, India, Hong Kong, channel plan, GS1 UHF overview
aliases: quy định tần số RFID Việt Nam, băng tần RFID 918-923 MHz, công suất phát RFID, giới hạn công suất ERP, Thông tư 08/2021/TT-BTTTT, thiết bị nhận dạng vô tuyến điện, miễn giấy phép tần số, thu phí không dừng ETC, what frequency can RFID use, is this reader power legal, allowed transmit power by country
verified: 2026-11-02
---
# UHF RFID regional regulation, bands and power limits

## UHF RFID power limits by region at a glance
All regional limits below were checked on 2026-11-02. Treat them as design inputs, and confirm with the national regulator before any deployment; GS1 calls its own table unofficial [1].

| Region | Band (MHz) | Limit | Licensing or technique |
|---|---|---|---|
| EU lower band | 865-868, four channels | 2 W ERP | ETSI EN 302 208 [2] |
| EU upper band | 915-921, three channels requested | 4 W ERP | Not all states [2][1] |
| USA | 902-928 | 1 W conducted + 6 dBi (4 W EIRP) | FHSS, at least 50 channels [3][1] |
| China | 920-925 (new rule) | see China section | Type approval [4] |
| Japan, licensed | 916.7-920.9 | 4 W EIRP | LBT or LBT-free [1][5] |
| Japan, unlicensed | 916.7-923.5 | 0.5 W EIRP | LBT [1][6] |
| Vietnam | 918.4-923 | 500 mW ERP | FHSS, LBT or duty cycle [7] |

Convert with EIRP (dBm) = ERP (dBm) + 2.15 dB, a ratio of about 1.64 [8]. For other APAC countries see the GS1 table section.

## ETSI EN 302 208 lower band 865-868 MHz rules (Europe)
Checked 2026-11-02 against ETSI EN 302 208 V3.3.1 (2020-08) [2]. Interrogators transmit only on four high-power channels of 200 kHz, centred at 865.7, 866.3, 866.9 and 867.5 MHz. The limit is 33 dBm ERP (2 W) in 200 kHz.

Requirements an installer must plan for:
- Horizontal beamwidth: no restriction up to 500 mW ERP; at most 180 degrees up to 1 W ERP; at most 90 degrees up to 2 W ERP.
- Continuous transmission on one channel is at most 4 s, then at least 100 ms off that channel. Switching at once to another high-power channel is allowed.
- Readers must support triggers that indicate tagged objects arriving, and must stop transmitting after they cease reading further tags.
- A presence-sensing mode allows bursts under 1 s, separated by at least 100 ms.

So an ETSI lower-band portal should use a motion sensor or light beam and cannot radiate continuously over an empty doorway.

## ETSI upper band 915-921 MHz rules (Europe)
Checked 2026-11-02 against ETSI EN 302 208 V3.3.1 [2]. High-power channels are 400 kHz wide with the lowest centred at 916.3 MHz and spacing of 1.2 MHz, which gives 916.3, 917.5, 918.7 and 919.9 MHz in the plan. The limit is 36 dBm ERP (4 W) in 400 kHz, and transmissions should last no longer than needed.

Beamwidth in the horizontal plane: no restriction up to 1 W ERP, at most 180 degrees up to 2 W ERP, at most 90 degrees up to 4 W ERP.

EU decision 2018/1538 asks member states to implement only three channels (916.3, 917.5, 918.7 MHz); some implement fewer or none, and may restrict use to professional installers [2][1]. Parts of the band are held by military services or by railway ER-GSM in some countries [2]. France requires deployments to be declared to ANFR, per the GS1 overview [1]. Never assume the upper band is available in a European country without checking CEPT ERC/REC 70-03.

## FCC Part 15.247 rules for 902-928 MHz RFID readers (USA)
Checked 2026-11-02 against 47 CFR 15.247 [3]. A frequency-hopping reader in 902-928 MHz with at least 50 hopping channels may use up to 1 W conducted output; with 25 to 49 channels the limit is 0.25 W. Those limits assume antenna gain up to 6 dBi. Above 6 dBi, conducted power must drop by the same number of dB as the excess gain, so 9 dBi requires 27 dBm.

Other limits: a hopping channel's 20 dB bandwidth may not exceed 500 kHz. If that bandwidth is under 250 kHz, at least 50 frequencies are required and average occupancy is at most 0.4 s in any 20 s. At 250 kHz or more, at least 25 frequencies and 0.4 s in 10 s apply [3].

The practical budget is 4 W EIRP (36 dBm) [1]. Zebra states 30 dBm maximum at the antenna input and 6 dBiL maximum gain including cable loss for its FX readers [9].

## Vietnam UHF RFID band 918.4-923 MHz and 500 mW ERP limit
Checked 2026-11-02. Vietnam Circular 08/2021/TT-BTTTT (in force 28 November 2021), Appendix 6, lets licence-exempt RFID use 918.4-923 MHz at up to 500 mW ERP [7]. Devices must use an interference mitigation technique such as frequency-hopping spread spectrum, LBT or a duty-cycle limit, and the hopping channel's 20 dB bandwidth may be at most 500 kHz [7].

Toll-collection readers in 920-923 MHz above 500 mW ERP need a licence and are excluded from the exemption [7]; GS1 lists them at 2 W ERP for licensed ETC only [1]. GS1 also lists 866-868 MHz at 0.5 W ERP for Vietnam, and rounds the main band to 918-923 MHz [1]. The circular text checked does not show 866-868 MHz in Appendix 6, so do not rely on it [7]. Circular 01/2025/TT-BKHCN amends Appendices 2, 10 and 16 only, not the RFID appendix [10]. Budget: 500 mW ERP = 27 dBm ERP = 29.15 dBm EIRP.

## China UHF RFID rule MIIT 2024 No. 76
Checked 2026-11-02. MIIT Notice (2024) No. 76, published 7 May 2024, applies from 1 November 2024 and sets 920-925 MHz as the working band for 900 MHz RFID [4]. New type approval for 840-845 MHz transmitters has ended, though equipment approved earlier may stay in use until retired [4]. A search-result summary of Article 6 mentions a permission requirement for use close to railway tracks; the page could not be opened, so verify the distance in the notice.

A third-party copy of the technical annex gives 920.5-924.5 MHz at 2 W ERP, and 100 mW ERP in 920-920.5 and 924.5-925 MHz, with frequency hopping over 20 channels, at most 250 kHz occupied bandwidth and 2 s dwell [13]. That copy does not itself cite No. 76, though GS1's older data agrees on the main band and power [1]. Confirm against the MIIT text, and expect SRRC type approval to be required.

## Japan UHF RFID: ARIB STD-T106 and STD-T107
Checked 2026-11-02. Japan has two tiers in the 920 MHz band.

High power, ARIB STD-T106: 916.7-920.9 MHz for premises and land mobile radio stations aimed at high-output passive tag systems, with a 1 W station reference; version 2.1 was issued in 2026 [5]. GS1 lists up to 4 W EIRP with a licence [1].

Low power, ARIB STD-T107 (specified low-power station): antenna power up to 250 mW and antenna gain up to 3 dBi, so 27 dBm EIRP. Sealed equipment under 27 dBm EIRP may raise antenna power to 500 mW. Carrier sense of at least 5 ms applies, with transmission up to 4 s and a pause of at least 50 ms for that class [6]. GS1 summarises this as 916.7-923.5 MHz at 0.5 W EIRP with LBT [1].

The Impinj R700 Japan model is specified at 916.7-920.9 MHz and limited to 30 dBm conducted [11].

## GS1 UHF allocation table for other APAC countries
GS1 lists these allocations in its overview dated 10 February 2022, an unofficial summary [1]. A 2025 edition exists but could not be opened. Treat every row as a lead to verify, checked 2026-11-02.

| Country | Band (MHz) | Limit | Note |
|---|---|---|---|
| Thailand | 920-925 | 4 W EIRP, FHSS | Licence above 0.5 W per GS1; a 2025 summary says above 50 mW [17] |
| Malaysia | 919-923 | 2 W ERP | Up to 4 W by apparatus assignment; above 4 W not permitted |
| South Korea | 917-923.5 | 200 mW EIRP | FHSS or LBT. A 2026 chart shows 4 W EIRP [16]; whether that is licence-exempt was not confirmed |
| Australia | 918-926 / 920-926 | 1 W EIRP / 4 W EIRP | |
| Hong Kong | 865-868 / 920-925 | 2 W ERP / 4 W EIRP | |

Row alignment in the GS1 PDF is poor, so Indonesia, the Philippines, Taiwan and New Zealand are left out rather than guessed. Readers are region-locked hardware, so check that the model's band matches too: the R700 family has separate global, ETSI and Japan models [11].

## India and Singapore UHF RFID limits from national texts
Checked 2026-11-02. These national texts replace the older GS1 figures for both countries.

| Country | Band (MHz) | Limit | Conditions |
|---|---|---|---|
| India | 865-868 | 2 W ERP | Channels at 865.7, 866.3, 866.9 and 867.5 MHz; at most 4 s continuous, then 100 ms off [14] |
| Singapore | 917-925 | 500 mW ERP | Licence-exempt; up to 2 W ERP in 920-925 MHz needs IMDA approval on an exceptional basis [15] |

India's G.S.R. 853(E) of 2021 supersedes the 2005 rules, which allowed 4 W ERP in 865-867 MHz; equipment approved under those rules is grandfathered [14]. Sense Sim's `rtls_list_regions` lists these and other countries, each with its source and how well it is verified.

## Converting a regional limit to reader power setting
To set a reader legally, work backwards from the limit: conducted power at reader = limit in dBm EIRP - antenna gain in dBi + cable loss in dB. For an ERP limit, first add 2.15 dB [8].

| Limit | EIRP (dBm) | At 6 dBi, 1 dB cable | At 9 dBi, 1 dB cable |
|---|---|---|---|
| Vietnam 500 mW ERP | 29.15 | 24.15 dBm | 21.15 dBm |
| ETSI 2 W ERP | 35.15 | 30.15 dBm | 27.15 dBm |
| FCC 4 W EIRP | 36 | 31 dBm | see note |
| Japan 0.5 W EIRP | 27 | 22 dBm | 19 dBm |

FCC note: above 6 dBi the rule reduces conducted power at the radiator 1 dB per dB of excess gain, so 9 dBi gets 27 dBm [3]. Antennas quoted in dBic read about 3 dB above their linear dBi rating for good circular polarisation; Times-7 equates 10 dBic with 7 dBi, for instance [12]. How each regulator measures a circular antenna was not confirmed, so use the higher figure when unsure.

## Applying this in Sense Sim
Call `rtls_set_region` with the site's country before planning, and `rtls_list_regions` to see the countries. It caps RFID readers to the EIRP limit and moves them into the band for profiles, simulation, coverage and auto deploy. Without a region the built-in profiles are not limited. Readers using a saved FDTD pattern are never capped. Uncapped EIRP of the built-in profiles:

| Profile | EIRP (dBm) | ERP (dBm) | Over Vietnam's 27 dBm ERP |
|---|---|---|---|
| `rfid-reader` | 39 | 36.85 | 9.85 dB |
| `rfid-portal` | 42 | 39.85 | 12.85 dB |
| `rfid-desktop` | 33 | 30.85 | 3.85 dB |

`freq_mhz` of 920.5 sits inside Vietnam's band and only affects path loss. Before quoting a range or coverage figure, tell the user which region it assumes. For example, Vietnam with 9 dBi and no cable loss gives `tx_power_dbm` 20.15. Re-run `rtls_coverage` with `tech` "rfid" after changing region.

## Sources
1. Overview of UHF frequency allocations (860 to 960 MHz) for RAIN RFID, 10 Feb 2022 — GS1, 2022 — https://assets.ctfassets.net/eqyxjqxiewqq/5jPzOfU16c46fYnrB2wqnE/b4bf88e0a1d361ca706ba0498516d416/uhf_regulations.pdf
2. ETSI EN 302 208 V3.3.1, RFID equipment 865-868 MHz up to 2 W and 915-921 MHz up to 4 W — ETSI, 2020 — https://www.etsi.org/deliver/etsi_en/302200_302299/302208/03.03.01_60/en_302208v030301p.pdf
3. 47 CFR 15.247 — US Code of Federal Regulations, Cornell LII — https://www.law.cornell.edu/cfr/text/47/15.247
4. China: Notice of provisions on RFID equipment (MIIT 2024 No. 76) — IB-Lenhardt, 2024 — https://ib-lenhardt.com/news/china-notice-of-provisions-on-rfid-equipment
5. Overview of ARIB Standards, STD-T106 — ARIB — https://arib.or.jp/english/std_tr/telecommunications/desc/std-t106.html
6. ARIB STD-T107 version 1.1, 920 MHz band RFID for specified low-power stations — ARIB, 2017 — https://www.arib.or.jp/english/html/overview/doc/5-STD-T107v1_1-E1.pdf
7. Thông tư 08/2021/TT-BTTTT, text reproduced by FireSmart — Vietnam MIC, 2021 — https://firesmart.vn/tin-tuc-thong-tu-so-08-2021-tt-btttt/
8. Effective radiated power — Wikipedia — https://en.wikipedia.org/wiki/Effective_radiated_power
9. FX Series RFID Fixed Reader Integration Guide — Zebra Technologies, 2024 — https://www.zebra.com/content/dam/zebra_new_ia/en-us/manuals/rfid/fxseries-ig-en.pdf
10. Thông tư 01/2025/TT-BKHCN — Vietnam MOST, 2025, via Hệ thống Pháp luật — https://hethongphapluat.com/thong-tu-01-2025-tt-bkhcn-573e457e1a9cf4e7914df3fb97840619.html
11. Impinj R700 Series RAIN RFID Readers Datasheet v2.0 — Impinj, 2023 — https://www.cisper.com/datasheets/impinj/Impinj_R700_Series_RAIN_RFID_Readers_Datasheet_2023.pdf
12. Times-7 Portal RAIN UHF RFID system — WIoT Group product page — https://wiot-group.com/think/en/products/times-7-portal-rain-uhf-rfid-system/
13. China 900 MHz RFID technical annex — third-party copy — https://file.smejs.cn/file/group1/M00/00/05/rBIAAWY4mWGAItwCAAJsZMHqdFs998.pdf
14. G.S.R. 853(E), Use of Low Power Equipment in 865-868 MHz for Short Range Devices (Exemption from Licence) Rules, 2021 — gazette copy — https://thc.nic.in/Central%20Governmental%20Rules/use%20of%20low%20power%20Equipment%20in%20the%20frequency%20band%20865%20to%20868%20MHz%20for%20Short%20Range%20Devices%20Exemption%20from%20Licence%20Rules,2021.pdf
15. IMDA TS SRD Issue 1 Rev 3, Tables 1 and 2 — IMDA Singapore, 2023 — https://www.imda.gov.sg/-/media/imda/files/regulation-licensing-and-consultations/ict-standards/telecommunication-standards/radio-comms/imdatssrd.pdf
16. RAIN RFID Spectrum Allocation chart — RAIN Alliance, April 2026 — https://therainalliance.org/wp-content/uploads/2026/04/RAIN-RFID-Spectrum-Allocation-042826-FINAL-4.pdf
17. Guidelines for RFID Usage in Thailand — Activio, 2025 — https://www2.activio.asia/2025/06/20/guidelines-for-rfid-usage-in-thailand/
