---
title: Reconfigurable antennas: switches, varactors, states and how to design them here
domain: antenna
tags: reconfigurable antenna, frequency reconfigurable, pattern reconfigurable, polarisation reconfigurable, PIN diode, varactor, RF switch, RF MEMS, tunable antenna, aperture tuning, switched parasitic, bias, DC block, RF choke, on state, off state, truth table
aliases: anten tái cấu hình, anten điều hưởng, đi-ốt PIN, varactor, chuyển mạch RF, phân cực cuộn, mảng ký sinh
verified: 2026-11-02
---
# Reconfigurable antennas: switches, varactors, states and how to design them here

## What the parts do and what they cost

PIN diodes and varactors are the most common tunable elements in reconfigurable antennas; a PIN diode acts as an RF switch whose on and off states work like a short and an open, and MEMS switches offer better insertion loss, isolation and power consumption than PIN diodes and FETs [1]. A varactor's capacitance follows its reverse bias, which gives a wide tuning range, at the price of loss in its series resistance and limits on power handling [2].

Neither state is ideal. An off PIN diode is a small capacitor, not an open: at 2.45 GHz, 0.23 pF is about -j282 ohm, against a wire of similar impedance. In this app's switched-length dipole, 0.23 pF diodes left the short state resonating near 1.8 GHz instead of 2.45 GHz, because the outer sections kept working through the off diodes; a 0.03 pF flip chip brought it to 2.3 GHz, and a small trim of the length to 2.45 GHz. So check off capacitance first at high frequency. On-state resistance costs more when more current flows through the part, so a part at a current maximum loses more than one near a current minimum.

Varactor capacitance is usually published as a SPICE form, C(V) = Cj0 / (1 + V/Vj)^M + Cp. For the SMV1405: Cj0 2.37 pF, Vj 0.77 V, M 0.5, Cp 0.29 pF, Rs 0.8 ohm [3]. Low bias means the most capacitance, the most current in Rs and the most loss, and the least RF swing before it conducts.

## Typical parts

| Part | On or series R | Off or tuning C | Series L | Limit | Source |
|---|---|---|---|---|---|
| BAR64-02V PIN | 2.1 ohm at 10 mA | 0.23 pF | 0.6 nH | 150 V, 100 mA | [4] |
| MADP-000907-14020P flip-chip PIN | 5.2 ohm | 0.030 pF | small | 45 V | [5] |
| SMV1405 varactor | 0.8 ohm | 0.66 to 2.66 pF over 0 to 30 V, from its SPICE form | package dependent | check the datasheet | [3] |

These are typical figures at the makers' test conditions. Use your own datasheet's values at your bias and frequency.

## Designing one in Sense Sim

Use antenna_reconfig_parts to see the library, antenna_new with a reconfig_ template (reconfig_dipole for frequency, reconfig_pifa for varactor tuning, reconfig_parasitic for the beam, reconfig_pol_patch for polarisation) or antenna_reconfig_set to put parts across ports of the current model, with states and targets. antenna_reconfig_solve solves every port once, and antenna_reconfig_results then has each state's match, band, gain, beam, polarisation, loss in the parts and RF stress. antenna_reconfig_search finds the best settings per target with no new solve, and antenna_reconfig_found applies them. antenna_reconfig_tuning gives a varactor's curve, antenna_reconfig_spread the chance a build still meets a target when parts differ, and antenna_reconfig_view puts one state on the result pages.

Only moved metal needs a new solve: each gap is a port, and closing a port with a part's impedance is algebra on the solved S-matrix and per-port far fields. Parts are linear in that algebra, so harmonics, intermodulation and an RF swing moving a varactor's capacitance are not included. Bias lines are in the model only if drawn as parts; draw them once the layout is known, and solve again.

## Sources

1. A Survey on Reconfigurable and Movable Antennas for Wireless Communications and Sensing — arXiv, 2026 — https://arxiv.org/pdf/2602.17977
2. Recent Developments of Reconfigurable Antennas (Ojaroudi Parchin et al.) — 2019 — https://vyunov.pp.ua/MW/energy_harvesting/2019-Ojaroudi%20Parchi-Recent%20Developments%20of%20Re.pdf
3. SMV1405 silicon abrupt junction varactor — Skyworks, datasheet with SPICE parameters — https://html.alldatasheet.com/html-pdf/852632/SKYWORKS/SMV1405/1164/3/SMV1405.html
4. BAR64-02V RF PIN diode — Infineon, product page — https://www.infineon.com/cms/en/product/rf/rf-diode/rf-pin-diode/antenna-switch/bar64-02v/
5. MADP-000907-14020P flip-chip AlGaAs PIN diode — MACOM, product page — https://macom.com/products/product-detail/MADP-000907-14020P
