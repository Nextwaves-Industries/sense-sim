---
title: Warehouse layout dimensions
domain: structure
tags: warehouse, clear height, column grid, column spacing, aisle width, forklift aisle, reach truck, counterbalance, VNA, turret truck, right angle stack, Ast, pallet rack, selective rack, APR, beam, upright, frame depth, bay, flue space, transverse flue, longitudinal flue, sprinkler clearance, ESFR, NFPA 13, EN 15620, EN 15635, FEM, RMI, MH16.1, dock door, loading dock, truck court
aliases: nhà kho, kho hàng, kệ, kệ pallet, kệ hàng, lối đi, lối đi xe nâng, xe nâng, chiều cao thông thủy, bước cột, lưới cột, cửa dock, cửa xuất nhập hàng, sàn dock, đầu phun chữa cháy, khe thoát khói kệ, warehouse aisle, racking, racks, distribution centre
verified: 2026-11-02
---
# Warehouse layout dimensions

## Warehouse clear height by building type

Clear height is the usable distance from the finished floor to the lowest immovable obstruction, which is usually lower than the roof deck [1]. Typical values from a US logistics owner [1], converted to metres:

| Facility type | Clear height |
|---|---|
| Light industrial, flex | 5.5-7.3 m |
| Mid-bay warehouse | 7.3-8.5 m |
| Standard distribution | 8.5-9.8 m |
| Class-A bulk distribution | 9.8-11.0 m |
| Large e-commerce with automated storage | 12.2 m or more |

A UK steelwork reference gives lower figures for smaller sheds: 4-6 m clear for supermarkets and simple warehouses depending on racking, 10-13 m or more for distribution centres with overhead cranes, and up to 20 m for very high bay [2]. Ask the user or read the photo before picking a value, since the range is wide.

## Warehouse column grid spacing

Columns inside storage bays limit where racks can go, so column spacing and clear height should be chosen together [1]. Modern class-A distribution buildings commonly use about 15.2 x 15.2 m up to 17.1 m or more [1]; a second source from the same owner gives 15.2-18.3 m [3]. In the UK, portal frames are typically 6-10 m apart, usually 8 m, with spans of 25-40 m, and trusses of 50-80 m span large distribution centres [2]. So a European shed often shows one short and one long grid dimension (for example 8 m x 30 m), while a US distribution centre is closer to square. Pairings reported by one source [1]: counterbalance trucks with 8.5-9.8 m clear height and 12.2 x 12.2 m or 15.2 x 15.2 m grids; reach trucks with 11 m or more; turret trucks with 12.2 m or more and wide spacing.

## Forklift aisle width by truck type

Published aisle widths are planning ranges. The truck's data sheet governs. All values are metres between loads or rack faces, converted from feet where needed.

| Truck | Toyota [4] | Link Logistics [1] | Boxic guide [5] |
|---|---|---|---|
| Counterbalance, 4-wheel | 3.2 or more | 3.66-3.96 | 3.6-4.2 |
| Reach truck, stand-up 3-wheel | 2.4-3.2 | 2.44-3.05 | 2.7-3.1 |
| Order picker, turret truck (VNA) | 1.5-2.1 | 1.52-1.83 | 1.6-1.8, rail or wire guided |

Use 3.6 m for counterbalance, 2.8 m for reach and 1.7 m for VNA as defaults, and state them as assumptions. Those three defaults are picked from inside the table, not taken from a single source. VNA aisles need guidance infrastructure in the floor or on the racks [1][5].

## Right-angle stacking aisle formula

To size an aisle for a known truck, use the manufacturer's right-angle stacking figure.

Aisle (m) = right-angle stack + load length + clearance [6]

Raymond recommends 0.30 m clearance; for a stand-up counterbalance truck with a 1.684 m right-angle stack and a 1.219 m load this gives 3.21 m [6]. Crown's sheet writes the same idea as turn radius + load distance + load length + clearance [7]. For a Crown SC 5200 (turn radius 1.377 m, load distance 0.365 m) with a 1.2 m load and 0.30 m clearance, that is 3.24 m (calculated) [7]. For reach trucks one training source uses outer turning radius + lost load centre - reach distance + load length, plus at least 0.23 m operator clearance [8]. Another guide uses 0.20 m per side between load and rack [5].

## Reach truck aisle width from data sheets

VDI-style data sheets give working aisle Ast directly. Crown RR 5725, 1.6-2.0 t: Ast 2.79-3.00 m for a 1200 mm wide by 1000 mm deep load, 2.88-3.00 m for 1000 wide by 1200 deep, and 2.84-2.95 m for 800 wide by 1200 deep, depending on battery compartment; turning radius 1.73-1.89 m; overall length 1.84-2.03 m; width 1.067 or 1.22 m [9]. Crown ESR 5200, 1.4-1.6 t and larger: turning radius about 1.64-1.81 m, aisle values about 2.6-2.9 m across its load and battery tables, travel 10 km/h [10]. So 2.8-3.0 m is a realistic reach truck aisle, and a figure below 2.6 m needs a specific truck behind it.

## Pedestrian and shared traffic route width

Germany's workplace rule ASR A1.8 is openly published and gives a useful formula. Vehicle route width = widest vehicle or load + an edge allowance on both sides, plus 0.40 m when two vehicles pass, at under 20 km/h [11]. Edge allowance: 0.50 m per side for vehicles only, 0.75 m per side with pedestrians in the 2012 text [11]; a secondary summary of the current rule uses 0.50 m per side, so check the current edition [12]. Clear height: vehicle with load and driver plus at least 0.20 m [11]. Pedestrian routes between storage equipment: 1.25 m; manual-loading aisles: 0.75 m [11]. An archived 1972 OSHA letter recommends aisles at least 0.91 m wider than the largest equipment, or 1.22 m minimum [13]. The current OSHA rule asks for sufficient, marked and unobstructed aisles without a number [14]. Local rules govern.

## Selective pallet rack frame depth and beam length

Steel King's openly published sizing rules [15], converted to metric:

- Upright frame depth = pallet depth - 152 mm, which leaves about 76 mm of overhang front and back. A 1219 mm deep pallet takes a 1067 mm frame. A 1200 mm deep pallet takes about 1050 mm (calculated).
- Beam length for two pallets per level = 2 x load width + 305 mm. Two 1067 mm wide loads take a 2438 mm beam. Two 800 mm wide EUR pallets take about 1905 mm (calculated).
- Round up to the next stocked beam length and check capacity.

A catalogue rack from Hoffmann is 1100 mm deep with 75 mm wide uprights and 100 mm deep beams, in heights of 2.5, 3.6 and 4.7 m [16]. Rack height, beam lengths and capacities come from the manufacturer's tables; those tables are not public.

## Pallet rack level height and top clearance

Steel King's rule allows 254 mm per level for beam depth plus lift-off space [15].

Upright height = (pallets high - 1) x (overall load height + 0.254 m) [15].

For four levels with 1.27 m overall loads that is 4.57 m [15]. The overall load height includes the pallet. Leave space above the top pallet for sprinklers, lights and other overhead items [15]. A floor-level load needs no beam under it, so the first beam sits at about load height plus lift-off space. Pallet-to-upright, pallet-to-pallet and top clearances for each truck class are set by EN 15620 tables, which are paywalled and were not verified here [17]. Use the 0.2 m per side planning figure [5] only as a working allowance.

## EN 15620 and EN 15635 rack tolerances

EN 15620:2021 sets tolerances, deformations and clearances for steel static storage; values depend on facility class [17]. Mecalux reproduces two classes, in force after assembly and before loading [17]:

| Tolerance | Class 300 | Class 400 |
|---|---|---|
| Clear bay width | +/-3 mm | +/-3 mm |
| Clear aisle width | +/-5 mm | +/-15 mm |
| Out of plumb | H/500 | H/350 |

H is the height to the top of the upright [17]. These mean a simulated rack row can be treated as straight: real errors are millimetres to a few centimetres over a tall frame. EN 15635 column protection is reported as at least 0.40 m high and rated at 400 Nm [5]. Design rules also exist in EN 15512, FEM and ANSI MH16.1, but their numeric content was not opened and is not repeated here.

## Pallet rack flue space

Flue spaces are the vertical gaps that let heat and sprinkler water pass through a rack. Reported NFPA 13 (2025) values, as typical values where the local code governs [18]:

| Flue | Value | Condition |
|---|---|---|
| Transverse (between loads, at uprights) | nominal 150 mm | single, double and multiple-row |
| Longitudinal, double-row | not required | storage to 7.6 m |
| Longitudinal, double-row | nominal 150 mm | storage over 7.6 m |
| Longitudinal, multiple-row | not required | transverse flues at most 1.5 m apart, rack at most 6.1 m deep, aisle at least 1.1 m |

FM Global DS 8-9 differs: for double-row racks 75 mm longitudinal with 75 mm or 150 mm transverse at set spacings [18]. In a generated scene, keep about 150 mm between back-to-back rows taller than 7.6 m, and about 150 mm between neighbouring loads.

## Sprinkler clearance above storage

Keep storage below sprinklers. OSHA 29 CFR 1910.159(c)(10) sets at least 0.457 m vertically between sprinklers and material below [19]. Fire protection practitioners report 0.9 m for ESFR and CMDA heads, and say the sprinkler's listing may be tighter than NFPA 13 [20]. These are typical values; the local code and the approved design govern. For planning, the top of the highest load should sit at least 0.46 m under standard sprinklers and 0.9 m under ESFR, and sprinklers themselves hang below the roof, so add their drop too. If the user gives no sprinkler type, assume 0.9 m in a modern rack warehouse. Openly available summaries do not give a single rule for every commodity and height.

## Loading dock door size and spacing

North American dock planning values from the LODEM guide [21] and manufacturers [22][23], converted to metres:

| Item | Value |
|---|---|
| Dock floor height | 1.17-1.32 m, commonly 1.22-1.32 m |
| Door width | 2.44 m limited, 2.74 m preferred, 3.05 m oversize |
| Door height | 2.44 m, 2.74 m popular, 3.05 m most versatile |
| Door centres | 3.66 m minimum, 4.27 m or more common |
| Largest common trailer | 2.59 m wide, 4.11 m high |

Rite-Hite ties dock height to trailer type: about 1.17 m dry, 1.22-1.27 m refrigerated, 1.32 m containers [23]. European trailer and dock dimensions were not verified. For a shell without dock levellers, leave the floor at y = 0 and add a raised outdoor apron only if the user asks.

## Truck court and yard dimensions

Outdoor space drives dock placement. Rite-Hite's rule for the drive approach is twice the longest tractor-trailer length plus 1.5-4.6 m depending on door centres; a 22.9 m combination at 3.66 m centres needs about 50.3 m [23]. A logistics owner asks for at least 36.6 m of truck court depth [3], which is a shorter dimension than the Rite-Hite rule, so treat 36.6 m as a floor. LODEM gives site road minimums: 4.0 m one-way, 7.9 m two-way; right-angle turns of 7.9 m inside and 15.2 m outside radius; decline approaches at most 10 percent [21]. Dock counts scale with size: at least 15 docks for about 9300 m², and over 50 for about 23000 m² [3].

## Applying this in Sense Sim

Start from `load_template` with name `rack_warehouse` if the request is small, then adapt with `update_object`, `place_objects` and `delete_objects`. Find racks with `search_assets` using `size_m`, and read real sizes with `get_asset_info` before spacing rows: rack centre spacing along z = rack depth + aisle. Build the shell with `add_structure`: `kind` "floor", "wall" or "column" with `size_m` [length, height, depth], `x`, `z`, `yaw_deg`. For the roof, use `kind` "ceiling" with `height_m` and `with_beams`, and `beam_spacing_m` for the frame spacing (default 6). Beams hang 0.4 m deep with their bottom 0.6 m under the ceiling, so set `height_m` at least tallest load + 0.6 m + sprinkler clearance. Without `height_m` the tool uses tallest object + 1 m, which is tight. Check with `verify_scene` and `get_scene`.

## Sources

1. What Is Clear Height? A Guide to Warehouse Ceilings and Vertical Space — Link Logistics — https://www.linklogistics.com/news-insights/industrial-real-estate-101/what-is-clear-height-a-guide-to-warehouse-ceilings-and-vertical-space/
2. Cost planning: industrial buildings — SteelConstruction.info — https://steelconstruction.info/topics/cost-of-structural-steelwork/cost-planning-industrial-buildings/
3. How to Choose Industrial Warehouse Space — Link Logistics — https://www.linklogistics.com/news/basics/how-to-choose-industrial-warehouse-space-essential-features-to-consider/
4. Do more with less: narrow aisle forklifts — Toyota Material Handling — https://www.toyotaforklift.com/blog/do-more-with-less-narrow-aisle-forklifts
5. Pallet racking planning — Boxic — https://boxic24.com/english/ratgeber/pallet-racking-planning
6. How To Calculate A Forklift Aisle Width Minimum — Raymond West, 2021 — https://raymondwest.com/learn/blog/2021/sep/forklift-aisle-minimums
7. SC 5200 Series specification sheet — Crown Equipment — https://www.crown.com/content/dam/crown/pdfs/en-la/specifications/sc-5200-spec-sheet.pdf
8. Aisleway dimensions for reach trucks — Fork Lift Training — https://fork-lift-training.co.uk/m/buyers-guide/reach-truck-aisleway-width.html
9. RR 5700 Series specifications — Crown Equipment — https://www.crown.com/content/dam/crown/pdfs/apac/specs/reach-truck-rr5700-spec-GB.pdf
10. ESR 5200 Series specifications — Crown Equipment — https://www.crown.com/content/dam/crown/pdfs/en-uk/specs/reach-truck-esr5200-spec-GB.pdf
11. ASR A1.8 Verkehrswege, archived 2012 text, superseded 2022 — umwelt-online — https://umwelt-online.de/recht/arbeitss/arbst.vo/arbst.rl/a1_8.htm
12. Fussgaengerverkehrswege im Lager nach ASR A1.8 — Industrie-Fachwissen (secondary) — https://industrie-fachwissen.de/lager/fussgaengerverkehrswege-lager-asr-a18-staplerverkehr-trennung
13. Marking and width requirements for aisles in industrial operations, archived letter — OSHA, 1972 — https://www.osha.gov/laws-regs/standardinterpretations/1972-05-15
14. 29 CFR 1910.176 Handling materials, general — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1910/1910.176
15. Pallet Rack Design Guide — Steel King Industries, hosted by Cisco-Eagle — https://www.cisco-eagle.com/uploads/SK-General-Rack/RackDesignGuide.pdf
16. Pallet rack product page — Hoffmann Group — https://hoffmann-group.com/p/992410
17. Pallet racking clearances, citing EN 15620 — Mecalux — https://www.mecalux.com/warehouse-manual/warehouse-design/pallet-racking-clearances
18. Navigating flue space requirements: NFPA 13 vs FM updates — RiskLogic — https://risklogic.com/navigating-flue-space-requirements-nfpa-13-vs-fm-updates
19. 29 CFR 1910.159 Automatic sprinkler systems — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1910/1910.159
20. Storage clearance from nearest fire sprinkler, practitioner forum — MeyerFire — https://www.meyerfire.com/daily/storage-clearance-from-nearest-fire-sprinkler
21. Dock Planning 101 — LODEM / MHI, 2023 — https://og.mhi.org/downloads/industrygroups/lodem/dock_planning_101.pdf
22. Door size — Blue Giant — https://www.bluegiant.com/architects/planning-and-design/door-size/
23. Loading Dock Design Part 1 — Rite-Hite, 2012 — https://www.ritehite.com/en/AM/News/2012/Blog/Loading-Dock-Design-Pt1
