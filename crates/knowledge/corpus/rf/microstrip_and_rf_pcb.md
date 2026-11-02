---
title: Microstrip, coplanar waveguide and RF PCB layout
domain: rf
tags: microstrip, coplanar waveguide, CPW, CPWG, grounded coplanar, trace width, 50 ohm trace, impedance control, PCB stackup, FR-4, 370HR, Rogers, RO4003C, RO4350B, Dk, Df, loss tangent, design Dk, process Dk, effective permittivity, eeff, via fence, ground stitching, RF layout, skin depth, copper roughness, conductor loss, dielectric loss, Hammerstad, Kirschning, prepreg, reference plane
aliases: đường vi dải, microstrip, ống dẫn đồng phẳng, độ rộng đường mạch, mạch in RF, thiết kế PCB RF, xếp lớp mạch in, hàng rào via, khâu đất, độ sâu thấm, độ nhám đồng, hằng số điện môi, tổn hao điện môi, how wide is a 50 ohm trace, trace width for 50 ohm
verified: 2026-11-02
---
# Microstrip, coplanar waveguide and RF PCB layout

## Microstrip impedance formula (Hammerstad and Jensen)

For strip width W over a substrate of height h and permittivity er, with u = W/h [1]:

- Z_air(u) = (376.73 / 2pi) ln(F/u + sqrt(1 + (2/u)^2)), where F = 6 + (2pi - 6) exp(-(30.666/u)^0.7528)
- eeff = (er + 1)/2 + ((er - 1)/2) (1 + 10/u)^(-a b)
- a = 1 + (1/49) ln((u^4 + (u/52)^2) / (u^4 + 0.432)) + (1/18.7) ln(1 + (u/18.1)^3)
- b = 0.564 ((er - 0.9) / (er + 3))^0.053
- Z0 = Z_air / sqrt(eeff)

Finite copper thickness t makes the strip electrically wider [1]. With T = t/h: dU1 = (T/pi) ln(1 + 4e / (T coth^2(sqrt(6.517 u)))), dUr = 0.5 dU1 (1 + sech(sqrt(er - 1))). Then Z0 = Z_air(u + dUr) / sqrt(eeff(u + dUr)).

These are quasi-static, so Z0 and eeff shift slowly with frequency. Several dispersion models exist, such as Kirschning and Jansen [1]. Solder mask over the strip is not in these formulas, and lowers Z0 slightly (rule of thumb). Use them for a first width, then confirm with the fabricator's impedance tool.

## Worked 50 ohm microstrip widths on FR-4 and Rogers

Computed with the formulas above at each laminate's stated Dk. Copper is 35 um (1 oz).

| Laminate | Dk used | h (mm) | W, no thickness (mm) | W, 35 um (mm) | eeff |
|---|---|---|---|---|---|
| Isola 370HR FR-4 | 4.04 | 1.6 | 3.26 | 3.22 | 3.08 |
| Isola 370HR FR-4 | 4.04 | 0.2 | 0.41 | 0.38 | 2.97 |
| RO4003C | 3.55 | 0.508 | 1.14 | 1.10 | 2.74 |
| RO4003C | 3.55 | 0.813 | 1.82 | 1.78 | 2.75 |
| RO4350B | 3.66 | 0.508 | 1.11 | 1.07 | 2.81 |

Reading it: width scales with h, with W about 2h on FR-4. A 3.2 mm trace on a 1.6 mm two-layer board does not fit a 0402 pad or a chip pin, which is why RF boards use a thin dielectric to the first ground: 0.2 mm core or prepreg gives a trace close to pad width.

Sensitivity: the same 3.22 mm width on a board whose true Dk is 4.4 gives 48.1 ohm (computed). That is about 34 dB return loss, so Dk error alone rarely breaks the match. It moves quarter-wave lengths and antennas far more.

## Laminate Dk and loss: use design values

Data sheet values for the laminates in the examples:

| Laminate | Frequency | Dk | Df |
|---|---|---|---|
| Isola 370HR | 1 GHz | 4.17 | 0.0161 |
| Isola 370HR | 2 GHz | 4.04 | 0.0210 |
| Isola 370HR | 5 and 10 GHz | 3.92 | 0.0250 |
| RO4003C | 2.5 / 10 GHz | Design 3.55 | 0.0021 / 0.0027 |
| RO4350B | 2.5 / 10 GHz | Design 3.66 | 0.0031 / 0.0037 |

Sources: [2][3]. Rogers also lists process Dk of 3.38 +/- 0.05 and 3.48 +/- 0.05 from a clamped stripline test at 10 GHz [3]. Use the design Dk for circuit dimensions. Sizing RO4003C at 3.38 and building at 3.55 gives 48.9 ohm (computed), and a resonator sized that way comes out too long, so its frequency lands low.

Other points: Rogers notes Dk falls by about 0.1 as core thickness drops from 0.020 in to 0.004 in, with LoPro foil [3]. "FR-4" is not one material. Dk and Df depend on glass style and resin content, so ask the fabricator for the actual stackup values.

## Coplanar waveguide with ground (CPWG) impedance

Grounded coplanar has a strip of width W with gaps s to side grounds, over a ground plane at distance h. From conformal mapping [4]:

- k1 = W / (W + 2s)
- k3 = tanh(pi W / (4h)) / tanh(pi (W + 2s) / (4h))
- R(k) = K(k)/K'(k). For k up to 0.707: R = pi / ln(2 (1 + sqrt(k')) / (1 - sqrt(k'))), with k' = sqrt(1 - k^2). Above that: R = ln(2 (1 + sqrt(k)) / (1 - sqrt(k))) / pi.
- q = R(k3) / (R(k1) + R(k3)); eeff = 1 + q (er - 1)
- Z0 = (60 pi / sqrt(eeff)) / (R(k1) + R(k3))

Worked 50 ohm values, thin copper assumed (computed):

| Laminate | h (mm) | Gap s (mm) | W (mm) | eeff |
|---|---|---|---|---|
| 370HR, Dk 4.04 | 1.6 | 0.20 | 1.43 | 2.60 |
| 370HR, Dk 4.04 | 1.6 | 0.15 | 1.17 | 2.57 |
| 370HR, Dk 4.04 | 0.2 | 0.30 | 0.41 | 3.06 |
| RO4350B, Dk 3.66 | 0.508 | 0.20 | 0.85 | 2.57 |
| RO4003C, Dk 3.55 | 0.508 | 0.25 | 0.93 | 2.55 |

Side grounds narrow the trace on a thick board by more than half. When s is well above h, coplanar becomes plain microstrip: at 0.2 mm height, W stays near the 0.41 mm microstrip value. Stitch the side grounds, or they become resonant strips.

## Microstrip loss at 915 MHz, 2.45 GHz and 6.5 GHz

Loss in nepers per metre, multiplied by 8.686 for dB [5]:
- Dielectric: alpha_d = (er / sqrt(eeff)) ((eeff - 1) / (er - 1)) (pi / lambda0) tan_d
- Conductor: alpha_c = (Rs / (Z0 W)) Kr Ki, with Rs = sqrt(rho pi f mu), Ki = exp(-1.2 (Z0/376.73)^0.7)

Estimated dB per metre on 50 ohm lines, computed with ED copper at 1.8 um RMS roughness [6], and Dk and Df from the laminate table:

| Laminate, h | 915 MHz | 2.45 GHz | 6.5 GHz |
|---|---|---|---|
| 370HR, 1.6 mm | 2.6 | 8.3 | 24.5 |
| 370HR, 0.2 mm | 6.1 | 15.0 | 36.0 |
| RO4003C, 0.508 mm | 1.6 | 3.4 | 7.1 |
| RO4350B, 0.508 mm | 1.8 | 3.8 | 8.1 |

Divide by 10 for dB per 100 mm. Practical reading: a 20 mm feed on thin FR-4 costs 0.3 dB at 2.45 GHz and 0.7 dB at 6.5 GHz, which is acceptable. A 100 mm run costs 1.5 dB and 3.6 dB, so reach for a laminate change or a coax. Thin traces are lossier because narrow strips have higher conductor loss. These are estimates for comparison; only a measured line settles the number.

## Skin depth and copper roughness

Skin depth: delta = 1 / sqrt(pi f mu sigma), where current density falls to 1/e [7]. With copper at 5.8e7 S/m (computed):

| Frequency | Skin depth |
|---|---|
| 100 MHz | 6.6 um |
| 915 MHz | 2.2 um |
| 2.45 GHz | 1.3 um |
| 6.5 GHz | 0.82 um |

So 35 um copper is many skin depths thick at all these frequencies. Thickness barely matters, but surface does.

Rogers gives rolled copper about 0.3 um RMS and ED copper about 1.8 um RMS [6]. High-profile ED roughness starts to affect loss near 1 GHz and has substantial effect at 10 GHz, and smooth rolled copper has negligible effect until about 50 GHz [6]. The effect is larger on thinner substrates [6].

Hammerstad's correction: Kr = 1 + (2/pi) arctan(1.4 (D/delta)^2), where D is RMS roughness [5]. For 1.8 um ED copper, conductor loss rises by a factor of 1.48 at 915 MHz, 1.76 at 2.45 GHz and 1.91 at 6.5 GHz (computed). For 0.3 um it is 1.02 to 1.12 over the same range. Plating also counts: a treatment less conductive than copper adds loss at high frequency [6], so ask before choosing a finish.

## Via fences and ground stitching spacing

Infineon AN91445 asks for vias spaced no more than one-twentieth of the RF wavelength between the top ground fill and the inner ground layer, and for ground vias right next to pads [8]. It does not say free-space or guided, so use the smaller guided value when space allows.

| Frequency | lambda/20 free space | lambda/20 guided, eeff 3 |
|---|---|---|
| 915 MHz | 16.4 mm | 9.5 mm |
| 2.45 GHz | 6.1 mm | 3.5 mm |
| 6.5 GHz | 2.3 mm | 1.3 mm |

Values computed. Use the highest frequency that matters, including harmonics you are trying to contain: a 2.45 GHz transmitter's third harmonic sits near 7.35 GHz.

Other via rules from the same note [8]:
- More vias in parallel reduce the parasitic inductance.
- Never share a via between pins or pads. Sharing raises common-mode noise between them.
- Fill unused top and bottom areas with ground and tie it to the plane with many vias.
- Do not route the RF trace through vias to another layer.
- Put a via fence between radio sections for isolation.

## RF PCB layout rules and stackup

Rules from Espressif and Infineon reference guidance [8][9]:
- Keep the RF trace 50 ohm, constant width, unbranched and short, on an outer layer with the reference plane on the next layer.
- Run dense ground vias along it. Use 135 degree bends or arcs, not right angles.
- Keep the adjacent ground plane complete. Route nothing beneath it.
- Place matching parts close to the pin, and place shunt parts on the RF trace itself rather than on a stub.
- Clear all layers under a small coax connector such as IPEX.
- Keep crystals, DDR, clocks and USB away from the antenna.
- A module in a housing should have at least 15 mm clearance, per Espressif [9].

Stackup for a four-layer RF board (rule of thumb): RF on top, solid ground on layer 2 at a thin dielectric such as 0.2 mm, power and signals below. It gives narrow traces, short vias and good isolation. For a low-cost two-layer board at 1.6 mm, use CPWG near the chip rather than a 3 mm microstrip.

Where the plane under a pad is too close, the pad itself is a shunt capacitance. Designers clear the plane below large pads on thin stackups to keep impedance flat (rule of thumb).

## Applying this in Sense Sim

Use antenna_calc with formula "microstrip" and {er, h_m, z0} or {er, h_m, w_m} for a starting width. It uses a zero-thickness model; for 1 oz copper reduce W by about 0.04 mm, from the table above. Coplanar feeds are not in antenna_calc, so use the CPWG table or formulas above, or model the feed. Use "skin_depth" with f and sigma, and "wavelength" with f and er for guided lengths.

Set the laminate with antenna_set (`substrate` for the material name, `vars` for the template's dimensions), then antenna_run and antenna_results. Check `no_loss_modelled` and rad_efficiency in the results: a lossless substrate overstates gain on FR-4. Judge with antenna_assess against a target set by antenna_spec.

Material defaults for Dk disagree with the data sheets at design frequency: see the antenna materials document. Before quoting a printed-antenna frequency to a user, restate which Dk it assumes and tell them to confirm it against the fabricator's stackup. Nothing here replaces a measured coupon on a real board.

## Sources

1. Quasi-static microstrip equations (Hammerstad and Jensen) — Qucs technical documentation, accessed 2026 — https://qucs.sourceforge.net/tech/node75.html
2. 370HR data sheet — Isola Group, accessed 2026 — https://www.isola-group.com/wp-content/uploads/data-sheets/370hr.pdf
3. RO4000 Series High Frequency Circuit Materials data sheet — Rogers Corporation, via Eurocircuits, accessed 2026 — https://www.eurocircuits.com/content/uploads/2025/02/RO4000-Laminates-Data-sheet.pdf
4. Coplanar waveguides — Qucs technical documentation, accessed 2026 — https://qucs.sourceforge.net/tech/node86.html
5. Microstrip losses and dispersion — Qucs technical documentation, accessed 2026 — https://qucs.sourceforge.net/tech/node75.html
6. Choosing copper foils for high-frequency PCBs — J. Coonrod, Rogers Corporation, in PCD&F, 2016 — https://rogerscorp.com/-/media/project/rogerscorp/documents/articles/english/advanced-connectivity-solutions/choosing-copper-foils-for-high-frequency-pcbs.pdf
7. Skin effect — Wikipedia, accessed 2026 — https://en.wikipedia.org/wiki/Skin_effect
8. AN91445, Antenna Design and RF Layout Guidelines — Cypress / Infineon, Rev. H — https://www.infineon.com/dgdl/Infineon-AN91445_Antenna_Design_and_RF_Layout_Guidelines-ApplicationNotes-v09_00-EN.pdf?fileId=8ac78c8c7cdc391c017d073e054f6227
9. ESP32-C5 PCB Layout Design — Espressif, accessed 2026 — https://docs.espressif.com/projects/esp-hardware-design-guidelines/en/latest/esp32c5/pcb-layout-design.html
