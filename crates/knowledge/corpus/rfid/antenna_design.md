---
title: UHF RFID tag and reader antenna design
domain: rfid
tags: tag antenna design, inlay design, T-match, loop match, meander dipole, conjugate match, complex impedance, chip impedance, power transmission coefficient, power transfer, read range, UCODE 9, Qstar-7U, reader antenna design, circular polarisation, truncated corner patch, dual feed patch, hybrid coupler, axial ratio
aliases: thiết kế anten thẻ RFID, thiết kế inlay, phối hợp trở kháng chip, anten đầu đọc phân cực tròn, how do I design an RFID tag antenna, why is my tag antenna not matched to 50 ohm, what impedance does an RFID chip have
verified: 2026-11-02
---
# UHF RFID tag and reader antenna design

## Why a tag antenna is not matched to 50 ohm
A passive UHF chip is strongly capacitive, so the antenna must be inductive and equal the chip's complex conjugate: Z_antenna = Z_chip* [1]. Judge a tag by the power reflection |s|^2 = |(Z_antenna - Z_chip*) / (Z_antenna + Z_chip)|^2, and by the power transfer 1 - |s|^2 that it leaves [1]. S11 against 50 ohm says nothing useful about a tag. A plain 73 ohm dipole delivers under a tenth of the collected power to a 15-j231 ohm chip.

## Chip impedances, and what is not published
Use the assembled impedance when the data sheet gives one, since assembly adds parallel capacitance [1].

| Chip | Impedance at 915 MHz | Read / write | Source note |
|---|---|---|---|
| NXP UCODE 9, blade diced | 15-j231 ohm assembled, 9-j243 chip alone | -24 / -22 dBm | on a 2.15 dBi antenna [2] |
| NXP UCODE 9, plasma diced | 16-j237 ohm assembled, 10-j248 chip alone | -24 / -22 dBm | [2] |
| NXP UCODE 9, single-slit assembly | 10-j191 ohm | -24 / -22 dBm | with 195 fF of assembly capacitance [2] |
| Quanray Qstar-7U | not published | about -24 / -21 dBm | reseller listings and a launch note, and one listing says -23 [3] |

No impedance is published for the Qstar-7U, so ask Quanray or measure an assembled tag rather than assume another chip's value.

## Tag structure: a matching element and a radiator
A small loop or T-match supplies the inductance a capacitive chip needs, and a dipole around half a wavelength supplies the gain [1]. In NXP's words, the loop matches the impedance while a second antenna trace adds the gain needed for read ranges of several metres. Their example of a loop with a half-wave wire reached 4.4 m, and shortening the wire to a quarter wave dropped it below 0.5 m because the resonance moved far above the band [1]. Meanders fold the dipole into a label, at the cost of gain and bandwidth.

## Tuning a tag, in order
Set reactance first: the T-match or loop size sets the inductance. Then resistance: bringing the dipole closer to its own resonance, through its length or meanders, raises the resistance the match presents. Then centre the band, and put the reactance back. Bandwidth is limited by the chip's Q, about 15 for 15-j231 ohm. Read range follows the Friis form with the power transfer included [1], and a real product detunes the tag, so tune on the real material.

## Circular reader panels: one feed or two
A square air-spaced patch with two opposite corners cut splits its mode in two, a quarter cycle apart, giving circular polarisation from one feed. That holds over a narrow band such as 865-868 MHz. For 902-928 MHz, feed the patch on two axes in quadrature through a hybrid, which holds the axial ratio across the band. Commercial panels reach about 8.5 dBic with an axial ratio near 1 dB: see the reader antenna selection document.

## Applying this in Sense Sim
antenna_references finds the structure for a job and opens as a sample a test holds to its numbers. Give antenna_spec a chip or chip_z and antenna_assess judges power transfer instead of S11, and reports a free-space read range. These are simulated figures for metal in free space, not measurements: a real inlay sits on PET on a product.

## Sources
1. UHF RFID PCB antenna design, rev. 3.0, sections 2.3, 2.5 and 6.3 — NXP AN1715 — https://www.nxp.com/docs/en/application-note/AN171530.pdf
2. SL3S1206 UCODE 9 data sheet, rev. 3.5, table 11 — NXP — https://www.nxp.com/docs/en/data-sheet/SL3S1206.pdf
3. Dry Inlay Quanray Qstar 7U 50x30mm, a reseller listing and not a manufacturer data sheet — Nextwaves — https://nextwaves.com/products/dry-inlay-quanray-7u-50x30mm
