---
title: Antenna simulation practice, openEMS FDTD and MoM wire rules
domain: antenna
tags: FDTD, openEMS, CSXCAD, mesh, cells per wavelength, Yee, Courant, CFL, timestep, Rennings, PML, PML_8, MUR, absorbing boundary, NF2FF, near field to far field, lumped port, Gaussian excitation, f0, fc, end criteria, energy decay, convergence, one third two thirds rule, thirds rule, MoM, method of moments, NEC, NEC2, wire segments, extended kernel, RWG
aliases: mô phỏng anten, lưới FDTD, kích thước ô lưới, điều kiện Courant, biên PML, biên hấp thụ, hội tụ lưới, phương pháp moment, phân đoạn dây, simulation diverges, blow up, simulation too slow, results change with mesh
verified: 2026-11-02
---
# Antenna simulation practice, openEMS FDTD and MoM wire rules

## FDTD maximum cell size versus wavelength

openEMS documentation gives a rule of thumb: keep cell size at or below lambda/10 at the highest simulated frequency, with lambda = c0 / (fmax * sqrt(er * mur)) inside the material [1]. An older openEMS wiki page says about lambda/15 at most, lambda/20 or finer along metal, and lambda/10 in a substrate as an example [2]. The openEMS patch antenna tutorial uses lambda/20 at f0 + fc [3].

Worked example, lambda/20 at 1.5 f0, which is Sense Sim RTLS Normal quality with a sweep of 0.5 f0 to 1.5 f0:

| f0 | In air | In er 4.4 |
|---|---|---|
| 915 MHz | 10.92 mm | 5.21 mm |
| 2.45 GHz | 4.08 mm | 1.94 mm |
| 6.5 GHz | 1.54 mm | 0.73 mm |
| 7.99 GHz | 1.25 mm | 0.60 mm |

Wavelength is only the upper bound. Geometry governs too: place at least a few cells across important shapes such as traces, and use finer cells around small features only [1]. A substrate needs several cells through its height: the tutorial uses 4 across 1.524 mm [3].

## Mesh grading and the one-third two-thirds rule

Neighbouring cells in an FDTD mesh should differ by no more than a factor of 1.5 per the openEMS mesh guide [1]; the older wiki allowed about 2 [2], and the tutorial uses SmoothMeshLines at 1.4 [3]. Smoothing helpers do not guarantee the ratio, so check the actual mesh [1].

Metal edge rule: FDTD field singularities at a metal edge are poorly resolved if a mesh line lies exactly on the edge. Recommended practice places lines so that 1/3 of a cell lies on the metal and 2/3 lies outside, which improves microstrip impedance and propagation accuracy [1][2]. A cell width of res/1.5 between the two lines keeps smoothing from subdividing it [1].

Worked example: a 3 mm microstrip trace with res = 0.3 mm has highres 0.2 mm: lines at 0.067 mm inside and 0.133 mm outside each edge. This is more accurate than a line on the edge, but it costs small cells and therefore a small timestep. Use it where line impedance or a narrow gap governs the result, and compare with an edge-aligned mesh to see whether it changes the answer.

## Zero-thickness metal and port placement in openEMS

Thin structures have strict mesh requirements in openEMS. Zero-thickness metal (AddMetal) and thin conducting sheets must sit exactly on a mesh line, or the simulator ignores them [1]. Ports must be either one-dimensional and on a mesh line, or two-dimensional spanning one full cell on each axis [1]. A port from -8 to 8 with a line at -8 is valid; one from -8 to -7.9 is not [4]. The solver warns that such primitives are unused, so read the log after a change to geometry or mesh.

A lumped port measures voltage and current at its own position, so reference impedance and location set S11 directly. Reference to the true feed: the tutorial uses a 50 ohm lumped port 2 mm wide through the 1.524 mm substrate, at a feed point 5.5 mm off centre [3]. A very narrow port or feed gap adds a parasitic not in the real structure, while a port far wider than a real probe under-represents probe inductance. Choose a gap comparable to the real feed and check port size sensitivity on any design relying on a tight match.

## Courant timestep and why small cells get slow

Explicit FDTD stability requires dt <= 1 / (v * sqrt(1/dx^2 + 1/dy^2 + 1/dz^2)) (the CFL limit), with the smallest cell setting the timestep regardless of signal frequency [1]. openEMS uses the Rennings2 criterion by default, and the timestep is calculated from mesh and materials automatically [1][4].

Worked example in vacuum: a 1 mm cube gives 1.93 ps. Making one axis 0.1 mm with the other two at 1 mm gives 0.33 ps, about six times more steps. Run time scales as cells times steps, so halving all cell sizes costs about 16 times.

A large mismatch between feature size and wavelength makes explicit FDTD impractical; for example, a 10 cm board at 1 MHz [1]. Wire antennas with very thin features at low frequency are usually better in MoM. To reduce the cost, remove unnecessary fine lines, avoid very thin gaps that set the step, and use graded mesh rather than uniform fine cells. Do not override the timestep upward: SetTimeStep can exceed the stability limit and is meant only for engine verification [4].

## PML and MUR boundary clearance

openEMS boundary options are PEC, PMC, MUR and PML_x, where x is 6 to 20 and PML_8 is the default [5]. PML is the most effective absorber and the most expensive. It occupies the nearest x mesh lines inside the box, with a true PEC outer edge [5]. MUR is cheaper and works best for near-normal incidence at the assumed phase velocity (c0 by default) [5]. The openEMS patch tutorial uses MUR on all six faces [3].

Clearance: keep radiating structures about lambda/4 from a PML boundary [5]; the wiki says more than lambda_max/4 [2], where lambda_max belongs to the lowest simulated frequency. Fringe and evanescent fields reaching a PML can destabilise it and cause blow-up [5][4]. Validity test: move the boundary further out in a second run; if the result does not change, clearance was sufficient [5]. Use PML only on radiating faces for efficiency, with MUR elsewhere [5]. Reflecting boundaries remove fringe fields beyond them, which can alter open structures [5].

## NF2FF box placement and far-field results

The near-to-far-field (NF2FF) box must enclose all structures and must not lie inside the PML [6]. It must be defined before the run, because the calculation uses recorded fields [6]. In the openEMS tutorial, the box is the simulation box reduced by 4 cells [3]. Pass frequencies of interest to save disk, otherwise time-domain data is stored [6]. Set the phase centre to the antenna position, and never outside the box [6]; default is the origin [6]. In Python, finalise mesh and boundaries before creating the box [7].

Practice: do not change the box faces on Cartesian meshes [6]. Far-field gain and efficiency are valid only if the box sees all radiation. The NF2FF page does not describe combining a PEC ground boundary with the box, so validate that configuration against a known case such as an ideal monopole at about 5.16 dB. Patterns are calculated only for requested frequencies, so re-run at each band edge if pattern versus frequency matters. Check that efficiency is plausible (under 100%, and consistent with loss); a value above 100% indicates a setup error.

## Gaussian excitation bandwidth and sweep choice

openEMS Gaussian excitation takes a centre f0 and an fc described as the -20 dB bandwidth [7]. The tutorial uses f0 = 2 GHz, fc = 1 GHz and analyses 1-3 GHz [3]. A dc_free option removes the DC component so the pulse leaves no static charge [7].

Guidance: the sweep sets fmax, which sets mesh and cost, so choose the narrowest band that shows the resonance plus some margin. A narrow fc lengthens the pulse in time, and a very wide one forces a finer mesh. Results near the pulse edges have less excitation, 20 dB down, so keep important frequencies inside them. A low sweep edge also increases simulation time and the boundary distance needed. If you see a strange ripple across S11, suspect short simulation time (truncation) before suspecting the antenna.

A wider sweep raises fmax and lowers fmin, so it both shrinks cells and enlarges the required boundary clearance. Request far fields only where needed.

## Energy decay end criteria

openEMS stops when total energy has decayed by the end criterion, or at the maximum timesteps. Documentation describes this in slightly different terms: 60 dB as a good default in the simulation guide [4], and "below 1e-4 recommended, default 1e-5" in the Python API [7]. As energy, 1e-4 is -40 dB, 1e-5 is -50 dB and 1e-6 is -60 dB [7].

The energy shown is a rough indicator that ignores material properties, and it can rise and fall during a run [4]. High-Q low-loss structures converge notoriously slowly, because injected energy has nowhere to go without terminations or absorbers [4].

Guidance: -40 dB can be adequate for a low-Q dipole, wideband monopole or lossy FR-4 structure. For narrow patches, small antennas or filters, repeat at -50 or -60 dB and compare. If the run ended on max timesteps instead of energy, S11 may show ripples because of truncated ringing. Energy growing rapidly is a divergence: stop the run, and first suspect the PML [4]. Coarse mesh and odd permittivity cause inaccuracy, not instability [4].

## Mesh convergence procedure

A single simulation is not a result until it is shown stable against mesh and boundaries. Procedure:

1. Run at the default density, then repeat with cells per wavelength raised by about 1.5 times, and compare resonance frequency, bandwidth and gain.
2. Compare against the design margin. Decide the tolerance from the requirement, such as a fraction of the operating band, not an absolute percentage.
3. If the result moves, refine the features with the highest fields (feed, slot, edge) before refining everywhere, because snapped metal edges can shift by up to half a cell.
4. Move the boundaries outward once and confirm no change [5].
5. Tighten the end criterion once and confirm no change [4].

Worked example: Sense Sim RTLS FDTD offers Fast (12 cells per lambda), Normal (20) and Fine (30). Compare Normal against Fine on a narrow design. A 2.45 GHz FR-4 patch has only about 27 MHz of bandwidth (estimate), so a mesh-induced shift of 1% (24.5 MHz) is about the entire band, and an unconverged mesh can give a false pass or fail.

## MoM wire segment rules from NEC

NEC-2 modelling guidelines for thin-wire method of moments [8][9]:

| Quantity | Rule |
|---|---|
| Segment length | Below about 0.1 lambda; about 0.05 lambda in critical regions; avoid below about 1e-3 lambda |
| Wire radius | 2*pi*a/lambda much less than 1 |
| Delta/a, thin-wire kernel | Above about 8 for under 1% error; reasonable currents down to about 2 |
| Delta/a, extended kernel | Down to about 2 at the same accuracy; reasonable currents to 0.5 |
| Bends | Extended kernel not used; avoid small Delta/a |
| Junctions | No more than 30 wires; ends must coincide to within 1e-3 of shortest segment |
| Radius changes | Step gradually over several segments |
| Ground | Perfect ground: h well above radius, several radii minimum |

Worked example, 24 segments per wavelength gives Delta = 0.0417 lambda, meeting the rule. Radius 0.001 lambda has Delta/a 41.7; 0.002 lambda has 20.8; and 0.005 lambda has 8.3, at the 1% limit. Keep each source on its own segment, with matching neighbours [8]. Avoid abrupt changes in segment length [9]. Tightly spaced parallel wires (folded dipoles, meander) need aligned segments and spacing well above radius (rule of thumb).

## Choosing FDTD or MoM for an antenna

Choose MoM for wire antennas and purely conducting surfaces: only metal needs meshing, free space is exact, and frequency sampling is direct. Choose FDTD where dielectric volumes are present (substrate, radome, nearby materials), because full volume modelling handles them naturally, and when you want a broadband result from one run. The openEMS example log shows 181,300 cells at about 111 MCells/s running 14,748 iterations in 24 s, for a sense of scale [4].

Accuracy trade-offs: FDTD uses rectilinear staircasing, so slanted or round features need fine cells. Thin wires in FDTD are costly, because a wire smaller than a cell is not resolved. MoM with thin-wire assumptions struggles with fat wires, close bends, and dielectric. If both solvers apply, comparing them is a strong check: agreement in resonance within a fraction of the bandwidth suggests a trustworthy model. Disagreement usually points to feed modelling. Report which solver gave a stated number. Do not mix a free-space MoM result with the on-board result for an FR-4 printed design.

## Applying this in Sense Sim

Studio automatic solver choice uses wire MoM if all solids are conducting wires, surface MoM if all are conducting sheets, and FDTD otherwise. Defaults at the time of writing are 20 FDTD cells per wavelength at the top of the sweep, a 40 dB energy-decay stop, 24 wire segments per wavelength and 12 surface edges per wavelength. Geometry can use a PEC bottom boundary for infinite ground; other faces are PML_8. These defaults may change while Studio is under development, so check current settings with antenna_get. antenna_set exposes density (cells_per_lambda, segments_per_lambda, edges_per_lambda), the sweep, solver and far-field frequencies. The end criterion is not exposed there.

The mesh keeps metal edges on mesh lines, grows at 1.4, and pads by lambda/4 at design frequency plus four cells before the PML. With very wide sweeps, that is less than lambda_max/4 at the lowest frequency, so results near the low sweep edge need a clearance check. Read the run log, confirm a decay-based stop, and treat single-run numbers as estimates until convergence is checked.

## Sources

1. Meshing concepts — openEMS documentation, accessed 2026 — https://docs.openems.de/en/latest/concepts/mesh.html
2. FDTD Mesh — openEMS wiki, accessed 2026 — https://wiki.openems.de/index.php/FDTD_Mesh.html
3. Tutorial: Simple Patch Antenna — openEMS wiki, 2017 — https://wiki.openems.de/index.php/Tutorial:_Simple_Patch_Antenna.html
4. Simulation concepts — openEMS documentation, accessed 2026 — https://docs.openems.de/en/latest/concepts/simulation.html
5. Boundary conditions — openEMS documentation, accessed 2026 — https://docs.openems.de/en/latest/_sources/concepts/bc.rst.txt
6. NF2FF — openEMS wiki, accessed 2026 — https://wiki.openems.de/index.php/NF2FF.html
7. openEMS Python interface — openEMS documentation, accessed 2026 — https://docs.openems.de/en/latest/python/openEMS/openEMS.html
8. NEC-2 Part III, Section II: Structure Modeling Guidelines — nec2.org, accessed 2026 — https://nec2.org/part_3/secii.html
9. The Numerical Electromagnetics Code (NEC-2), Ishii and Baba — IEEE PES Lightning Performance tutorial, undated — https://ewh.ieee.org/soc/pes/lpdl/archive/nectut.pdf
