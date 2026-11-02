---
title: Wall and material penetration loss by frequency
domain: rf
tags: penetration loss, wall loss, attenuation, drywall, plasterboard, glass, low-E, brick, concrete, reinforced concrete, rebar, masonry, wood, plywood, metal, water, NIST 6055, ITU-R P.2040, permittivity, conductivity, 900 MHz, 2.4 GHz, 5 GHz, 6.5 GHz, 8 GHz
aliases: suy hao xuyên tường, suy hao vật liệu, tường bê tông, tường gạch, thạch cao, kính, gỗ, ván ép, kim loại, sắt, cốt thép, xuyên tường, through-wall loss, obstruction loss
verified: 2026-11-02
---
# Wall and material penetration loss by frequency

## How to use wall loss numbers and why sources disagree

Material loss has no single right value. Published results vary with thickness, moisture, mix, incidence angle, and measurement method. For concrete, NIST measurements run about 1.6 to 3 times the ITU-R P.2040 slab calculation in decibels, depending on frequency [1][2]. Treat the two as a bracket: P.2040 as a low estimate of a homogeneous slab, NIST as a high estimate from lab specimens at normal incidence.

Design rule: use the upper value when deciding whether a device may rely on passing through a wall, and the lower value when judging interference leakage. Loss rises with frequency for concrete and masonry, so a loss for 900 MHz must not be reused at 6.5 GHz. Oblique paths cross more material and lose more than normal incidence, and most real links are oblique.

## NIST measured loss for concrete by thickness and frequency

NISTIR 6055 measured plain concrete (Type 1 mix) at normal incidence with gated horn antennas; values below come from its published regression fits, evaluated here, relative to free space [1].

| Thickness | 0.9 GHz | 2 GHz | 3 GHz | 5.8 GHz | 6.5 GHz | 8 GHz |
|---|---|---|---|---|---|---|
| 102 mm | 11.8 | 14.9 | 23.5 | 24.1 | 25.3 | 27.8 |
| 203 mm | 22.5 | 28.6 | 46.3 | 51.2 | 54.3 | 63.9 |
| 305 mm | 34.8 | 36.0 | 73.5 | 74.9 | 74.6 | about 90 |

The 8 GHz entry for 305 mm is the text statement in the report, because the fit is unreliable at the band edge [1]. NIST did not measure 2-3 GHz; low and high bands used different antennas, so the step between 2 and 3 GHz should not be read as a physical jump. For 2.4 GHz through 102 mm, use a bracket of 15-23 dB. Mix matters little: 102 mm at 1 GHz gives 12.0 to 13.9 dB across the four mixes evaluated here [1].

## ITU-R P.2040 material parameters and attenuation rate

P.2040-4 models material properties as real permittivity e = a f^b and conductivity s = c f^d in S/m, with f in GHz [2]. Selected values, with b = 0:

| Material | a | c | d | Stated range GHz |
|---|---|---|---|---|
| Concrete | 5.24 | 0.0462 | 0.7822 | 1-100 |
| Brick | 3.91 | 0.0238 | 0.16 | 1-40 |
| Plasterboard | 2.73 | 0.0085 | 0.9395 | 1-100 |
| Wood | 1.99 | 0.0047 | 1.0718 | 0.001-100 |
| Glass | 6.31 | 0.0036 | 1.3394 | 0.1-100 |

Internal attenuation rate for a low-loss dielectric: A = 1636 s / sqrt(e) dB/m [2]. Computed for concrete: 0.31 dB/cm at 0.92 GHz, 0.66 at 2.44 GHz, 1.19 at 5.18 GHz, 1.42 at 6.49 GHz, 1.68 at 8 GHz. Reflection adds further loss and ripple, so total slab loss is not simply rate times thickness. Below 1 GHz concrete and brick are outside the stated range, so 920 MHz values are slight extrapolations.

## P.2040 computed slab loss for common walls

Normal-incidence transmission loss from the P.2040 single-slab equations and Table 3 parameters, averaged over thickness variation of plus or minus 30% to remove resonance ripple (computed from [2]). P.2040 is a representative material model, not a measurement of any given wall.

| Wall | 0.92 GHz | 2.44 GHz | 5.18 GHz | 6.49 GHz | 8 GHz |
|---|---|---|---|---|---|
| Plasterboard 12.5 mm | 0.3 | 1.1 | 1.1 | 0.9 | 1.1 |
| Wood 38 mm | 0.6 | 0.7 | 1.5 | 1.8 | 2.2 |
| Glass 6 mm | 0.4 | 1.9 | 3.1 | 2.6 | 1.6 |
| Brick 100 mm | 2.8 | 3.3 | 3.5 | 3.6 | 3.7 |
| Concrete 100 mm | 4.5 | 7.9 | 12.9 | 15.0 | 17.3 |
| Concrete 200 mm | 7.5 | 14.1 | 23.5 | 27.4 | 31.5 |

Thin slabs show strong resonance: 6 mm glass peaks near 5 GHz and falls by 8 GHz. Quarter-wave style ripple is physical, so a single-frequency measurement can mislead.

## 3GPP material penetration formulas

TR 38.901 Table 7.4.3-1 gives simple frequency formulas used for outdoor-to-indoor loss, with f in GHz [3].

| Material | Formula dB | 0.92 GHz | 2.44 GHz | 5.18 GHz | 6.49 GHz | 8 GHz |
|---|---|---|---|---|---|---|
| Standard multi-pane glass | 2 + 0.2 f | 2.2 | 2.5 | 3.0 | 3.3 | 3.6 |
| IRR (coated) glass | 23 + 0.3 f | 23.3 | 23.7 | 24.6 | 24.9 | 25.4 |
| Concrete | 5 + 4 f | 8.7 | 14.8 | 25.7 | 31.0 | 37.0 |
| Wood | 4.85 + 0.12 f | 5.0 | 5.1 | 5.5 | 5.6 | 5.8 |

Values are computed from the formulas. The concrete formula tracks the computed P.2040 result for a 200 mm slab closely. The coated-glass row is the most useful warning: an energy-efficient window behaves like a thick wall at all these frequencies, about 24 dB, against about 3 dB for plain glass.

## Drywall and plasterboard loss

Drywall is nearly transparent at all bands in this domain. NIST panels of 6, 13 and 16 mm measured about 0.3 dB at 0.9 GHz, 0.6 dB at 2 GHz, and no more than about 1 dB across 3-8 GHz [1]. The P.2040 calculation for 12.5 mm gives 0.3-1.1 dB [2].

A stud partition has two sheets, so roughly double the single-sheet figure, about 1-2 dB, plus metal studs, insulation foil and service ducts where present. NIST did not test assembled stud walls, so this is an estimate from sheet data. Practical consequence: thin office partitions rarely block a UWB or BLE link on their own, and they should not be modelled with a heavy wall. Foil-backed insulation or metal stud spacing changes that and must be checked on site.

## Brick and masonry block loss

NIST clay brick and hollow concrete block results, relative to free space [1]:

| Specimen | 0.9 GHz | 2 GHz | 3-8 GHz |
|---|---|---|---|
| Brick 89 mm | 3.3 | 5.4 | 15-16 |
| Brick 178 mm | 5.1 | 7.6 | fit unreliable |
| Brick 267 mm | 6.4 | 10.6 | fit unreliable |
| Hollow block 203 mm | 11.3 | 11.3 | 15 (3-5.8 GHz) to 18 (8 GHz) |
| Hollow block 406 mm | 16.9 | 17.9 | fit unreliable |
| Hollow block 610 mm | 27.7 | 30.0 | fit unreliable |

The P.2040 calculation for 100 mm brick is only 2.8-3.7 dB across these bands [2], well below NIST above 3 GHz, so brick is a strong disagreement. For a single brick leaf the source bracket is 3-5 dB at 0.9-2 GHz, 3-15 dB at 2.4 GHz (no NIST data between 2 and 3 GHz) and 4-16 dB at 5-8 GHz; verify on site for UWB. Hollow block contains air cells, so its loss does not scale with bulk thickness.

## Reinforced concrete and rebar grid loss

NIST compared 203 mm plain concrete with 1% and 2% steel specimens [1].

| Specimen 203 mm | 0.9 GHz | 2 GHz | 5.8 GHz | 8 GHz |
|---|---|---|---|---|
| Plain (mix 8) | 26.8 | 34.7 | 58.5 | 73.3 |
| 1% steel | 26.5 | 31.0 | 56.7 | 69.0 |
| 2% steel | 29.5 | 36.8 | 60.4 | 70.8 |

Steel at these ratios changes concrete loss by only a few dB. Bare rebar grids tested in air (19 mm bars) show why: a 140 mm grid loses 3.0 dB at 0.9 GHz and under 1 dB at 5-8 GHz, while a 70 mm grid loses 14.4 dB at 0.9 GHz and about 2 dB at 6.5 GHz [1]. Grid openings pass short wavelengths and block long ones, so tighter meshes hurt UHF RFID most. The concrete itself dominates at UWB frequencies.

## Glass and wood measured loss

NIST measured float glass and dry wood [1]. Values in dB.

| Specimen | 0.9 GHz | 2 GHz | 3 GHz | 5.8 GHz | 8 GHz |
|---|---|---|---|---|---|
| Glass 6 mm | 0.7 | 1.4 | 1.9 | 1.0 | 1.5 |
| Glass 13 mm | 2.0 | 3.3 | 0.6 | 0.4 | 1.8 |
| Lumber dry 38 mm | 2.7 | 3.3 | 3.9 | 3.5 | 4.4 |
| Lumber dry 76 mm | 2.5 | 4.8 | 8.3 | 7.8 | 9.0 |
| Lumber dry 152 mm | 5.7 | 8.5 | 18.8 | 21.6 | 25.3 |
| Plywood dry 13 mm | 0.5 | 1.0 | 0.5 | 0.0 | 0.8 |
| Plywood wet 13 mm | 1.7 | 2.8 | 4.4 | 2.4 | 3.0 |

Moisture matters: wet 76 mm lumber loses about 14-15 dB at 3-8 GHz against 8-9 dB when dry [1]. P.2040 lists plywood with a constant 0.33 S/m, which computes to about 4-5 dB for 12 mm [2]; that is several times the NIST dry result, so check the sample source before relying on either.

## Metal and conductive surfaces

P.2040 lists metal as conductivity 1e7 S/m [2]. Using its conductor attenuation A = 545.8 sqrt(s f) dB/m, a 1 mm sheet gives over 1700 dB at 1 GHz (computed), which means the sheet itself passes nothing measurable. All real energy goes around it: through gaps, by diffraction over edges, or by reflection.

So metal should be treated as a blocker for the direct ray, and coverage behind it should be judged by the size of the shadow and available reflected paths. Racks, machines and vehicles shadow rather than attenuate. A measured transmission figure quoted for a metal object really describes its leakage geometry and will not transfer to another object.

## Water-filled goods and liquid-bearing materials

Water adds loss, and the effect shows up in wet building materials: NIST wet lumber and wet plywood generally lose more than dry, clearly so above 3 GHz [1]. By extension, a pallet of drinks, a rack of bottles, or a coolant drum should be expected to shadow a UWB or 2.4 GHz link like a heavy wall (rule of thumb).

No primary source with a directly comparable dB-per-thickness figure for packaged liquids at these frequencies was opened for this document, so no number is given here. Measure the specific goods, or use a local FDTD study, when RFID tags or UWB paths must pass through liquid-bearing stock. For RFID tags on liquid goods, also expect detuning, covered in tag selection guidance rather than here.

## Applying this in Sense Sim

Radio obstruction is wall_loss_db x wall_eq, where wall_eq sums material weights: thin 0.15, wood 0.4, glass 0.5, generic 1, concrete 2, liquid 4, metal 6. Generated walls scale by thickness / 0.10 m, so 10 cm concrete is 2 and 20 cm is 4. Default wall_loss_db is 6 (BLE, WiFi 2.4), 8 (WiFi 5 GHz, RFID), and UWB uses 6 in code.

Sim concrete at 20 cm: BLE 24 dB (sources 14-29 near 2-2.44 GHz), UWB 24 dB (27-54), RFID 32 dB (7.5-23), which overstates UHF loss and understates UWB loss. To calibrate, run an FDTD through-object study and use its Use in the floor map action; this is a UI action, with no rtls_ tool. Then re-check with rtls_coverage and rtls_run. rtls_status reports blocked_links and nlos_pct in its RF summary.

## Sources

1. NISTIR 6055, Electromagnetic Signal Attenuation in Construction Materials — W. C. Stone, NIST, 1997 — https://nvlpubs.nist.gov/nistpubs/Legacy/IR/nistir6055.pdf
2. Recommendation ITU-R P.2040-4, Effects of building materials and structures on radiowave propagation in the range of 1 MHz to 450 GHz — ITU, 2025 — https://www.itu.int/dms_pubrec/itu-r/rec/p/R-REC-P.2040-4-202509-I!!PDF-E.pdf
3. 3GPP TR 38.901 v18.0.0, Study on channel model for frequencies from 0.5 to 100 GHz — ETSI/3GPP, 2024 — https://www.etsi.org/deliver/etsi_tr/138900_138999/138901/18.00.00_60/tr_138901v180000p.pdf
