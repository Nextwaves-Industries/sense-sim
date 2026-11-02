---
title: EMC, ESD and coexistence between radios on a board
domain: rf
tags: EMC, EMI, shielding, shield can, aperture, slot, seam, ESD, TVS, IEC 61000-4-2, antenna port protection, radiated emissions, FCC Part 15, 15.209, 15.205, 15.247, restricted bands, spurious, harmonics, CISPR 32, EN 55032, RED, EN 300 328, EN 302 208, EN 301 489, desense, desensitization, platform noise, DC/DC, clock harmonics, coexistence filter, isolation, co-located radios, BLE, WiFi, UHF RFID, UWB
aliases: tương thích điện từ, can nhiễu điện từ, che chắn, vỏ chắn, phóng tĩnh điện, bảo vệ cổng anten, đo phát xạ, thử nghiệm hợp chuẩn, giảm độ nhạy, nhiễu nội bộ bo mạch, nhiễu từ nguồn xung, cùng tồn tại trên bo mạch, cách ly anten, lọc cùng tồn tại, why is my radio worse in the enclosure
verified: 2026-11-02
---
# EMC, ESD and coexistence between radios on a board

## EMC basics: source, path and victim

Every interference problem has a source, a coupling path and a victim. Typical solutions focus on suppressing the source or the path. On a radio board all three sit together: a switching regulator or clock is the source, a shared ground or an unshielded trace is the path, and the receiver input, sensitive to around -100 dBm, is the victim.

Order of fixes by cost (rule of thumb):
1. Source: lower edge rates, spread-spectrum clocks, moving clock frequencies away from channels.
2. Path: unbroken ground plane, filtering of supply and control lines entering the RF section, keep RF and digital areas apart.
3. Shield: a can over the noisy part or the radio. A can is last, since it cannot fix conducted noise, and slots limit it.
4. Victim: narrow the receive filter, which helps only with out-of-band energy.

Two different goals share this topic. Emissions testing checks what leaves the product, and desense is what the product does to its own radio. A product can pass emissions by a wide margin and still lose 10 dB of sensitivity: the regulatory limit is far above a receiver's noise floor.

## Shielding effectiveness and apertures

A closed metal shield works well at RF, and absorption in a metal layer several skin depths thick is large. In practice, holes and seams set the result.

Approximation for a single slot of longest dimension d: SE = 20 log10(lambda / (2 d)) dB, and the slot leaks almost freely at f = c / (2 d) [1]. Length matters, not width [1]. For N identical holes, subtract 10 log10(N) [1]. These come from a secondary reference with some inconsistent examples, so use them for trends only, and test the final product.

Computed from the formula:

| Slot length | SE at 2.45 GHz |
|---|---|
| 6.1 mm | 20 dB |
| 10 mm | 15.7 dB |
| 30 mm | 6.2 dB |

For 20 dB, slots must be below lambda/20: 16.4 mm at 915 MHz, 6.1 mm at 2.45 GHz and 2.3 mm at 6.5 GHz (computed). So a shield-can seam needs ground contacts at that pitch, and an unfilled board edge is a long slot. Cables through a shield are worse than any hole, since they carry noise straight through: filter them at the wall. A closed can is also a resonant cavity, so absorber may be needed (rule of thumb).

## ESD levels and test method

IEC 61000-4-2 defines system-level ESD [2]:

| Level | Contact | Air | Peak current, contact | Rise time |
|---|---|---|---|---|
| 1 | 2 kV | 2 kV | 7.5 A | 0.8 ns |
| 2 | 4 kV | 4 kV | 15 A | 0.8 ns |
| 3 | 6 kV | 8 kV | 22.5 A | 0.8 ns |
| 4 | 8 kV | 15 kV | 30 A | 0.8 ns |

Contact and air severity are not equivalent [2]. Sub-nanosecond rise time makes via and trace inductance matter a lot. L di/dt across even 1 nH with 30 A in 0.8 ns gives about 38 V (computed), on top of the clamp voltage. The ESD path must be a short, wide connection to the chassis or ground plane, not a thin trace.

These figures come from a vendor guide, not the standard itself, so check the current edition before a compliance test. An exposed antenna, SMA connector or metal enclosure seam is the most likely strike point for a tag, gateway or anchor.

## ESD protection on an antenna port

The protector shunts the RF line to ground and must be nearly invisible at RF. Example: onsemi ESD7462 is 0.30 pF typical at 1 MHz and 0.25 pF at 1 GHz, with insertion loss of 0.05 dB at 1 GHz and 0.10 dB at 3 GHz, Level 4 IEC rating and 16 V reverse working voltage [3].

Worked effect of 0.25 pF as a shunt on 50 ohm (computed):

| Frequency | Reactance | Return loss if alone |
|---|---|---|
| 915 MHz | 696 ohm | 29 dB |
| 2.45 GHz | 260 ohm | 20 dB |
| 6.5 GHz | 98 ohm | 12 dB |

So include its capacitance in the matching network, especially for UWB.

Check the working voltage against RF swing. At 30 dBm, 50 ohm peak voltage is 10 V; at 33 dBm it is 14.1 V, and with VSWR 1.5 it reaches 17 V (computed). That exceeds 16 V, so this part is unsuitable for a 33 dBm reader port, even though it suits a 10 dBm BLE tag. A diode that conducts on RF peaks also creates harmonics.

Placement (rule of thumb): right at the connector, before anything else, with a short ground via. A shunt inductor to ground at the antenna, which is a DC short, helps where the antenna allows it; a series DC-block capacitor adds protection, and can double as a match element.

## Radiated emission limits (FCC Part 15)

47 CFR 15.209 general limits at 3 m [4]:

| Frequency | Field strength | dBuV/m (computed) |
|---|---|---|
| 30-88 MHz | 100 uV/m | 40 |
| 88-216 MHz | 150 uV/m | 43.5 |
| 216-960 MHz | 200 uV/m | 46 |
| Above 960 MHz | 500 uV/m | 54 |

Below 1 GHz these use a CISPR quasi-peak detector, above 1 GHz an average detector [4], and the peak limit above 1 GHz is 20 dB over the average limit [5]. For intentional transmitters under 15.247, out-of-band power must be at least 20 dB below the in-band level in 100 kHz, or 30 dB with RMS measurement, and emissions in restricted bands must also meet 15.209 [6]. Restricted bands include 2310-2390, 2483.5-2500, 2690-2900 MHz, 3600-4400 MHz, 4.5-5.15 and 7.25-7.75 GHz [7].

Worked: 500 uV/m at 3 m is about -41.2 dBm EIRP (from E^2 d^2 / 30, computed). A US RFID reader at 36 dBm EIRP has its third harmonic at 2.7-2.78 GHz, in a restricted band, so it needs about 77 dB below carrier in radiated terms (computed). A 2.44 GHz radio's second harmonic near 4.88 GHz is also restricted.

## Regulatory test overview for a radio product

Testing has separate parts. The radio module's approval does not automatically cover a new antenna or a whole product, so check its conditions.

| Region | Topic | Reference |
|---|---|---|
| US | Unintentional emissions and intentional radiators | Part 15, as above [4][6] |
| US | RF exposure | Sections 1.1307 and 1.1310, by 15.247(i) [6] |
| EU | Spectrum use, 2.4 GHz | EN 300 328 V2.2.2 under RED Article 3(2) [8] |
| EU | UHF RFID | EN 302 208 V3.3.1 [8] |
| EU | Radio EMC | EN 301 489 series, such as -3 and -17, Article 3(1)(b) [8] |
| Global | Multimedia emissions | CISPR 32 / EN 55032 [9] |

The list above reflects a harmonised standards summary generated on 7 September 2026 [8]; versions change, so check the current Official Journal.

CISPR 32 Class B, for residential equipment, is 40 dBuV/m in 30-230 MHz and 47 in 230-1000 MHz at 3 m, or 30 and 37 at 10 m, quasi-peak [9]. Class A at 10 m is 40 and 47 [9]. Pre-scan early using a near-field probe. The UWB document covers UWB rules; the safety document covers exposure.

## Desense from on-board noise

Noise that reaches the receiver in-band adds directly to its floor. Desense = 10 log10(1 + 10^((Nx - N)/10)), where Nx is added noise and N is the receiver's own noise power, both in the channel bandwidth (computed).

| Added noise relative to floor | Sensitivity lost |
|---|---|
| +10 dB | 10.4 dB |
| 0 dB | 3.0 dB |
| -3 dB | 1.8 dB |
| -6 dB | 1.0 dB |
| -10 dB | 0.4 dB |

So keep platform noise at least 6 dB below the floor for under 1 dB loss. Floors from kTB: BLE 1 MHz with 7 dB NF is -107 dBm, and UWB 499.2 MHz with 6 dB NF is -81 dBm (computed). A narrowband spur matters only if it lands in the channel, but UWB's width catches many spurs.

Real example: the TI CC2651P3 errata reports about 2 dB of RX sensitivity loss with its integrated DC/DC, and the workaround is an internal LDO at higher current [10].

Diagnosis (rule of thumb): compare conducted sensitivity with the system off and on. Compare radiated sensitivity with displays, cameras and buses off. Turn off blocks one by one. Measure the spectrum at a loop probe near the antenna feed, and look for harmonic combs at the clock spacing.

## Coexistence between radios on one board

Between co-located radios, three effects differ in their fix: receiver blocking is filtered at the receiver, transmitter noise is filtered at the transmitter only, and 3 dB desense occurs when transmitter noise equals receiver noise at the receiver input [11].

Isolation tools, in order [12]:
- Spatial: place antennas apart, orient linear antennas orthogonally, line up their nulls, and use a via fence between radio sections.
- Frequency: adaptive hopping, needing good selectivity in-band, and filters out-of-band.
- Temporal: arbitrate so they never overlap, which is the only choice for in-band pairs such as BLE and WiFi 2.4 GHz.

Worked example (computed): a 30 dBm UHF RFID reader beside a BLE radio with 20 dB of antenna isolation gives +10 dBm at the BLE input. Against a -20 dBm blocking figure (the simulator's unsourced preset), 30 dB more is needed. The Johanson 2450BP14 filter has 30 dB minimum at 880-915 MHz, 35 typical, and 1.5 dB loss [13], so it covers part of the US RFID band, at 1.5 dB of BLE sensitivity. Check its response to 928 MHz. Also check harmonics: 2.45 GHz's third is 7.35 GHz, and a 915 MHz seventh is 6.4 GHz, which falls in UWB channel 5.

## Applying this in Sense Sim

design_coexist takes radios by preset name (BLE, WiFi 2.4 GHz, WiFi 5 GHz, WiFi 6E, UWB channel 5, UWB channel 9, UHF RFID reader, LTE-M band 13) or full radio objects, plus isolation_db (default 20) or a touchstone file with measured coupling. It returns desense, blocked, range_left, isolation_needed_db, and harmonics. Its results are estimates. It assumes harmonic levels of -60 dBc, blocking levels and receiver noise that come from no cited source, so pass real figures from the data sheets.

For the touchstone input, use a VNA measurement of the antennas on the real board; its port order must equal the radio order.

Neither rtls_coverage nor antenna_results models conducted noise, ESD capacitors, shield cans or emissions. Subtract desense from margins yourself. antenna_match does not include an ESD capacitor unless you add it to the network by hand. Never claim emissions, ESD or coexistence compliance from simulation: say which lab test remains.

## Sources

1. What is the relationship between the size of an aperture and the frequency at which it leaks — RF Essentials, accessed 2026 — https://rfessentials.com/rf-knowledge-base/what-is-the-relationship-between-the-size-of-an-aperture-and-the-frequency-at-wh/
2. SLVU702, ESD Protection Diodes EVM user's guide — Texas Instruments, accessed 2026 — https://ti.com/lit/pdf/SLVU702
3. ESD7462 ESD Protection Diode data sheet — onsemi, accessed 2026 — https://www.onsemi.com/download/data-sheet/pdf/esd7462-d.pdf
4. 47 CFR 15.209, Radiated emission limits; general requirements — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.209
5. 47 CFR 15.35, Measurement detector functions and bandwidths — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.35
6. 47 CFR 15.247 — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.247
7. 47 CFR 15.205, Restricted bands of operation — Cornell LII, checked 2026 — https://www.law.cornell.edu/cfr/text/47/15.205
8. Radio Equipment Directive 2014/53/EU, summary list of harmonised standards — IBF Solutions, generated 7 September 2026 — https://www.ibf-solutions.com/fileadmin/dateidownloads/amtsblaetter/radio-equipment-directive-2014-53-eu-summary-list-of-harmonised-standards.pdf
9. SLYY202, Fundamentals of EMI Requirements for an Isolated DC/DC Converter — R. Martinez, Texas Instruments, accessed 2026 — https://ti.com/lit/slyy202
10. CC2651P3 errata, Radio_04 — Texas Instruments, 2022 — https://ti.com/document-viewer/lit/html/SWRZ120/GUID-8FCD1A63-0CC9-4EF0-A015-769E7F199FDE
11. RF System Aspects for SDR, a tutorial — R. Leschhorn, Rohde & Schwarz, Wireless Innovation Forum, 2011 — https://winnforum.org/assets/Proceedings/2011/2011-2a%20-%20leschhorn.pdf
12. AN91445, Antenna Design and RF Layout Guidelines, coexistence section — Cypress / Infineon, Rev. H — https://www.infineon.com/dgdl/Infineon-AN91445_Antenna_Design_and_RF_Layout_Guidelines-ApplicationNotes-v09_00-EN.pdf?fileId=8ac78c8c7cdc391c017d073e054f6227
13. 2450BP14D0100001T 2.45 GHz Band Pass Filter, Rev. 3.0 — Johanson Technology, 2025 — https://www.johansontechnology.com/docs/4650/BandPassFilter-2450BP14D0100001T-Rev3.0.pdf
