---
title: PCB and chip antennas, ground clearance, placement and enclosure retuning
domain: antenna
tags: chip antenna, ceramic antenna, PCB antenna, trace antenna, embedded antenna, IFA, meander, keep-out, no-ground area, ground clearance, ground plane size, board edge, board corner, via stitching, battery, LCD, screw, shield can, metal nearby, plastic enclosure, housing, detuning, carbon fibre, metallic paint, pi network, retuning, reference design, BLE, Wi-Fi, 868 MHz, 915 MHz, 2.4 GHz
aliases: anten chip, anten gốm, anten mạch in, anten trên PCB, vùng cấm đồng, vùng không đồng, khoảng trống mặt đất, kích thước mặt đất, đặt anten ở góc bo mạch, pin gần anten, vỏ nhựa làm lệch tần số, tinh chỉnh lại anten, mạch pi, my antenna works on the eval board but not on my board, how much keep-out do I need, why did my range drop after adding the case
verified: 2026-11-02
---
# PCB and chip antennas, ground clearance, placement and enclosure retuning

## Why a reference antenna does not copy across boards

On a small device the board ground is part of the radiator. Most chip antennas are monopole-type and use the top-layer ground as the other half of the dipole, which is why that layer is critical [1]. TI says the impedance of its tuned inverted-F changes with ground size and shape, housing, batteries, PCB thickness and PCB material [2]. Its 868 MHz note adds that the size and shape of the ground also change the radiation pattern [3].

Antenova characterises its Rufa 2.4 GHz part on a 50 x 20 mm evaluation board: 2.1 dBi peak gain, -1.2 dBi average gain, 75% average efficiency and -11 dB maximum return loss [4]. Those numbers belong to that board. Treat any datasheet figure the same way.

Consequence: a layout copied from a reference design is a good start, not a finished design. Plan matching footprints from day one, and retune on the fully populated board with its enclosure [1].

## Ground plane size and low-frequency efficiency

Ground size and frequency set what a device antenna can do. TI measured two kit boards with identical board size and antenna area: 91% efficiency when tuned to 868 MHz, 7% when tuned to 169 MHz. Raising the 169 MHz figure needs a larger antenna and a larger ground [5]. In a separate dual-band design, TI reports 84.1% at 2440 MHz and 25.3% at 510 MHz on one antenna [6].

Worked example: 7% is -11.5 dB, 25.3% is -6.0 dB, and 91% is -0.4 dB. Going from 868 MHz to 169 MHz gains 14.2 dB of free-space path loss, but this antenna gives back 11.1 dB.

Johanson links ground size to range: a very short link can use minimal ground, whereas a longer link, around 30 m in its example, needs a large continuous ground [1]. It lists a broken ground pour, poor via stitching and a very small board as common causes of bad patterns [1]. When the longest ground dimension falls far below a quarter wavelength (82 mm at 915 MHz), expect low efficiency and a strong dependence on cables (rule of thumb).

## Keep-out rules for chip and trace antennas

The clear area under and around the antenna matters as much as the antenna itself. Follow the part datasheet, since these figures are part-specific.

| Rule | Source |
|---|---|
| No-ground area extends through every layer of the board | Johanson [1] |
| Other ground and components at least 2 mm from the antenna, for Antenova's Rufa | Antenova [4] |
| Ground must not extend underneath the antenna | Antenova [4] |
| Ground, power and signal planes vacant directly below the antenna | Digi [7] |
| Metal chassis at least 25.4 mm from the antenna, for Digi's PCB antenna modules | Digi [7] |
| Avoid components or ground near the end points of the inverted-F | TI [2] |
| Antenna must not be surrounded by ground | Microchip [8] |

An exception exists: Johanson sells a chip that mounts directly over or under a metal layer and needs no clearance [1]. Changing a specified no-ground area can still work, but performance is less predictable and less efficient [1]. An inner ground layer that intrudes on the clearance is a listed defect and causes mismatch [1], so check all layers in the Gerber files, not just the top.

## Placement on the board

Place the antenna at the board edge [4, 7]. Antenova states that maximum performance comes from placing the antenna towards a corner, with the feed as close to that corner as possible, and with clearance ideally in five spatial directions [4]. Johanson publishes separate layouts for end, corner, centre-edge and circular placements, so the matching and pattern differ for each [1].

Practical order of preference (rule of thumb where uncited):

| Position | Effect |
|---|---|
| Corner, antenna extending clear of ground | Usually the best start [4] |
| Centre of a short edge | Valid with a matching layout, more symmetric ground |
| Centre of a long edge | Uses a different ground mode, so retune |
| Board interior surrounded by copper | Avoid: an antenna must not be surrounded by ground [8] |

Keep the feed trace a controlled 50 ohm line, and stitch the ground on both sides with vias. Johanson treats the feed line and surrounding ground as part of the resonant system [1]. TI notes that a PCB wider than the antenna must keep components and ground away from the antenna sides [3].

## Batteries, screws, displays and metal near the antenna

Common causes of poor patterns are tall conductors next to the no-ground area, such as batteries, shields and metal screws, and LCDs, buttons or other conductors directly above or beside the chip [1]. In one Johanson case, a replaceable battery sitting directly over or under the antenna disrupted both gain and pattern through its reflection [1].

For a metal chassis, keep the distance well beyond a few millimetres; Digi states at least 25.4 mm for its modules [7] and warns that metal enclosures may greatly reduce range, recommending an external antenna instead [7].

Design rules (rule of thumb): put the battery and shield can at the far end of the board from the antenna, route cables away from the antenna end, and tie shields to ground at several points so that they do not become floating resonators. Treat any conductor over about a tenth of a wavelength long as part of the antenna, and include it in the model.

## Enclosure detuning: how far it moves

A plastic case or a hand adds permittivity and lowers the resonant frequency [5]. TI measurements of one narrowband 868 MHz antenna, a Pulse W3013:

| Condition | Resonance | SWR at 868 MHz | Note |
|---|---|---|---|
| Free space | 868 MHz | Matched | |
| In plastic case, 1 mm away | 862.875 MHz | 2.53 | About 18% power lost [9] |
| In case, in hand | 847 MHz | 6.61 | About 50% power lost [9] |

Worked example: those shifts are -0.6% and -2.4%. A compact 2.4 GHz PCB antenna in the same TI note went from SWR 1.70 in free space to 3.68 when cased and in hand [9]. TI states that detuning cannot be predicted reliably, so measure it [9].

A bandwidth wider than the shift protects you: TI notes that its 300 MHz IFA is less sensitive to plastic and nearby objects for that reason [2]. A cased antenna with only 1% bandwidth will fall out of band, whereas a 12% IFA stays in. For on-body testing, TI suggests a 0.33 litre water bottle taped to the device as an equivalent to the body [9].

## Enclosure materials to avoid

Plastic mostly detunes, but some materials absorb or block. Johanson reports low gain from conductive enclosures, from metal-loaded or shiny paint, and from carbon fibre, whose loss caused very low gain in a customer design [1]. These do not appear as mismatch at first, so a good S11 does not clear them. Measure radiated power.

The level of detuning depends on the plastic's chemistry and carbon level [9], so use the production resin and colour for a tuning sample. Keep antenna-to-wall distance uniform, because varying spacing across a production tolerance becomes varying detuning.

Thin coatings and labels near the antenna count as well. Treat any late change of colour or paint as a reason to re-measure. If a metallic look is required, ask for a non-conductive vacuum metallisation process, and confirm with a measurement (rule of thumb).

## Retuning practice with a pi network

Fit a pi network footprint near the feed point, because ground geometry changes impedance and plastic detunes it [2, 3]. Chip antennas are typically matched with one to three parts [1]. Johanson's procedure, in brief [1]:

1. Populate the final board, and fit it in the enclosure.
2. Cut the feed trace to isolate the antenna from the radio.
3. Solder a semi-rigid cable to the antenna side, leaving by the end opposite the antenna.
4. Calibrate open, short and load across the band.
5. Measure with the antenna absent, then present, and match the trace to 50 ohm.

TI advises starting with one main component, because this shows how fast the marker moves with value [9]. Some networks are sensitive: small-value parts move the marker quickly, so production tolerance moves impedance a lot [9]. Prefer a network whose response stays flat when a part changes by one E-series step.

Change copper before parts when you can. Length moves frequency: shorter raises it and longer lowers it, so make the prototype slightly long [9]. Matching parts cannot recover low efficiency, and each adds its own loss.

## Applying this in Sense Sim

Model the board first, then the surroundings. Start from ifa_pcb, pifa or meander at the design frequency, and use antenna_set to set the true ground dimensions (Gy and Bx on ifa_pcb) and laminate. Use antenna_sweep on ground size to see how sensitive the design is before choosing a final board.

Use design_integrate to test an enclosure, battery or metal part: parts are boxes, a shell wraps the antenna in plastic, and plastic requires a build with FDTD. It reports the effect and proposes a matching network or a dimension retune. Then use antenna_match to size the pi network; its result states loss in the parts, which you should report. For an existing outline use antenna_import_cad rather than an idealised rectangle.

There is no chip antenna template, so a chip's S-parameter behaviour is not simulated; use the vendor's data and measure. Note that antenna_assess judges only what is modelled: a clean verdict on a bare board is not a verdict on the cased product.

## Sources

1. Understanding Chip Antennas Handbook — Johanson Technology, 2023 — https://www.johansontechnology.com/docs/4565/johanson-antenna-handbook.pdf
2. 2.4-GHz Inverted F Antenna, SWRU120D — Texas Instruments, 2019 — https://ti.com/lit/swru120
3. DN024, 868/915/920 MHz PCB antenna, SWRA227E — Texas Instruments, undated — https://ti.com/lit/pdf/SWRA227
4. Rufa 2.4 GHz SMD Antenna Datasheet — Antenova, undated — https://www.tme.com/Document/83089ef37187ae8744d8cf1759e5bb2e/Rufa.pdf
5. Antenna Selection Guide, AN058 / SWRA161B — Texas Instruments, undated — https://ti.com/lit/swra161
6. SWRA730 Application Note, dual-band PCB antenna section — Texas Instruments, undated — https://www.ti.com/document-viewer/lit/html/SWRA730/GUID-E2C4847B-FF4F-4DB4-AEAB-23D546313AB2
7. Design notes for PCB antenna devices — Digi International, accessed 2026 — https://docs.digi.com/resources/documentation/Digidocs/90002002/Reference/r_design_notes_pcb.htm
8. Antenna Placement Rule, module hardware design guide — Microchip Technology, accessed 2026 — https://onlinedocs.microchip.com/oxy/GUID-B47CF6CB-944F-4819-B134-F4A713917697-en-US-1/GUID-C63A971D-3C9A-420C-AF49-AA1D518BA9DC.html
9. Antenna Impedance Measurement and Matching, SWRA726 — Texas Instruments, 2022 — https://ti.com/lit/swra726
