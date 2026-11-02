---
title: Factory, hospital, office and retail room dimensions
domain: structure
tags: ceiling height, room height, corridor width, door width, hospital, ward, bed space, bed spacing, patient room, ICU, operating room, office, workstation, desk, floor area per person, retail, supermarket, shop, gondola, shelf, checkout, accessible route, ADA, HBN 04-01, ASR A1.2, DHA, egress, exit route
aliases: bệnh viện, phòng bệnh, giường bệnh, khoảng cách giường, hành lang, văn phòng, bàn làm việc, cửa hàng, siêu thị, kệ siêu thị, lối đi siêu thị, quầy thu ngân, chiều cao trần, trần nhà, nhà máy, ward, canteen, shop floor
verified: 2026-11-02
---
# Factory, hospital, office and retail room dimensions

## Workplace ceiling height by floor area

Germany's workplace rule ASR A1.2 sets minimum clear height by room area [1], which is a useful baseline for factories and offices:

| Room floor area | Minimum clear height |
|---|---|
| Up to 50 m² | 2.50 m |
| Over 50 m² | 2.75 m |
| Over 100 m² | 3.00 m |
| Over 2000 m² | 3.25 m |

The height may drop by 0.25 m after a risk assessment, never below 2.50 m [1]. US OSHA sets 2.3 m for exit routes, with projections no lower than 2.0 m [2]. Machines, cranes and ventilation often push factory height higher than these minimums. Other countries set their own minimums, and the local code governs.

## Workplace pedestrian route width

Widths for walking routes, from the archived 2012 text of Germany's ASR A1.8 [3]:

| Route | Minimum clear width |
|---|---|
| Up to 5 people served | 0.875 m |
| Up to 20 people | 1.00 m |
| Up to 200 people | 1.20 m |
| Up to 300 people | 1.80 m |
| Up to 400 people | 2.40 m |
| Workplace access aisle | 0.60 m |
| Maintenance aisle | 0.50 m |

Summaries of the newer rule differ for up to 5 people: 0.875 m [4] or 0.9 m [8]. Clear height of routes is at least 2.0 m, or 2.1 m for new workplaces [3]. OSHA's exit access minimum is 0.71 m [2]. These are legal floors; a factory main aisle with trucks is wider, sized from the vehicle route formula in the warehouse layout document. Local code governs escape routes.

## Hospital corridor width

Dubai Health Authority's openly published guidelines give clear widths, measured clear of handrails, with about 100 mm allowed for a handrail [5]:

| Corridor | Clear width |
|---|---|
| Inpatient, operating, ICU, with regular bed traffic | 2.45 m |
| Major inter-department and public | at least 2.45 m |
| Irregular bed traffic, such as radiology | 2.0 m |
| Outpatient, any patient use | at least 1.85 m |
| Staff only, up to 12 m long | 1.5 m |

Corridors may need widening at doors so beds can turn, or a recessed or double door [5]. The English HBN 00-04 corridor figures could not be opened. These are one authority's recommendations, not a universal rule, and the local health code governs.

## Hospital ceiling height

Dubai Health Authority recommendations [5]:

| Space | Ceiling height |
|---|---|
| Patient bed rooms and bays, treatment, offices | 2.7 m |
| Corridors in new areas | 2.7 m, minimum 2.4 m |
| ICU, CCU, resuscitation | 3.0 m |
| Operating, interventional imaging, birthing | at least 3.0 m |
| Occupied areas, absolute minimum | 2.4 m |
| Plant room traffic areas | at least 2.4 m |
| Loading dock, canopy soffit | 3.2 m |

Scotland's SHPN 04-01 also requires at least 2.7 m in bedded areas [6]. Stowed ceiling-mounted equipment must clear 2.15 m [5]. X-ray and operating rooms may need more for equipment [5].

## Ward bed space and bed spacing

England's HBN 04-01 states that most bedside activity fits a clear bed space of 3.6 m wide by 3.7 m deep, excluding fixed storage and worktops [7]. So beds in a bay sit about 3.6 m centre to centre (derived from that width), and a row of four needs about 14.4 m of length. Reported areas [7]:

| Room | Area |
|---|---|
| Single-bed room | 23.5 m² |
| 4-bed room, beds and clinical support | 64 m² |
| 4-bed room, including assisted shower and WC | 72.5 m² |

Wards plan a mix of single rooms and multi-bed rooms [7]. Older or low-cost wards may be tighter, so state 3.6 m as the modern guideline. Bed sizes themselves were not verified; read them from the asset with `get_asset_info`.

## Office floor area per workstation

Reported German office guidance: at least 8 m² for the first workstation and 6 m² for each additional one; 8-10 m² for a single office and 12-15 m² per workstation in open plan [8]. A screen workstation needs at least 1 m depth and width, and a movement area of at least 1.5 m² that does not overlap traffic routes [8]. A path to windows or radiator controls can be 0.6 m wide, and an escape route for up to 5 people 0.9 m wide [8]. The UK minimum is 11 m³ per person, counting any height above 3 m as 3 m; this equals about 3.7 m² at 3 m or 4.6 m² at 2.4 m, and furniture may make it too small [9]. Staff-only office corridors can be 1.5 m clear when no longer than 12 m [5].

## Desk, counter and work surface heights

US accessibility rules give heights that apply across buildings:

| Element | Height or size | Section |
|---|---|---|
| Dining and work surfaces | 0.71-0.865 m | 902.3 [10] |
| Sales counter, accessible portion | at most 0.915 m | 904.4 [10] |
| Check-out counter | at most 0.965 m | 904.3.2 [10] |
| Knee clearance width | at least 0.76 m | 306.3 [10] |
| Clear floor space at a fixture | 0.76 x 1.22 m | 305.3 [10] |

Standing work heights depend on task. CCOHS gives about 5 cm above elbow height for precision work, 5-10 cm below elbow for light work, and 20-40 cm below elbow for heavy work [11]. Standard desk sizes such as EN 527 were not verified. Read furniture sizes from the asset.

## Retail aisle width

The firmest open number for shop aisles is the US accessible route: at least 0.915 m clear [10]. Where a route is narrower than 1.525 m, passing spaces of 1.525 x 1.525 m are needed at most every 61 m [10]. A 180 degree turn around a gondola end under 1.22 m wide needs 1.065 m approaching and 1.22 m at the turn, unless the turn is 1.525 m wide [10]. Check-out aisles follow the same walking-surface rules and have counters up to 0.965 m [10]. Commercial grocery aisle widths were not verified from a primary source: a shelf aisle must be wider than the accessible minimum if two trolleys must pass, so state the width as an assumption. An Innsbruck field study found that removing secondary displays from a congested aisle raised sales by about 11.5 percent, with cart users most sensitive to tight aisles [12], so keep the aisle clear of mid-aisle displays.

## Retail gondola shelving dimensions

Open data on shop fixtures is thin. A US patent describes a standard gondola section of about 1.22 m seam to seam, with sections also made at 1.52 m, 0.91 m and 0.76 m [13]. Fixture depth and gondola height were not verified from a primary source, so do not state a standard height. A small HL Display end unit measures 395 x 200 x 560 mm [14], which is an accessory, not a full gondola. Read the size of library shelving from `get_asset_info` and keep rows on the section module. Put rows parallel, with the aisle measured between the fronts of the fixture bases.

## Applying this in Sense Sim

Walls and ceilings come from `add_structure`: `kind` "wall", "column", "floor" or "block" with `size_m` [length, height, depth], `x`, `z`, `yaw_deg`; `kind` "ceiling" with `height_m`. Omit `with_beams` for finished rooms such as wards and offices, and use it in factories. If you omit `height_m`, the ceiling is placed at the tallest object + 1 m and at least 3 m, which is too low for an ICU, so always pass it. Find beds, desks and shelves with `list_asset_categories` and `search_assets` with `size_m`. Place them in one `place_objects` call, with `yaw_deg` turning each to face the aisle: origin is bottom centre, and yaw 0 faces +Z. For a ward bay of beds, use x spacing 3.6 m and check with `verify_scene`, then `set_camera` and `screenshot` to confirm the layout reads as intended.

## Sources

1. ASR A1.2 Raumabmessungen, section 6 — BGN Branchenwissen — https://vorschriften.bgn-branchenwissen.de/daten/tr/asr_a1_2/6.htm
2. 29 CFR 1910.36 Design and construction requirements for exit routes — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1910/1910.36
3. ASR A1.8 Verkehrswege, archived 2012 text, superseded 2022 — umwelt-online — https://umwelt-online.de/recht/arbeitss/arbst.vo/arbst.rl/a1_8.htm
4. Fussgaengerverkehrswege im Lager nach ASR A1.8 — Industrie-Fachwissen (secondary) — https://industrie-fachwissen.de/lager/fussgaengerverkehrswege-lager-asr-a18-staplerverkehr-trennung
5. Health Facility Guidelines, Part C section 2, Space Standards and Dimensions — Dubai Health Authority — https://services.dha.gov.ae/tahpi-app/HealthFacilityGuidelines/Guidelines/FileContent/Preview/DHAHFG/Part%20C_2%20-%20Space_Standards_Dimensions
6. Scottish Health Planning Note 04-01: Adult in-patient facilities — NHS Scotland, 2010 — https://www.nss.nhs.scot/media/5267/shpn-04-01-v1-oct-2010.pdf
7. Health Building Note 04-01: Adult in-patient facilities — NHS England, 2009 — https://www.england.nhs.uk/wp-content/uploads/2009/12/HBN_04-01_Final.pdf
8. Arbeitsstättenverordnung Büro: Mindestgröße und Raumhöhe — Haufe — https://www.haufe.de/arbeitsschutz/sicherheit/arbeitsstaetten-regel-wie-viel-platz-muss-im-buero-sein_96_224924.html
9. How much space am I entitled to at work? — HSE — https://www.hse.gov.uk/contact/faqs/roomspace.htm
10. ADA Accessibility Standards — US Access Board — https://www.access-board.gov/ada/
11. Working in a Standing Position — CCOHS — https://www.ccohs.ca/oshanswers/ergonomics/standing/standing_basic.html
12. Field tests: clearing aisle islands boosts sales — University of Innsbruck, 2026 — https://www.uibk.ac.at/en/newsroom/2026/field-tests-clearing-aisle-islands-boosts-sales/
13. US 5472103 Segmented shelving construction — US patent — https://patents.google.com/patent/US5472103
14. SKM-S Kit gondola end unit — HL Display — https://www.hl-display.com/uk/catalogue/impulse--promotion/cross-merchandising/gondola-end-units/skm-s-kit
