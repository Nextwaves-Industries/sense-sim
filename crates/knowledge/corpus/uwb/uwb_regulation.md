---
title: UWB spectrum regulation (FCC, EU/ETSI, Vietnam, APAC)
domain: uwb
tags: uwb regulation, power spectral density, psd, maximum transmit power, fcc part 15 subpart f, 15.517, 15.519, -41.3 dbm/mhz, 0 dbm/50 mhz, eirp, etsi en 302 065, eu 2019/785, 2024/1467, ecc, ldc, daa, tpc, vietnam, circular 08/2021/tt-btttt, qcvn, japan, korea, china miit, channel 5 legality, channel 9 legality, outdoor uwb, type approval
aliases: quy định UWB, giới hạn công suất UWB, miễn giấy phép tần số, Thông tư 08/2021/TT-BTTTT, Thông tư 01/2025/TT-BKHCN, Phụ lục 13, thiết bị siêu rộng băng, băng siêu rộng, kênh 5 có hợp pháp không, lắp cố định ngoài trời, Cục Tần số vô tuyến điện, UWB legal limit, is UWB allowed
verified: 2026-11-02
---
# UWB spectrum regulation

All limits here were read on 2026-11-02 from the cited texts. This is engineering guidance, not legal advice. Confirm with the national regulator or a test laboratory before a product decision.

## Global UWB power limit: what -41.3 dBm/MHz means
The value -41.3 dBm/MHz mean EIRP, with a 0 dBm peak in 50 MHz, is the common limit in the US, EU, and the UWB bands of Japan and Vietnam [1][4][5][9]. Over a 499.2 MHz channel the mean limit is -14.3 dBm total (computed: -41.3 + 10 log10(499.2)).

| Quantity | Value |
|---|---|
| Mean EIRP density | -41.3 dBm/MHz |
| Mean EIRP over a 499.2 MHz channel | -14.3 dBm |
| Peak EIRP | 0 dBm in 50 MHz |
| Free-space loss at 40 m, channel 5 | 80.7 dB (computed) |

EIRP includes antenna gain, so an antenna with 3 dBi peak gain requires 3 dB less conducted power at the limit: extra gain does not buy range toward the boresight at full power. The DW3000 can be programmed up to about -31 dBm/MHz on the DW3110, more than these limits allow, so compliance depends on configuration and calibration, not on the chip [10]. The DW3000 is not itself certified for any region; each product must be approved [10].

## United States: FCC Part 15 Subpart F limits for UWB (checked 2026-11-02)
Two FCC rule sections matter for positioning systems. Both allow -41.3 dBm/MHz average EIRP in 3100 to 10600 MHz, measured in 1 MHz, and 0 dBm peak in 50 MHz [1][2].

| Topic | Section 15.517 indoor | Section 15.519 handheld |
|---|---|---|
| Use | Indoors only; fixed indoor infrastructure allowed | Indoors or outdoors, handheld only |
| Infrastructure | Outdoor antennas and outdoor infrastructure prohibited | Must not depend on fixed infrastructure; antennas only on the device |
| Transmit condition | Only when sending to an associated receiver | Stops within 10 s without an acknowledgement |
| Label | "May only be operated indoors" statement required | Not stated in this section |
| Emissions 1610 to 3100 MHz | -53.3 and -51.3 dBm | -63.3 and -61.3 dBm |

Operation on aircraft, ships and satellites and use in toys are prohibited [3].

Guidance: an anchor-based RTLS falls under 15.517, so anchors cannot be mounted outdoors in the US under these sections. A tag that leaves the building is a compliance question to raise early.

## European Union: Decision 2019/785 as amended by 2024/1467 (checked 2026-11-02)
The EU's harmonised conditions are in Implementing Decision (EU) 2019/785, amended in 2024. Conformity is usually shown with the ETSI EN 302 065 series [4][11]. Values for the main bands:

| Band | Generic UWB mean / peak | Condition |
|---|---|---|
| 3.1 to 4.8 GHz | -70 to -80 dBm/MHz; or -41.3 / 0 dBm | Higher level only with LDC or DAA |
| 6.0 to 8.5 GHz | -41.3 dBm/MHz / 0 dBm in 50 MHz | No mitigation listed |
| 8.5 to 9.0 GHz | -65 dBm/MHz; or -41.3 / 0 dBm | Higher level only with DAA |

Generic use excludes fixed outdoor installations, aviation, and road and rail vehicles. Both channel 5 and channel 9 sit inside 6 to 8.5 GHz with no mitigation requirement.

The 2024 amendment adds two relevant categories in 6 to 8.5 GHz [4]:
- Enhanced indoor devices: -31.3 dBm/MHz and +10 dBm in 50 MHz, indoor only, at most 5% duty cycle per second.
- Fixed outdoor location tracking: -41.3 dBm/MHz, at most 5% duty per second, antenna no higher than 10 m. Above 2.5 m, antennas must be directive and down-tilted, with total radiated power density at most -46.3 dBm/MHz.

## Vietnam: UWB bands in Circular 08/2021/TT-BTTTT (checked 2026-11-02)
Vietnam lists licence-exempt UWB devices in Circular 08/2021/TT-BTTTT, Appendix 13, in force since 28 November 2021. It defines UWB as a -10 dB bandwidth of at least 500 MHz or a fractional bandwidth of at least 0.2 [5][6].

| Band (MHz) | Mean / peak | Conditions |
|---|---|---|
| 4200 to 4800 | -70 dBm/MHz, -30 dBm/50 MHz | Indoors or shielded only; not on aircraft |
| 4200 to 4800 | -41.3 dBm/MHz, 0 dBm/50 MHz | Needs LBT, duty-cycle limit or DAA |
| 7238.4 to 8500 | -41.3 dBm/MHz, 0 dBm/50 MHz | No extra mitigation |
| 8500 to 9000 | -65 dBm/MHz, -25 dBm/50 MHz | No extra mitigation |
| 7238.4 to 9000 | -41.3 dBm/MHz, 0 dBm/50 MHz | Needs LBT, duty-cycle limit or DAA |

All rows also forbid fixed outdoor installation or connection to a fixed outdoor antenna. The drafting note says the 7.25 to 9 GHz band was added for IoT, asset management and industrial tools, after field interference tests [7]. Circular 01/2025/TT-BKHCN, effective 15 May 2025, amends Appendices 2, 10 and 16, not Appendix 13, according to a legal summary [8].

## Vietnam: channel 5 is outside the licence-exempt UWB list
Consequence for Vietnam, on the text of Circular 08/2021/TT-BTTTT Appendix 13 [5][6]:

| Channel | Occupied band | Status under Appendix 13 |
|---|---|---|
| 5 | 6240.0 to 6739.2 MHz | Not listed: no row between 4800 and 7238.4 MHz |
| 9 | 7737.6 to 8236.8 MHz | Listed at -41.3 dBm/MHz and 0 dBm/50 MHz |

Design for channel 9 for a licence-exempt system in Vietnam. Hardware that only does channel 5, including every DW1000-based product, does not fit the exemption. A 2019 forum post said channel 5 was allowed under the older Circular 46/2016 [12]; that circular was replaced in 2021, and the post should not be relied on. Anchors outdoors at fixed locations are also outside the exemption, whatever the channel.

Not verified: whether a channel 5 system could be individually licensed, and whether any amendment after the 01/2025 circular touched Appendix 13. Ask the Authority of Radio Frequency Management (Cục Tần số vô tuyến điện) for a written position on a real project.

## Japan, Korea and China UWB limits (checked 2026-11-02)
| Region | UWB bands | Mean limit | Notes |
|---|---|---|---|
| Japan | 3.4 to 4.8, 7.25 to 10.25 GHz | -41.3 dBm/MHz, 0 dBm/50 MHz | 3.4 to 4.8 GHz needs interference mitigation; outdoor use possible in 7.587 to 8.4 GHz [9] |
| Korea | 3.735 to 4.8, 6.0 to 10.2 GHz in [9]; about 6.0 to 8.8 GHz in [11] | -41.3 dBm/MHz [9] | Not confirmed: [11] cites a newer notice (2022-75) that makes DAA or strict limits mandatory; read the current RRA notice |
| China, since 1 Aug 2025 | 7163 to 8812 MHz | Not verified; in a Chinese-language annex | Banned within 1 km of radio astronomy sites and on aircraft; old approvals may stay in use [13] |

Channel 9 fits Japan and China. Channel 5 falls outside Japan's table and outside China's new band, so a new China type approval must use channel 9 [9][13]. Whether either channel works in Korea without mitigation is not confirmed. Source [11] also misstates the FCC peak limit, so treat its numbers with caution.

Other countries: `rtls_list_regions` lists the rules confirmed so far, each marked verified, secondary source or not verified. Do not assume limits for a country marked not verified or not listed. Check the national regulator.

## Wi-Fi coexistence near UWB channel 5
Circular 01/2025/TT-BKHCN opens 5925 to 6425 MHz to WLAN/RLAN devices in Vietnam [8]. The upper part of that band overlaps UWB channel 5, which occupies 6240.0 to 6739.2 MHz (computed). Channel 9, at 7737.6 to 8236.8 MHz, avoids it entirely. The DW3000 datasheet gives in-band blocking of -80 dBm at the chip input for 1% PER with 3 dB desense [10]. No field measurement of UWB under Wi-Fi 6E load was verified for this knowledge base.

Guidance: where site surveys show 6 GHz Wi-Fi access points, prefer channel 9 even if channel 5 is legal.

## Applying this in Sense Sim
- `rtls_auto_deploy` takes `anchor_tx_dbm_mhz`. The simulator caps it at -41.3 dBm/MHz, so it cannot represent the EU's -31.3 dBm/MHz enhanced indoor category. Use lower values to plan low-power or conservatively calibrated systems: -53.3 dBm/MHz cuts link range to a quarter.
- `uwb-anchor` defaults to channel 5 (6489.6 MHz). Call `rtls_set_region` with the site's country before planning: for Vietnam, Japan or China it moves UWB to channel 9, which shortens range because free-space loss is higher. Hardware that only supports channel 5 still does not comply, so check the real product.
- Placement tools do not check outdoor mounting. If the user places anchors outdoors with `rtls_add_device`, or asks for coverage across a yard, warn about fixed-outdoor restrictions in the applicable region.
- Antenna gain counts toward EIRP in reality, but the link model adds gain on top of `uwb_tx_dbm_mhz`. Lower `anchor_tx_dbm_mhz` by the antenna's peak gain to keep a study legally consistent.

## Sources
1. 47 CFR 15.517, Technical requirements for indoor UWB systems — Cornell LII copy of the eCFR, checked 2026-11-02 — https://www.law.cornell.edu/cfr/text/47/15.517
2. 47 CFR 15.519, Technical requirements for hand held UWB systems — Cornell LII copy of the eCFR, checked 2026-11-02 — https://www.law.cornell.edu/cfr/text/47/15.519
3. 47 CFR 15.521, Technical requirements applicable to all UWB devices — Cornell LII copy of the eCFR, checked 2026-11-02 — https://www.law.cornell.edu/cfr/text/47/15.521
4. Commission Implementing Decision (EU) 2024/1467 amending Decision (EU) 2019/785, Annex — copy at LawPlayer, official text at EUR-Lex prevails, checked 2026-11-02 — https://lawplayer.com/eu/act/32024D1467 (official: https://eur-lex.europa.eu/eli/dec_impl/2024/1467/oj)
5. Thông tư 08/2021/TT-BTTTT, Phụ lục 13 — Ministry of Information and Communications of Vietnam, 2021, copy at CaseLaw — https://caselaw.vn/van-ban-phap-luat/401574-thong-tu-so-08-2021-tt-btttt-ngay-14-10-2021-cua-bo-truong-bo-thong-tin-va-truyen-thong-quy-dinh-ve-danh-muc-thiet-bi-vo-tuyen-dien-duoc-mien-giay-phep-su-dung-tan-so-vo-tuyen-dien-dieu-kien-ky-thuat-va-khai-thac-kem-theo
6. Circular 08/2021/TT-BTTTT, second copy of Appendix 13 — Hệ thống pháp luật, 2021 — https://hethongphapluat.com/circular-no-08-2021-tt-btttt-dated-october-14-2021-on-prescribing-list-of-license-exempt-radio-devices-and-accompanying-technical-and-operational-conditions.html
7. Thuyết minh dự thảo Thông tư về Danh mục thiết bị vô tuyến điện được miễn giấy phép — drafting note hosted by VIBOnline, 2021 — https://vibonline.com.vn/wp-content/uploads/2021/04/Thuyet-minh-du-thao-Thong-tu-ve-Danh-muc-thiet-bi-vo-tuyen-dien-duoc-mien-giay-phep.pdf
8. Adds 5925-6425 MHz band for WLAN/RLAN devices and updates technical conditions (Circular 01/2025/TT-BKHCN summary) — LuatVietnam, 2025 — https://english.luatvietnam.vn/legal-updates/adds-59256425-mhz-band-for-wlan-rlan-devices-and-updates-technical-conditions-892-101700-article.html
9. UWB outdoor-use working group report outline, document 61-2-2, comparison table of national UWB rules — Ministry of Internal Affairs and Communications of Japan — https://www.soumu.go.jp/main_content/000786294.pdf
10. DW3000 Datasheet, version 1.1 — Decawave (Qorvo), 2020 — https://www.mouser.com/datasheet/2/412/DW3000_Data_Sheet-3532945.pdf
11. UWB: Technology and Compliance — IB-Lenhardt (test-lab summary, secondary), checked 2026-11-02 — https://ib-lenhardt.com/kb/ultra-wideband
12. Regulations in Indonesia (thread mentioning Vietnam) — Qorvo forum, 2019 — https://forum.qorvo.com/t/regulations-in-indonesia/5826
13. MIIT interim regulations on radio management of UWB equipment — CAICT translation, 2024 — https://gma.caict.ac.cn/en/update/128/522
