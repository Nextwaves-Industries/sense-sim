---
title: UHF RFID tag structures, and what surroundings do to them
domain: rfid
tags: tag tuning, tag metrics, loop size, meander length, quarter wave, conjugate match, chip impedance, tag structure, inlay structure, loop coupled tag, inductive coupling, dogbone tag, bow-tie tag, slot tag, slotted plate, spiral tag, fractal tag, Hilbert curve, dual band tag, detuning, label, paper label, face stock, PET, adhesive, carton, glass, on-metal tag, retune, read range
aliases: metric của tag là gì, chỉnh vòng loop, đường kính loop, độ dài meander, hòa hợp trở kháng với chip, chỉnh tần số tag, what are the metrics of a tag, how do I tune a tag to 902 MHz, inlay dưới giấy, nhãn giấy RFID, thẻ bị lệch tần số, thẻ dán lên hộp, thiết kế thẻ RFID mới, which tag structure should I use, why did my tag stop reading under paper, how much does paper detune a tag, what changes when I put an inlay under a label
verified: 2026-11-02
---
# UHF RFID tag structures, and what surroundings do to them

## There is no one tag structure
A tag needs inductance to resonate with the chip and an aperture to collect power. Several structures provide both, and they trade size, bandwidth, loss and tolerance differently, so compare them for the job instead of adjusting the first one found. Inductive coupling and loop-coupled small antennas are established designs [1]. Each structure below is modelled in this app, and these descriptions are how they behave there, not measurements.

| Structure | How it matches | Good for |
|---|---|---|
| T-match meander dipole | A T of trace transforms the dipole's impedance [2] | A long narrow label, tuned the familiar way |
| Coupled loop and radiator | The chip's own loop sets reactance, and its distance from a separate radiator sets resistance [1] | Freedom to retune each independently |
| Loop and two radiators | Two couplings make two resonances from one chip | Two regions or a wide band |
| Dogbone | T-match with wide arms and end plates | Range when there is height to spare |
| Bow-tie with a hairpin | A shunt inductor across a low-Q flared dipole | Bandwidth, and changing surroundings |
| Slotted plate | A window and slit cut in solid metal | Low loss, simple printing, a spacer over metal |
| Square spiral | Arms wound round a shunt hairpin | Square labels, at a cost in gain and loss |
| Fractal arms | Space-filling arms with a T-match | The most wire in the least area, at a cost in loss |
| Bent arms | Arms turned at right angles | Fewer deep nulls across orientations |

## The two things to tune
A tag with a loop and a radiator comes down to two settings. Each chip has its own impedance, so the loop's length and width are sized to give the antenna the conjugate of it: the loop's inductance cancels the chip's capacitance [1][5]. The long line outside the loop sets where the radiator resonates, and a quarter wavelength per arm, with wavelength = c / f, is the place to start: 83 mm at 902 MHz. Hitting both means the antenna is the chip's conjugate at the target frequency, which also makes that frequency the peak of the power into the chip.

The split isn't clean, and the quarter wave isn't the answer. In this app's solver, on a T-match meander tuned to 902 MHz for a UCODE 9, one more millimetre of loop height adds 25 ohm of reactance and 2 ohm of resistance, and one more millimetre of meander height adds 28 ohm and 14 ohm. So the loop mostly sets reactance while the line also sets resistance, and both move the peak by about 20 MHz per millimetre: change one and check the other. On a coupled loop with end plates, the line moves the peak by only 4 MHz per millimetre against 25 for the loop. An arm came to 114 mm along its metal on that meander, 1.37 quarter waves, 104 mm under a paper label, and 77 mm on the coupled loop with its end plates. A line that can't reach resonance in the footprint leaves too little resistance, which no loop can fix: 50 by 20 mm came to 27 % of the power at best with this structure.

## Why paper moves a tag down in frequency
Anything with permittivity near the metal adds capacitance, and the near field is strongest right against the trace, so a very thin layer counts for more than its share of the volume. Paper is around er 3.2 to 3.4, with a loss tangent of 0.05 to 0.08 [3][4], and it varies by grade, so measure the stock for a design that has to be right. In this app's solver, a half-wave strip dipole of 2 mm trace resonating at 948 MHz moves down by about 2 % under 80 um of that paper, about 6 % under 300 um and about 12 % under 1 mm. A tag's chip Q is about 15, so a 2 % shift alone costs real range on a narrow band, and it adds to what the film and glue do. Thin layers hardly change the radiation pattern, so this is a tuning problem and a small retune gets most of it back.

## What to do about surroundings
Design for the real build-up, not free space: the chip, the film, the glue, the face stock and what it is stuck to. Small changes to a dimension tune a design back into the band, and a design that has to survive a range of products, such as cartons and plastic, should be chosen by what it does across that range rather than only at its best. High-permittivity lossy contents such as water, and metal, are a different problem: they change the resistance and absorb power, so they need a different structure or a spacer rather than a retune.

## Applying this in Sense Sim
antenna_tag_design searches every structure above against a chip, band, footprint and surroundings, and gives options with their read range and what each does if the surroundings change. antenna_draw draws any new shape with its dimensions as variables, and vary tests it against the others; evolve lays out metal cell by cell with no assumed shape. antenna_tag_retune moves an existing design into new surroundings with small changes. antenna_tag_tune sets the loop and the line for an exact frequency in seconds, reports both, and says what a millimetre of each is worth. Layers are as wide as the world in that solver, and an FDTD run builds them to size. All of it is simulated, not measured.

## Sources
1. RFID Tags for On-Metal Applications: A Brief Survey — MDPI Technologies 12(5), 58 — https://www.mdpi.com/2227-7080/12/5/58
2. UHF RFID PCB antenna design, rev. 3.0 — NXP AN1715 — https://www.nxp.com/docs/en/application-note/AN171530.pdf
3. Inkjet-Printed RF Energy Harvesting and Wireless Power Transmission Devices on Paper Substrate, EuMW 2013 — Kim, Vyas, Georgiadis, Collado, Tentzeris — https://tentzeris.ece.gatech.edu/EuMW13_EH.pdf
4. A Novel Green Inkjet-Printed Z-Shaped Monopole Antenna for RFID Applications, EuCAP 2009 — Konstas, Rida, Vyas, Katsibas, Uzunoglu, Tentzeris — https://tentzeris.ece.gatech.edu/EuCAP2009_Zissis.pdf
5. The art of UHF RFID antenna design: impedance-matching and size-reduction techniques — Marrocco, IEEE Antennas and Propagation Magazine 50(1), 66-79, 2008, doi:10.1109/MAP.2008.4494504 — https://art.torvergata.it/handle/2108/31667
