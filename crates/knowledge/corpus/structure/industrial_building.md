---
title: Industrial building shell dimensions
domain: structure
tags: portal frame, steel frame, truss, roof, rafter, haunch, purlin, eaves height, eave, span, frame spacing, bay, roof pitch, mezzanine, guardrail, stairs, industrial door, sectional door, personnel door, floor slab, rooflight, wall, cladding, RF attenuation, concrete wall, metal, shed
aliases: nhà xưởng, xưởng, khung thép, khung tiệp, kèo, vì kèo, mái, mái tôn, trần, xà gồ, cột, chiều cao diềm mái, nhịp, bước khung, gác lửng, sàn lửng, lan can, cầu thang, cửa cuốn, cửa công nghiệp, tường, sàn bê tông, factory shell, roof truss
verified: 2026-11-02
---
# Industrial building shell dimensions

## Portal frame span and spacing

A steel portal frame is the common shell for sheds. Typical values, not limits, from SteelConstruction.info [1][2][3]:

| Item | Typical value |
|---|---|
| Span, single-span pitched | 15-50 m |
| Most efficient span | 25-35 m |
| Economical in practice | up to about 50 m; unit cost lowest at 30-35 m |
| Frame spacing | 6-8 m; large sheds 6-10 m, typically 8 m |
| Roof pitch | 5-10 degrees, commonly 6 |
| Mono-pitch span | up to 15 m |

Multi-bay frames often drop alternate internal columns using valley beams [3]. A multi-span example has 36 m spans and 12 m to the haunch underside [2].

## Eaves height and clear height of sheds

Height ranges for UK industrial buildings [3]:

| Building | Height |
|---|---|
| Low eaves | 4-8 m eaves |
| Large-span low eaves | 6-8 m eaves |
| High eaves | 10-13 m eaves |
| Very high bay | clear height up to 20 m |
| Supermarkets, simple warehouses | 4-6 m clear |
| Distribution with overhead cranes | 10-13 m or more clear |

The portal frame guide cites 8-18 m clear from floor to haunch underside [1]. Haunches, beams and lights sit under the eaves, so the clear height is less than the eaves height. A sloped roof at 6 degrees rises about 1.3 m over half of a 25 m span (calculated from tan 6 degrees), so the apex is above eaves by that amount. Sense Sim models a flat ceiling only, so choose the clear height for `height_m`.

## Roof truss and long-span options

Lattice trusses suit spans over 50 m and become more efficient beyond 60 m [2]. Large distribution centres use steel trusses of 50-80 m and space frames up to 100 m [3]. Pitched or short-span flat roof rafters can be as shallow as 1/40 of the span [2]. As an example, a 36 m multi-span frame used rafters 457 or 533 mm deep and columns 686 mm deep, with total steelwork around 35 kg/m² [2]. Upper-floor long-span beams over retail are 12-18 m [3]. A frame with a 45 m span at 8 m bay centres and omitted valley columns gave column-free areas over 700 m² [2]. Rafters over 20 m may need site splices for transport [1].

## Haunch, purlin and crane details

Portal frame details useful for visual plausibility, from SteelConstruction.info [1]:

- Eaves haunch length: about 10 percent of the span.
- Haunch depth below rafter axis: about 2 percent of the span.
- Purlin spacing: up to 1.8 m, closer near eaves.
- Crane brackets: suit travelling cranes up to about 20 t.
- Service load on the roof: 0.1-0.25 kN/m².

For a 30 m span this means a 3 m long haunch about 0.6 m deep (calculated). Secondary steelwork such as purlins and side rails is 15-25 percent of the steel weight [2]. Sense Sim's beam pieces are plain boxes, so purlins are usually left out unless they are needed for the look; a roof trussed every 6-8 m reads correctly.

## Floor slab and wall envelope

Ground floor concrete is at least 150 mm thick on at least 150 mm of granular fill, and column bases are typically about 450 mm below finished floor [2]. Rooflights are typically 10-15 percent of the roof area in UK sheds, and offices are about 5 percent of the building area [2]. Up to three mezzanine levels are used in some high eaves buildings [3]. Thicknesses of insulated wall and roof panels were not verified from an open primary source. A 0.2 m thick `wall` piece, the default used by photo reconstruction in Sense Sim, is a modelling choice, not a measured panel thickness.

## Mezzanine edges and stairs

Mezzanine dimensions from US workplace rules, which are typical values where the local code governs:

| Item | Value | Rule |
|---|---|---|
| Guardrail top edge | 1.07 m, +/-0.08 m | 1910.29(b)(1) [4] |
| Toeboard | at least 0.09 m | 1910.29(k)(1)(ii) [4] |
| Stair angle | 30-50 degrees | 1910.25(c)(1) [5] |
| Riser | at most 0.24 m | 1910.25(c)(2) [5] |
| Tread depth | at least 0.24 m | 1910.25(c)(3) [5] |
| Stair width | at least 0.56 m | 1910.25(c)(4) [5] |
| Headroom over stairs | at least 2.03 m | 1910.25(b)(2) [5] |
| Landing depth | at least 0.76 m | 1910.25(b)(4) [5] |

Keep at least 2.0 m clear walking height under a mezzanine; new German workplaces use 2.1 m [6]. Add vehicle height plus 0.2 m where trucks pass underneath [6]. A portal frame's stability check must include the mezzanine [1]. Deck loading and column grid were not verified from an open source.

## Industrial door sizes

Door rules of thumb from open sources:

| Door | Size |
|---|---|
| Dock door | 2.44-3.05 m wide, 2.44-3.05 m high [7] |
| Forklift double-action door | twice as wide as the goods, 500 mm higher than the goods [8] |
| Accessible personnel door | at least 0.815 m clear; 0.915 m for openings deeper than 0.61 m [9] |
| Exit route | at least 2.3 m ceiling, 0.71 m wide [10] |

Hörmann double-action doors reach up to 4.5 m high for forklift traffic [8]. Hörmann also links up to three sectional doors to make openings up to 30 m wide with movable centre supports [11]. For a forklift with a pallet: door width = 2 x load width, height = mast or load height + 0.5 m [8]. For a 1.2 m load that is 2.4 m wide, in line with dock door width.

## How building materials affect RF

NIST measured 0.5-8 GHz transmission for common materials [12]. At 0.5-2 GHz through 267 mm brick, about 40 percent of power was received (about 4 dB, calculated); through 610 mm of masonry block at 1 mW, about 5 percent (13 dB, calculated) [12]. A 13 mm plywood panel lost 10 percent of power, 19 mm glass 30 percent, 13 mm drywall 6 percent [12]. Concrete absorbed the most, yet signals still passed 305 mm slabs [12]. One simulation study gives about 10-15 dB per room and wall for 0.15-0.20 m concrete at 2.4 GHz, as a rough on-site figure [13]. ITU-R P.2040-3 models metal with conductivity 10^7 S/m at 1-100 GHz, which behaves as a near-perfect conductor [14]; treat steel cladding, roof sheet and racks as blocking and reflecting (inference from that model). NIST also tested water-soaked wood and steel rebar grids, so wet or reinforced walls need their own figures [12].

## Applying this in Sense Sim

Shell pieces come from `add_structure`. `kind` "wall" uses `size_m` [length, height, depth], `x`, `z`, `yaw_deg`; "column" and "beam" use the same shape; "floor" sits below y = 0 by its thickness. `kind` "ceiling" with `height_m` covers the whole scene with a 2 m margin. Set `with_beams` true and `beam_spacing_m` to the frame spacing, 6-8 m. Beam pieces are 0.2 m wide by 0.4 m deep, spanning x and spaced along z, so span the short direction with x if you want them to read as trusses, or place extra `beam` pieces yourself with `yaw_deg` 90. Mezzanines are a raised `floor` or `block` plus columns; use the `y` parameter for height. Stairs and doors have no generated part: place library assets or leave a wall gap. RF matters: rtls tools ray cast against these pieces, so build the shell first, then use `rtls_auto_deploy` or `rtls_coverage`.

## Sources

1. Portal frames — SteelConstruction.info — https://steelconstruction.info/Portal_frames
2. Single storey industrial buildings — SteelConstruction.info — https://www.steelconstruction.info/Single_storey_industrial_buildings
3. Cost planning: industrial buildings — SteelConstruction.info — https://steelconstruction.info/topics/cost-of-structural-steelwork/cost-planning-industrial-buildings/
4. 29 CFR 1910.29 Fall protection systems — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1910/1910.29
5. 29 CFR 1910.25 Stairways — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1910/1910.25
6. ASR A1.8 Verkehrswege, archived 2012 text, superseded 2022 — umwelt-online — https://umwelt-online.de/recht/arbeitss/arbst.vo/arbst.rl/a1_8.htm
7. Dock Planning 101 — LODEM / MHI, 2023 — https://og.mhi.org/downloads/industrygroups/lodem/dock_planning_101.pdf
8. Double-action doors and strip curtains — Hörmann — https://www.hoermann.com/fileadmin/_country/kataloge/pdf/85728_Double_Action_Doors_and_Strip_Curtains_EN.pdf
9. ADA Accessibility Standards, section 404 — US Access Board — https://www.access-board.gov/ada/
10. 29 CFR 1910.36 Design and construction requirements for exit routes — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1910/1910.36
11. Industrial sectional doors — Hörmann UK — https://www.hormann.co.uk/industry-commerce-and-public-authorities/industrial-doors/industrial-sectional-doors/
12. Electromagnetic Signal Attenuation in Construction Materials, NISTIR 6055 — W. C. Stone, NIST, 1997 — https://nvlpubs.nist.gov/nistpubs/Legacy/IR/nistir6055.pdf
13. Different kinds of walls and their effect on the attenuation of radiowaves indoors — Ali-Rantala et al., IEEE, 2003 — https://usnc-ursi-archive.org/aps-ursi/2003/ursi/s120p05j.pdf
14. buildingMaterialPermittivity, based on ITU-R P.2040-3 — MathWorks — https://www.mathworks.com/help/comm/ref/buildingmaterialpermittivity.html
