---
title: Pallets, totes, handling vehicles and people
domain: structure
tags: pallet, ISO 6780, EUR pallet, EPAL, GMA pallet, euro container, KLT, tote, carton, stacking box, forklift, counterbalance, reach truck, AMR, AGV, MiR250, MiR1200, pallet jack, turning radius, Wa, person height, worker, stature, walking speed
aliases: pallet, tấm pallet, ván kê hàng, thùng nhựa, thùng carton, hộp, xe nâng, xe nâng tay, xe tự hành, robot tự hành, công nhân, người, chiều cao người, tốc độ đi bộ, bán kính quay, palette, skid
verified: 2026-11-02
---
# Pallets, totes, handling vehicles and people

## ISO 6780 pallet footprints

ISO 6780 lists six flat pallet footprints for intercontinental handling [1]. The standard text is paid; this summary comes from an open reference, not the standard itself [1].

| Footprint (mm) | Main region | Note |
|---|---|---|
| 1016 x 1219 | North America | GMA footprint, 40 x 48 in |
| 1000 x 1200 | Europe, Asia | same as EUR 3 |
| 1165 x 1165 | Australia | Australian standard pallet |
| 1067 x 1067 | North America, Europe, Asia | 42 x 42 in |
| 1100 x 1100 | Asia | |
| 800 x 1200 | Europe | same as EUR 1 |

Default by region: 800 x 1200 mm in Europe, 1000 x 1200 mm or 1100 x 1100 mm in Asia, 1016 x 1219 mm in North America. Choose 1100 x 1100 mm or 1000 x 1200 mm for a Vietnamese site if the user gives nothing, and say that it is an assumption.

## EUR pallet dimensions and load rating

The EUR 1 (EPAL) pallet is 800 x 1200 mm and 144 mm high, weighing about 25 kg [2][3]. Rated load is 1500 kg when evenly spread and 1000 kg when uneven, with up to 4000 kg of further load when stacked [2]; Mecalux lists 1500 kg dynamic and 4000 kg static [3]. Other types: EUR 2 and EUR 3 are 1000 x 1200 mm, and EUR 6 is 600 x 800 mm [2]. Deck boards are 22 mm thick, from 100 to 145 mm wide [3]. Pallet entry openings were not stated in the sources opened. Add the 144 mm to goods height when computing overall load height for a rack level. A pallet found through `search_assets` with `size_m` near [1.2, 0.144, 0.8] is the right scale.

## Euro container and tote sizes

Euro containers (KLT) use a modular footprint [4]. Common sizes:

| Nominal size (mm) | Actual size (mm) |
|---|---|
| 600 x 400 | 594 x 396 |
| 400 x 300 | 396 x 297 |
| 300 x 200 | nominal only |
| 800 x 600 | nominal only |

Standard overall heights are 147.5 mm and 280 mm, including 15 mm of stacking overlap, so a stack rises by 132.5 mm or 265 mm per box [4]. The 600 x 400 and 400 x 300 sizes fill a 1200 x 800 mm EUR pallet [4]: four 600 x 400 boxes make one layer (calculated). Carton sizes are not standardised in the sources opened, so read cartons from the library with `search_assets` rather than assuming one.

## Counterbalance forklift dimensions

A sit-down electric counterbalance example, from Crown's SC 5200 sheet [5]:

| Model | Capacity at 600 mm | Head length | Width | Turning radius |
|---|---|---|---|---|
| SC 521X-30 | 1361 kg | 1.74 m | 1.067 m | 1.377 m |
| SC 522X-35 | 1588 kg | 1.85 m | 1.067 m | 1.486 m |
| SC 524X-35 | see sheet | 1.96 m | 1.067 m | 1.595 m |

With the 4.8 m triple mast the sheet is based on, the mast is 2.11 m collapsed and 6.05 m extended; other masts differ. Travel speed is about 12-15 km/h (3.4-4.2 m/s), and standard fork options run 0.76-1.83 m [5]. Head length excludes forks, so add the fork or load length to get overall length with a pallet: about 1.2 m more for a 1.2 m load (calculated). Mast height matters for door and sprinkler clearance, since a mast raised to place a top pallet is far taller than the truck when travelling. Larger capacity trucks were not verified here.

## Reach truck dimensions

Reach truck dimensions from Crown data sheets. RR 5725, 1.6-2.0 t: overall length 1.84-2.03 m without forks, width 1.067 m or 1.22 m depending on model, turning radius 1.73-1.89 m, travel about 12-13 km/h (3.3-3.7 m/s), collapsed mast from 2.26 m up to about 4.8 m for the highest masts, extended up to about 12 m [6]. ESR 5200, 1.4-1.6 t: width 1.12 m or 1.285 m, turning radius 1.64-1.74 m, travel 10 km/h (2.8 m/s) [7]. The collapsed mast on the highest RR 5725 models is taller than a 3.05 m dock door, so check doors against the chosen mast. Aisle widths for these trucks are in the warehouse layout document.

## Autonomous mobile robot dimensions

MiR specification values, as examples of AMR size. Other makes differ.

| Item | MiR250 [8] | MiR1200 Pallet Jack [9] |
|---|---|---|
| Length | 800 mm | 1934 mm |
| Width | 580 mm | 820 mm |
| Height | 300 mm | 1990 mm |
| Payload | 250 kg | 1200 kg |
| Top speed | 2.0 m/s | 1.5 m/s |

MiR250 corridor widths, default setup [8]: straight 1.45 m (down to 0.85-0.90 m with muted fields), 90 degree turn 1.5 m, U-turn 1.6 m, two robots passing 3.0 m, pivoting 1.8 m. Acceleration at full load is 0.3 m/s² and it needs 9.5 m to reach top speed [8]. Doorways should be sized from those corridor figures. AGV dimensions and hand pallet truck dimensions were not verified from a primary source.

## Person height for scale and sight lines

Mean adult height in the US (ages 20 and over, August 2021 to August 2023) is 1.75 m for men and 1.61 m for women, converted from 68.9 in and 63.5 in [10]. This is one population, and height varies by country. Use about 1.7 m as a scale check when a photo has a person in it (rule of thumb). The system prompt's 1.4-1.8 m camera height for a handheld photo is consistent with these eye levels. UWB tags worn on a worker sit at about 1.2 m in Sense Sim. Head clearance rules (2.0 m under projections, 2.3 m ceiling on exit routes) are in the factory, hospital, office and retail document.

## Walking speed for worker paths

Measured preferred walking speeds [11][12]:

| Group | Speed |
|---|---|
| Adults about 33 years, BMI 33.6, n = 23 | 1.28 m/s |
| Ages 70-79 | 1.19 m/s |
| Ages 80 and over | 0.94 m/s |
| Required at pedestrian lights, as used by the study | 1.2 m/s |

So use about 1.2-1.3 m/s for a worker on a route, and 0.9-1.0 m/s for a slow walker or a worker carrying or inspecting (0.9 m/s is the template's slow loop value, not a measurement for that task). Sample sizes above are small. The `rack_warehouse` template uses 1.2 m/s and 0.9 m/s for its walking tags, which falls within this range.

## Applying this in Sense Sim

Sizes are metres. Pass real sizes to `search_assets` as `size_m` [width, height, depth], with 0 for unknown, for example [1.2, 0.144, 0.8] for a EUR pallet or [1.067, 2.11, 2.8] for a counterbalance truck with forks. Confirm with `get_asset_info`. Avoid `scale` in `place_objects` unless the asset is clearly wrong; a scaled pallet breaks stacking and aisles. Stack goods with `on_top_of` set to a placed object's name, such as a carton on a pallet, then run `verify_scene`. Plain placeholder loads can go in with `add_structure` `kind` "block" and `size_m`. Moving workers and vehicles are RTLS tags: `rtls_auto_deploy` adds them, and `rtls_add_device` adds them manually at about 1.2 m height. Doors are not an asset rule: leave a wall gap sized from the vehicle's width and mast height.

## Sources

1. Pallet, ISO sizes table — Wikipedia — https://en.wikipedia.org/wiki/Pallet
2. EUR-pallet — Wikipedia — https://en.wikipedia.org/wiki/EUR-pallet
3. Euro pallets EPAL — Mecalux — https://www.mecalux.com/warehouse-manual/pallet/euro-pallet
4. Euro container — Wikipedia — https://en.wikipedia.org/wiki/Euro_container
5. SC 5200 Series specification sheet — Crown Equipment — https://www.crown.com/content/dam/crown/pdfs/en-la/specifications/sc-5200-spec-sheet.pdf
6. RR 5700 Series specifications — Crown Equipment — https://www.crown.com/content/dam/crown/pdfs/apac/specs/reach-truck-rr5700-spec-GB.pdf
7. ESR 5200 Series specifications — Crown Equipment — https://www.crown.com/content/dam/crown/pdfs/en-uk/specs/reach-truck-esr5200-spec-GB.pdf
8. MiR250 Specifications — Mobile Industrial Robots, 2023 — https://www.hartfiel.com/wp-content/uploads/2024/04/MiR250-Specs.pdf
9. MiR1200 Pallet Jack brochure — Mobile Industrial Robots, 2024 — https://www.hartfiel.com/wp-content/uploads/2024/04/MiR1200_Pallet_Jack_Brochure.pdf
10. FastStats: Body Measurements — CDC NCHS — https://www.cdc.gov/nchs/fastats/body-measurements.htm
11. The Determinants of the Preferred Walking Speed in Individuals with Obesity — Obesity Facts, 2019 — https://pmc.ncbi.nlm.nih.gov/articles/PMC6876590
12. Older adults must hurry at pedestrian lights! — Eggenberger et al., PLoS One, 2017 — https://www.ncbi.nlm.nih.gov/pmc/articles/PMC5536437/
