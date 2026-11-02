---
title: Piping and pump sizing rules
domain: structure
tags: pipe, piping, DN, NPS, nominal diameter, schedule 40, outside diameter, bore, flow velocity, head loss, friction, Darcy-Weisbach, Hazen-Williams, Reynolds, roughness, NPSH, NPSHa, cavitation, pump, centrifugal pump, affinity laws, suction, discharge, valve placement, reducer, eccentric reducer, straight run
aliases: đường ống, ống, đường kính ống, bơm, máy bơm, tổn thất áp, tổn thất cột áp, vận tốc dòng chảy, lưu lượng, cột áp, van, van bi, côn thu, ống hút, ống đẩy, bồn, bể chứa, xâm thực, pipework, pipe run, plumbing
verified: 2026-11-02
---
# Piping and pump sizing rules

## DN and NPS pipe sizes with outside diameter

DN and NPS name a size, not a diameter. Up to NPS 12 the outside diameter is fixed and larger than the name suggests [1]. Values below use ASME dimensions; inside diameter is calculated as OD minus twice the Schedule 40 wall [1].

| DN | NPS | OD (mm) | Sch 40 wall (mm) | ID (mm) |
|---|---|---|---|---|
| 25 | 1 | 33.4 | 3.38 | 26.6 |
| 40 | 1 1/2 | 48.3 | 3.68 | 40.9 |
| 50 | 2 | 60.3 | 3.91 | 52.5 |
| 65 | 2 1/2 | 73.0 | 5.16 | 62.7 |
| 80 | 3 | 88.9 | 5.49 | 77.9 |
| 100 | 4 | 114.3 | 6.02 | 102.3 |
| 150 | 6 | 168.3 | 7.11 | 154.1 |
| 200 | 8 | 219.1 | 8.18 | 202.7 |
| 300 | 12 | 323.9 | 10.31 | 303.2 |

A DN 50 pipe is about 60 mm across, not 50 mm. The wall thickness and bore change with schedule, and metric tube series were not checked here.

## Water velocity guidance in pipes

Velocity limits avoid noise and wear [2]. Reference ranges in m/s [2]:

| Service | Velocity |
|---|---|
| General water service | 0.9-2.4 |
| Tap water | 1.0-2.5 |
| Cooling water | 1.5-2.5 |
| Process water | 1.5-3 |
| Pump discharge | 1.5-3 |
| Pump suction | 0.9-2.4 |
| Boiler feed suction | 0.5-1.0 |
| Heating circulation | 1.0-3.0 |

The same site publishes a stricter suction table: 0.5 m/s for bores up to 75 mm, 0.6 m/s at 150 mm, 0.9 m/s at 250 mm and 1.4 m/s at 300 mm [3]. These sources disagree, so design suction lines at the low end and say which figure you used. Larger pipe has lower friction but higher first cost [4]. Velocity v (m/s) = Q / A, where Q is flow in m³/s and A is bore area in m².

## Pipe flow at a target velocity

Flow capacity by DN, calculated with Q = v x pi x ID² / 4 and the Schedule 40 inside diameters of the DN table [1]:

| DN | ID (mm) | l/s at 1.5 m/s | l/s at 3 m/s |
|---|---|---|---|
| 25 | 26.6 | 0.84 | 1.7 |
| 50 | 52.5 | 3.2 | 6.5 |
| 80 | 77.9 | 7.2 | 14.3 |
| 100 | 102.3 | 12.3 | 24.6 |
| 150 | 154.1 | 28 | 56 |
| 200 | 202.7 | 48 | 97 |

Pick the smallest size that keeps velocity in the service range from the velocity table [2]. For example, 5 l/s of process water runs at about 2.3 m/s in DN 50 or 1.6 m/s in DN 65 (calculated), both inside the process water range. The suction line should be one or two sizes larger than the pump inlet [5].

## Darcy-Weisbach head loss formula

Head loss in a straight pipe works for any liquid [6]:

h = f x (L / D) x v² / (2 g)

where h is head loss (m), f the Darcy friction factor, L length (m), D bore (m), v velocity (m/s), and g = 9.81 m/s². Some texts use a friction factor one quarter of this one [6].

Reynolds number Re = v x D / nu. Below about 2300 flow is laminar with f = 64 / Re; between 2300 and 4000 f is uncertain; above 4000 it is turbulent [7]. The Swamee-Jain approximation holds for Re 5000 to 10^8 [8]:

f = 0.25 / [log10(k/(3.7 D) + 5.74/Re^0.9)]²

Absolute roughness k: commercial steel 0.045-0.09 mm; PVC 0.0015-0.007 mm; new cast iron 0.25-0.8 mm [7].

Example, calculated with nu = 1e-6 m²/s for water near 20 °C: DN 50 steel at 1.5 m/s gives Re about 79000, f about 0.022, and about 4.9 m of head per 100 m.

## Hazen-Williams head loss for water

Hazen-Williams is an empirical shortcut for water only [9].

h = 10.67 x q^1.85 / (c^1.85 x d^4.8655)

where h is head loss per metre of pipe (m/m), q flow (m³/s), d inside diameter (m), and c a material coefficient [9]. Higher c means smoother pipe [9]. Design values [9]:

| Material | c, design |
|---|---|
| Cast and wrought iron | 100 |
| Copper, glass, brass | 140 |
| Cement-lined steel or iron | 140 |
| Epoxy, vinyl ester | 150 |

It is valid for water at ordinary temperatures [9]; use Darcy-Weisbach for other fluids such as oil or coolant [9]. Convert head to pressure with p = h x 9810 N/m³ for water [9]. Different references round the exponents to 1.852 and 4.87, with little effect.

## NPSH available and cavitation margin

Net positive suction head available from a tank is [10]:

NPSHa = p0/(rho g) - pv/(rho g) - (zi - z0) - hf

where p0 is pressure at the liquid surface, pv vapour pressure, zi pump inlet elevation, z0 liquid surface elevation, and hf suction friction loss, all as metres of head. Sea-level atmosphere is about 10 m of water [10]. Vapour pressure depends strongly on temperature [10], so hot liquid has much less margin. Keep NPSHa above the pump's required NPSHr at every operating point, with a margin often about 0.5-1 m depending on fluid and how critical the pump is [11]. NPSHr rises with flow, so a pump far out on its curve may cavitate even if it is safe at design flow [11]. A flooded suction, with the tank level above the pump, raises NPSHa by that height.

## Pump affinity laws

For a given pump at constant impeller diameter, with speed changing from n1 to n2 [12]:

- Flow: Q2 = Q1 x (n2 / n1)
- Head: H2 = H1 x (n2 / n1)²
- Power: P2 = P1 x (n2 / n1)³

With constant speed and small changes in impeller diameter from d1 to d2, the same exponents apply with d2 / d1 [12]. Example: a pump giving 10 l/s and 20 m at 1450 rpm gives about 8 l/s, 12.8 m and 51 percent of the power at 1160 rpm (calculated). The laws predict the pump, not the system, so the operating point still follows the system curve. Static head does not fall with speed, so a large enough speed cut stops the flow in a high-lift system.

## Suction piping and valve placement

Rules for pump suction, from sources citing ANSI/HI 9.8 and Hydraulic Institute guidance:

- Keep at least 5 pipe diameters of straight pipe between any flow-disturbing fitting and the suction nozzle, up to 10 diameters when fluid properties may vary [5].
- Partly open valves count as flow-disturbing; fully open non-disturbing valves are exempt from the 5D rule [5].
- Make the suction line one or two sizes larger than the pump inlet, never smaller [5].
- Use an eccentric reducer on horizontal suction to prevent air pockets, flat side up when the source is below the pump and flat side down when above [5][4].
- Replace a short elbow near the nozzle with a long radius elbow [4].
- Support piping independently so no load reaches the pump nozzles [5].

A strainer adds pressure drop and can contribute to cavitation [5]. Discharge valve order was not verified from an open source.

## Applying this in Sense Sim

Part diameters are outer diameters in metres: for DN 50 pass `diameter_m` 0.06, not 0.05. The default 0.05 is closer to DN 40. Bore is 0.9 x diameter for flow. Use `plant_build` `recipe` "pump_loop" with `diameter_m`, `tank_diameter_m`, `height_m`, `x`, `z`, `yaw_deg`, or build by hand: `plant_add_part` `kind` "pump", "tank", "valve", "reducer" (`diameter_m` to `diameter2_m`), then `plant_route_pipe` `from` and `to` "object.port". The pump's `plinth_m` raises its suction to a tank nozzle. Throttle with `plant_set` `valve_open`, set `fill`, `medium`, `rpm`, then `plant_run` and read flow and head from the report; compute v from flow and 0.9 x diameter to check the velocity table. Generated valves are ball valves and there is no check valve part, so do not claim one exists. Run `verify_scene` for open pipe ends.

## Sources

1. Nominal Pipe Size — Wikipedia — https://en.wikipedia.org/wiki/Nominal_Pipe_Size
2. Water flow velocity in pipes — Engineering ToolBox — https://www.engineeringtoolbox.com/flow-velocity-water-pipes-d_385.html
3. Pump suction flow velocity, water — Engineering ToolBox — https://www.engineeringtoolbox.com/pump-suction-flow-velocity-water-d_228.html
4. Eight Tips to Boost Pump Piping Efficiency — Hydraulic Institute — https://www.pumps.org/eight-tips-to-boost-pump-piping-efficiency/
5. Pumps suction piping: eccentric reducers and straight lengths — The Process Piping — https://www.theprocesspiping.com/pumps-suction-piping-eccentric-reducers-straight-lengths
6. Darcy-Weisbach equation — Engineering ToolBox — https://www.engineeringtoolbox.com/darcy-weisbach-equation-d_646.html
7. Major loss in ducts and tubes — Engineering ToolBox — https://www.engineeringtoolbox.com/major-loss-ducts-tubes-d_459.html
8. Darcy friction factor formulae — Wikipedia — https://en.wikipedia.org/wiki/Darcy_friction_factor_formulae
9. Hazen-Williams equation — Engineering ToolBox — https://engineeringtoolbox.com/william-hazens-equation-d_645.html
10. Net positive suction head — Wikipedia — https://en.wikipedia.org/wiki/Net_positive_suction_head
11. NPSH: net positive suction head — Fabrico — https://www.fabrico.io/blog/npsh-net-positive-suction-head/
12. Affinity laws — Engineering ToolBox — https://www.engineeringtoolbox.com/affinity-laws-d_408.html
