---
title: Coax loss, connectors, attenuators and power for reader antennas
domain: rf
tags: coax, coaxial cable, cable loss, attenuation per metre, RG-58, RG-174, LMR-195, LMR-400, 915 MHz, 2.4 GHz, 6.5 GHz, SMA, RP-SMA, U.FL, IPEX, MHF, N connector, RP-TNC, connector loss, mating cycles, torque, attenuator, dBm to watts, power conversion, PoE, PoE+, 802.3af, 802.3at, 802.3bt, bend radius, reader antenna installation, pigtail
aliases: cáp đồng trục, suy hao cáp, suy hao trên mét, đầu nối SMA, đầu nối N, đầu nối U.FL, bộ suy hao, đổi dBm sang watt, cấp nguồn qua Ethernet, nguồn PoE, lắp đặt anten đầu đọc, bán kính uốn cáp, how much loss in this cable, cable for rfid antenna, can I use RG-58 at 6 GHz
verified: 2026-11-02
---
# Coax loss, connectors, attenuators and power for reader antennas

## Coax loss per metre at 915 MHz, 2.4 GHz and 6.5 GHz

Loss in dB per metre, typical at 25 C with a matched load:

| Cable | Diameter | 915 MHz | 2.45 GHz | 6.5 GHz |
|---|---|---|---|---|
| LMR-400 | 10.3 mm | 0.13 | 0.22 | 0.38 |
| LMR-195 | 4.95 mm | 0.37 | 0.62 | 1.04 |
| RG-58 (Belden 8259) | 4.9 mm | 0.70 | about 1.3 | about 2.1 (extrapolated) |
| RG-174 (Belden 7805) | 2.8 mm | 0.73 | 1.24 | 2.1 (extrapolated) |

How each row was obtained:
- LMR: Times formula, dB per 100 ft = k1 sqrt(F_MHz) + k2 F_MHz, with k1 = 0.12229, k2 = 0.00026 for LMR-400 and k1 = 0.35686, k2 = 0.00047 for LMR-195 [1]. Matches sheet values of 12.8 dB per 100 m at 900 MHz for LMR-400 [2] and 36.5 and 62.4 dB per 100 m at 900 and 2500 MHz for LMR-195 [3].
- RG-58: 21.1 and 22.8 dB per 100 ft at 900 and 1000 MHz, Belden [4]. Its 2.45 GHz value comes from a comparison chart of 38.9 dB per 100 ft at 2400 MHz [5]. Values at 6.5 GHz are scaled by sqrt(f) and unverified.
- RG-174: Belden 7805 to 6 GHz, 62 dB per 100 ft [6]; 6.5 GHz is extrapolated (fit computed). Other RG-174 cables are worse: the same chart lists 30 dB per 100 ft at 1 GHz [5].

## Total cable loss for common runs

Loss in dB, computed from the per-metre table. Connectors are extra.

| Run | LMR-400 | LMR-195 | RG-58 | RG-174 |
|---|---|---|---|---|
| 1 m at 915 MHz | 0.13 | 0.37 | 0.70 | 0.73 |
| 3 m at 915 MHz | 0.39 | 1.1 | 2.1 | 2.2 |
| 6 m at 915 MHz | 0.77 | 2.2 | 4.2 | 4.4 |
| 10 m at 915 MHz | 1.3 | 3.7 | 7.0 | 7.3 |
| 3 m at 2.45 GHz | 0.66 | 1.9 | 3.9 | 3.7 |
| 3 m at 6.5 GHz | 1.1 | 3.1 | 6.3 | 6.4 |

A monostatic RFID reader pays it twice. Forward power falls by the loss, and backscatter falls by it again, so 2 dB of cable is 4 dB of round trip. In a forward-limited link only the forward loss sets tag reach, while on the receive side it also adds directly to noise figure.

At 6.5 GHz UWB, coax is rarely used: 3 m of LMR-195 costs 3 dB, and it adds delay that needs calibrating. Put the radio at the antenna and run Ethernet instead. Temperature and age raise loss, so round up. The LMR-400 sheet states its maximum at 8 GHz [2]. RG-58 and RG-174 sheets for the types above stop at 1 GHz and 6 GHz respectively [4][6], so RG-58 at 6.5 GHz is outside published data.

## Coax power rating and bend limits

Average power falls as frequency rises. LMR-400 has 580 W CW at 900 MHz, 330 W at 2.5 GHz and 210 W at 6 GHz, typical [2]. LMR-195 has 160 W at 900 MHz and 90 W at 2.5 GHz [3]. These all sit far above a 1 W RFID reader, so power handling does not choose the cable; loss and connectors do.

Peak voltage rises with power and mismatch. In 50 ohm, 1 W is 10 V peak and 2 W (33 dBm) is 14.1 V peak, computed. A mismatch raises the standing-wave peak by (1 + |Gamma|).

| Cable | Minimum bend, installation | Minimum bend, repeated | Velocity | Delay |
|---|---|---|---|---|
| LMR-400 | 25.4 mm | 101.6 mm | 85% | 3.94 ns/m [2] |
| LMR-195 | 12.7 mm | 50.8 mm | 80% [1] | 4.17 ns/m (computed) [3] |

Sharp bends and crushed cable change the local impedance. Times states under 1 ohm impedance change at its specified bend radius [1]. Secure cable within limits, and never kink it at the back of a connector. Foam cables crush more easily than solid ones, so use a short, flexible jumper where a tight bend is unavoidable (rule of thumb).

## SMA, U.FL and N connector limits

| Connector | Frequency | Mating cycles | Torque | Notes |
|---|---|---|---|---|
| U.FL | 0-6 GHz, typical | About 30 [7] | Push-on | Small pigtails of 0.81-1.37 mm cable [7] |
| SMA, CuBe or stainless | DC-18 GHz [8] | At least 500 [8] | 0.8-1.1 N m [8] | Return loss 30 dB typical, straight cable type [8] |
| SMA, brass | DC-18 GHz [8] | At least 100 [8] | 0.3-0.6 N m [9] | Cheap; easy to overtighten |
| N, standard | To 11 GHz [10] | Not verified | About 1.7 N m typical [10] | Precision versions to 18 GHz [10] |

Power is limited by heating at the centre contact. An SMA example is roughly 500 W at 1 GHz, falling under 200 W at 10 GHz [9]. A new N connector into a good load might carry 500 W at 2 GHz [10]. Treat both as vendor-dependent.

Practice notes (rule of thumb):
- U.FL is an assembly connector, not a service connector. Pull straight by the proper tool. Its 30-cycle life means no repeated test plugging.
- N is often hand-tightened, but check the maker's torque figure [10]. Use a torque wrench for SMA where repeatability matters.
- A connector pair typically costs about 0.1-0.5 dB at these frequencies. Measure rather than assume.
- RP-SMA and SMA look alike but do not mate properly. The Impinj R700 uses RP-TNC antenna ports [11].

## Attenuators and pads

A fixed pad provides a known loss and improves match. Common uses are protecting a receiver during a conducted test, and setting power on a test bench. It improves apparent source return loss by twice its attenuation, as a reflection crosses it twice (from the line-loss rule).

Its dissipated power: Pdiss = Pin (1 - 10^(-A/10)). At 33 dBm (2 W) input, a 10 dB pad dissipates 1.8 W and a 30 dB pad dissipates about 2.0 W (computed). So rate an attenuator by input power, and check thermal derating for the temperature in the enclosure. For high-power tests, place a high-power pad first, then a cheaper low-power one.

Conducted test example: reader 30 dBm into a pad chain of 40 dB gives -10 dBm at the analyzer (computed). Always check the analyzer's maximum input before connecting. Do not guess a value from the label: measure pads, since a pad's value changes with frequency, and calibrate through it at the frequency of interest.

Never replace an antenna with a short cable to a low-power instrument. Use an attenuator or dummy load on any transmitting port.

## Power conversion: dBm and watts

P(mW) = 10^(dBm/10), and dBm = 10 log10(P in mW). Computed:

| dBm | Power | Typical use |
|---|---|---|
| -14.3 | 37 uW | UWB at the -41.3 dBm/MHz limit, 499.2 MHz |
| 0 | 1 mW | BLE tag, default |
| 10 | 10 mW | BLE upper power |
| 20 | 100 mW | WiFi conducted |
| 27 | 0.5 W | Low-power reader setting |
| 30 | 1 W | RFID reader conducted |
| 33 | 2 W | Impinj R700 maximum on PoE+ [11] |
| 36 | 4 W | RFID 4 W EIRP |
| 39 | 8 W | Sense Sim `rfid-reader` EIRP |
| 42 | 16 W | Sense Sim `rfid-portal` EIRP |

EIRP = conducted power + antenna gain in dBi - cable and connector loss. So cable loss lets you raise conducted power within a legal EIRP limit, or lowers EIRP if you fail to account for it. Never add dBm to dBm. To add powers, convert to milliwatts. Gain and loss in dB add directly to dBm. The regulation documents give current limits, and region-specific rules apply.

## PoE power for readers and gateways

PoE classes per IEEE 802.3, delivered power at the powered device [12]:

| Standard | Type | PSE output | At device |
|---|---|---|---|
| 802.3af | 1 | 15.4 W | 13 W |
| 802.3at | 2 | 30 W | 25.5 W |
| 802.3bt | 3 | 60 W | 51 W |
| 802.3bt | 4 | 90 W | 71.3 W |

The gap between PSE and device figures is mainly power lost in the cable. Above class 4, power requires all four pairs [12].

Radio impact is direct. The Impinj R700 reaches 33 dBm on PoE+ but only 30 dBm on 802.3af PoE, and 31.5 dBm on PoE+ in the ETSI lower band [11]. A site with old 802.3af switches silently loses 3 dB, which is about 29% less free-space range (computed, 10^(-3/20)). So check switch class and total budget: a 24-port switch rarely supplies full class 4 on every port at once (rule of thumb). Reader power negotiation can use LLDP [11]; if it fails, the reader may fall back to lower power.

## Installation notes for reader antennas

These add to the RFID installation document, which covers cable limits, port power and VSWR.

Planning (rule of thumb):
- Keep reader near the antennas. Choose the thickest cable that fits. LMR-400 for runs over about 3 m, LMR-195 for short jumpers.
- Use equal cable types per reader, or set per-port loss values, or EIRP differs by port.
- Keep total loss in the budget: reader connector, jumper, surge arrestor if any, antenna connector.

Mechanical (rule of thumb):
- Add a drip loop outdoors, and seal outdoor joints with weatherproof tape over connectors. Use a UV-rated jacket. LMR-400 is black PE; the Times notes state UV protected PE [1].
- Use strain relief close to connectors. Ground outdoor shields, and use a surge arrestor at building entry.
- Never power a reader without a load on an active port.

Verification: measure return loss through the whole installed cable, knowing it reads better than the antenna's true value. Compare a port's RSSI with its neighbour's for fault finding. Label both ends.

## Applying this in Sense Sim

rtls_coverage has no cable field, as device profiles take tx power and antenna gain as a single budget. Add cable and connector losses by hand: EIRP = tx_power_dbm + antenna_gain_dbi - loss, using values from rtls_list_profiles, and for a reader also subtract the same loss from the receive path. Report a separate number with cable included, rather than editing the device data without asking.

Cap or check region limits with rtls_set_region. For example the `rfid-reader` profile's 39 dBm EIRP assumes no loss. With 2 dB of cable, EIRP would be 37 dBm, still above 36 dBm for the US (computed).

For conversions, use antenna_calc: "power" with dbm or watts, "erp_eirp" with erp_dbm or eirp_dbm. In the antenna studio, antenna_results reports the antenna port only; add feed cable by hand. For cable budgets at 915 MHz, assume LMR-400 unless the user gives a part, and state what you assumed.

## Sources

1. Communications Coax Selection Guide — Times Microwave Systems, accessed 2026 — https://www.rfparts.com/old_site/pdf_docs/LMR/Coax_Selection_Guide.pdf
2. LMR-400-BULK data sheet — Fairview Microwave, 2023 — https://fairviewmicrowave.com/images/productPDF/LMR-400-BULK.pdf
3. LMR-195 flexible low loss cable data sheet — Times Microwave Systems, accessed 2026 — https://www.rfparts.com/old_site/pdf_docs/LMR/LMR-195.pdf
4. Belden 8259 RG-58 data sheet — Belden, via RS, accessed 2026 — https://docs.rs-online.com/e040/A700000013640140.pdf
5. Coax cable performance chart — Universal Radio, accessed 2026 — https://www.universal-radio.com/catalog/cable/coaxperf.html
6. Belden 7805, RF 100 Wireless Coax, RG174 — Belden, accessed 2026 — https://media.onlinecomponents.com/productfiles/mf-bl1/7805_techdata.pdf
7. Hirose U.FL — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Hirose_U.FL
8. SMA connector series — Rosenberger, accessed 2026 — https://rosenberger.com/product/sma
9. SMA RF connector — Electronics Notes, accessed 2026 — https://www.electronics-notes.com/articles/electronic_components/rf-connectors/sma-connector.php
10. N-type connector — Electronics Notes, accessed 2026 — https://www.electronics-notes.com/articles/electronic_components/rf-connectors/n-type-connector.php
11. Impinj R700 RAIN RFID Reader datasheet — Impinj, 2020 — https://www.cisper.com/datasheets/impinj/Impinj_R700_RAIN_RFID_Reader_Datasheet.pdf
12. Overview of 802.3bt, Power over Ethernet standard — Ethernet Alliance, 2018 — https://ethernetalliance.org/wp-content/uploads/2018/04/WP_EA_Overview8023bt_FINAL.pdf
