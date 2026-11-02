---
title: Drives, belts and conveyors
domain: structure
tags: motor, induction motor, synchronous speed, slip, 50 Hz, 60 Hz, rpm, belt drive, V-belt, pulley, sheave, speed ratio, belt ratio, center distance, shaft alignment, coupling, conveyor, roller conveyor, belt conveyor, conveyor speed, conveyor width, conveyor height, roller pitch, RollerDrive, Interroll, guarding, nip point, OSHA 1910.219, emergency stop
aliases: động cơ, động cơ điện, tốc độ quay, vòng tua, puly, dây đai, truyền động đai, tỉ số truyền, khớp nối, căn chỉnh trục, băng tải, băng tải con lăn, con lăn, chiều cao băng tải, tốc độ băng tải, che chắn an toàn, máy phát điện, gearbox drive, conveyor line
verified: 2026-11-02
---
# Drives, belts and conveyors

## Induction motor speeds at 50 and 60 Hz

Synchronous speed of an AC motor is n_s = 120 x f / p, with f in Hz and p the number of poles [1]. Calculated values:

| Poles | 50 Hz (rpm) | 60 Hz (rpm) |
|---|---|---|
| 2 | 3000 | 3600 |
| 4 | 1500 | 1800 |
| 6 | 1000 | 1200 |
| 8 | 750 | 900 |

An induction motor runs below this by the slip s = (n_s - n) / n_s. At full load slip ranges from over 5 percent on small motors to under 1 percent on large ones [1]. So a 4-pole motor on 50 Hz runs near 1450 rpm at about 3.3 percent slip (calculated), which is also the default rated speed in Sense Sim. Use 60 Hz values in the Americas and other 60 Hz regions; Vietnam and Europe use 50 Hz (rule of thumb). A drive (VFD) can set other speeds.

## Belt drive speed ratio

For a belt between two pulleys, speed is inversely proportional to size [2]:

n2 = n1 x d1 / d2

where n1 and d1 are driver speed and diameter, and n2 and d2 driven speed and diameter. Actual speeds run about 0.5-1 percent below this because of slip and stretch [2]. Belt speed = pi x pitch diameter x rpm [2]; for metres per second: v = pi x d x n / 60. Example: a 120 mm pulley at 900 rpm drives an 80 mm pulley at 1350 rpm, ratio 1.5, and the belt runs at about 5.7 m/s (calculated). Sense Sim's `engine_drive` recipe uses exactly these sizes. To size for a target, d2 = d1 x n1 / n2.

## Belt drive centre distance and wrap

For V-belts, keep the centre distance larger than the largest pulley diameter and below three times the sum of both pulley diameters [2]. Power ratings assume 180 degrees of contact; large ratios or short centres reduce wrap and traction, so power must be derated from manufacturer tables [2]. Pulleys should be as small as practical, but not under the belt's minimum diameter, which shortens life [2]. Example: for 120 mm and 240 mm pulleys, use centres between 240 mm and 1080 mm (calculated). Published misalignment and rim speed limits (MPTA B11i, B13i) sit behind a form and were not verified. Keep pulleys in one plane with parallel shafts as the design intent.

## Coupled shaft alignment tolerance

There is no international standard for shaft alignment tolerance [3]. A widely used guideline for short flexible couplings [3]:

- Acceptable offset (mm) = 110 / rpm. Excellent is half that.
- Acceptable angularity (mm per 100 mm) = 75 / rpm. Excellent is half that.
- Below 600 rpm, hold the 600 rpm value.

At 1450 rpm that is about 0.076 mm offset and 0.052 mm per 100 mm (calculated). The site's specification, machine or coupling manufacturer takes precedence over this rule [3]. These numbers apply to real machine alignment. In a simulated scene they are far smaller than what a viewer can see, so what matters is that shafts are collinear and the link exists.

## Interroll conveyor widths and speeds

Interroll data for unit-load conveyors, as a realistic reference:

| Conveyor | Speed | Load | Source |
|---|---|---|---|
| MCP 24 V roller straight | up to 1 m/s | 35 kg | [4] |
| MCP 400 V roller straight | up to 2 m/s | 50 kg | [4] |
| MCP belt straights, inclines, curves | up to 2.5 m/s | 50 kg, straights up to 550 kg | [4] |
| HPP belt, roller modules | 0.1-3.0 m/s by module | 50 kg/m typical | [5] |
| Modular pallet conveyor MPP | 0.1-0.5 m/s | up to 1500 kg | [6] |

HPP rated widths are 620, 840, 1025 and 1225 mm [5]. MCP belt inclines reach 16 degrees and merges are 30 or 45 degrees [4]. Interroll also sells light roller platforms in narrower widths, not verified here. From these ranges, carton lines at 0.4-1 m/s and pallet lines under 0.5 m/s are plausible values to use.

## Roller pitch and roller length

Interroll's planning rule: choose roller pitch so that every item is carried by at least three rollers at all times [7]. So pitch is at most one third of the shortest item length, and softer undersides need closer rollers [7]. Pitch and load work together: weight per roller must not exceed its rating, which can require more than three rollers under an item [7]. Reference roller length is conveyor width + 50 mm, or + 100 mm for large goods such as pallets [7]. Chain drives are not recommended over 0.5 m/s because of noise; friction rollers are limited to 0.5 m/s; usual curve speed is at most 0.8 m/s, and tapered rollers keep goods oriented in curves [7]. Example: a 200 mm tote needs pitch at most about 67 mm (calculated).

## Conveyor height and manual work speed

Ergonomic height comes from CCOHS, which gives 65-120 cm as the general range and asks that belts be set for the tallest workers, with platforms for shorter ones [8]. By task [8]: precision work 95-120 cm; light work about 107 cm hand height; heavy downward force about 91 cm; jam clearing about 81 cm. Reach should stay within 45 cm [8]. Interroll lists a minimum conveying height of 700-1000 mm on one high-performance module [5]. CCOHS gives 10 m/min (0.17 m/s) as the maximum where people work on moving items, due to conveyor sickness [8]. Automated lines run faster than that, as the Interroll table shows, so slow down only stations with manual work alongside. Standard transfer heights between units were not verified; line up belt tops.

## Conveyor and belt guarding

US rules, as typical values where the local code governs. Under 29 CFR 1910.219, horizontal belts with both runs at or below 2.13 m need a guard rising at least 0.38 m above the belt or to standard height, and both runs at or below 1.07 m need full enclosure [9]. A pulley with any part at or below 2.13 m is guarded [9]. An overhead belt above 2.13 m is guarded over walkways if it runs at 9.1 m/s or more, has centres of 3.05 m or more, or is 0.2 m or wider [9]. Guardrails are 1.07 m high with a midrail and posts at most 2.44 m apart [9]. On general-industry conveyors, nip points are guarded, or guarded by location at 2.44 m or more above the floor [10]. The US construction rule asks for guards below conveyors crossing work areas and aisles, marked crossovers, and stop controls with a warning before start [11].

## Applying this in Sense Sim

Build with `plant_build` `recipe` "conveyor_line" and `count`, `length_m`, `width_m`, `height_m`, `speed_mps`, `boxes`, `x`, `z`, `yaw_deg`. Defaults: length 2 m, width 0.6 m, height 0.8 m, 0.4 m/s. `recipe` "engine_drive" takes `cylinders`. By hand, use `plant_add_part` `kind` "motor" (`diameter_m` is the shaft), "pulley" (`pulley_diameter_m`, `diameter_m`), "engine" or "conveyor", and `plant_connect` `a`, `b` for shaft to shaft (same speed), pulley to pulley (speed x diameter ratio) or conveyor out to in. Pulleys must have parallel axes and sit within 10 mm of one plane; coupling tolerance in the app is 5 mm and 2 degrees. Set speeds with `plant_set` `on`, `rpm`, `speed_mps`, then `plant_run` and fix alarms. There is no guard part: add guards as `add_structure` `kind` "block" or library assets.

## Sources

1. Induction motor — Wikipedia — https://en.wikipedia.org/wiki/Induction_motor
2. Belt (mechanical) — Wikipedia — https://en.wikipedia.org/wiki/Belt_(mechanical)
3. Shaft alignment tolerance calculator — Sensemore — https://sensemore.io/tools/shaft-alignment-tolerance-calculator/
4. Interroll Modular Conveyor Platform MCP modules — Ultimation — https://www.ultimationinc.com/products-conveyor-systems/interroll-modular-conveyor-mcp/
5. High Performance Conveyor Platform catalogue, 2026 — Interroll — https://www.interroll.com/fileadmin/Downloads/Catalogs/Conveyors_MCP/HPP/Katalog_HighPerformanceConveyorPlatform_EN.pdf
6. Interroll corporate brochure — Interroll — https://www.interroll.com/fileadmin/Downloads/Product_Overview_Brochure/ITRL_Corporate_Brochure_EN.pdf
7. Conveyor rollers planning principles — Interroll — https://www.interroll.com/fileadmin/products/product_data/Rollers_Technical_Information_Planning/Foerderrollen_Planungsgrundlagen_EN.pdf
8. Conveyors: Ergonomics — CCOHS — https://www.ccohs.ca/oshanswers/ergonomics/conveyor_ergonomics.html
9. 29 CFR 1910.219 Mechanical power-transmission apparatus — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1910/1910.219
10. STD 1-12.9 Guarding Belt Conveyors — Minnesota OSHA — https://www.dli.mn.gov/sites/default/files/pdf/STD_1-12.9_belt_conveyors.pdf
11. 29 CFR 1926.555 Conveyors — OSHA — https://www.osha.gov/laws-regs/regulations/standardnumber/1926/1926.555
