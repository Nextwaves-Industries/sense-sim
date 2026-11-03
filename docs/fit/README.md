# The built-in finite-difference solver

`crates/fdtd_kernel` is a finite-difference time-domain field solver in Rust, written here, running on
CPU threads or on a GPU through wgpu. Written in voltages on a Cartesian grid, as here, finite differences
and the finite integration technique are the same equations. `crates/antenna_core/src/fit.rs` connects it
to the app's geometry and results. On the Setup page it's the solver called "FDM time domain (built in)",
and the assistant's `antenna_set` calls it `fdm`.

It was written to replace openEMS. openEMS is GPL-3.0, so a fork couldn't ship inside the app without
putting the app under the GPL, and nothing here was taken from its source.

**What it is not.** It isn't HFSS or CST. A Cartesian time-domain solver is the same method class as CST's
time-domain solver, so that's the fair comparison, and only on the classes of antenna tested below.
There is no FEM, MLFMM or SBR solver here. It has been held against two antennas someone else built and
measured, below, but not against one built for this project: [measure-tags.md](measure-tags.md) has that
step, which needs hardware.

## What runs on it

Everything in the app that needs a finite-difference run, with nothing to install:

- **Antenna Studio**, on either of two routes. Left on Auto, or with this solver chosen, it plans its own
  mesh (`fit::simulate`), which needs fewer cells. With openEMS named it runs openEMS's own model.
- **Everything that writes an openEMS model**: the RTLS antenna, through-object and whole-scene runs, the
  RFID gate's aisle field, SAR and the hardware recipes. `fit/csxrun.rs` reads that same XML and writes the
  same files (`port_ut1`, `port_it1`, `<name>_f=<Hz>_abs.vtr`, `_arg`, `_p=NNN`), so the readers don't know
  which ran.

openEMS stays as a second opinion. It's used when it's installed and either named as the solver or chosen
on the field-solver card; `SENSESIM_FDTD=builtin` or `openems` overrides that for a run. Named but not
installed, the run goes on the built-in solver and says so.

Every benchmark that used to need openEMS passes with `SENSESIM_FDTD=builtin`:

| Benchmark | Result | Time |
|---|---|---|
| Strip dipole | D 2.152 dBi, X = 0 at 999.1 MHz, efficiency 1.0045 | 4 s |
| Monopole on ground | D 5.164 dBi, efficiency 1.0048 | 4 s |
| Patch on FR-4 | 2.442 GHz at -25 dB, D 5.66, efficiency 0.454 | 16 s |
| Two ports | reciprocal | 31 s |
| Dipole against MoM | 2.30 %, then 0.92 %, apart at 24 and 36 cells/λ | 12 s |
| RFID gate against MoM | S11 -11.1 / -12.5 dB, S21 -24.3 / -24.1 dB; read fraction within 0.02 | 23 s |
| Enclosure on a dipole (hardware recipe) | moves the resonance -8.3 % | 9 s |
| Wall S21, 5.4 M cells | 6.4 dB through the wall | 34 s |
| Whole scene, 6.7 M cells | Friis to -0.0 dB in line of sight, 7.6 dB behind the wall | 60 s |

The dipole test's reactance limit was openEMS's own first-order figure on that mesh, 74.6+4.3j. This
solver gives 74.06+0.49j there, nearer the converged figure, so each solver now has its own pin to the
same 2 ohms. The physics limits are shared.

## Against antennas that were built

Vasylchenko, Schols, De Raedt and Vandenbosch, "A benchmarking of six software packages for full-wave
analysis of microstrip antennas" (EuCAP 2007), built two inset line-fed patches on 60 mil Rogers and
published what HFSS, CST and four others predicted. Their figures, from its Tables 1 and 2, in GHz at
-10 dB, against `fit/measured_tests.rs` with their stated permittivity, a wave port and PEC:

| Plain RO4003 board | Fmin | Fmax | Fc | BW |
|---|---|---|---|---|
| Measured | 5.115 | 5.22 | 5.168 | 0.105 |
| HFSS | 5.255 | 5.353 | 5.304 | 0.098 |
| CST MWS | 5.213 | 5.326 | 5.27 | 0.113 |
| FEKO | 5.226 | 5.374 | 5.3 | 0.148 |
| Momentum | 5.23 | 5.322 | 5.276 | 0.092 |
| IE3D | 5.29 | 5.4 | 5.345 | 0.11 |
| MAGMAS | 5.305 | 5.423 | 5.364 | 0.118 |
| **This solver**, 20 cells/λ, 1.1 M cells, 27 s | 5.286 | 5.368 | 5.327 | 0.082 |
| **This solver**, 30 cells/λ, 2.4 M cells | 5.262 | 5.359 | 5.311 | 0.097 |
| **This solver**, 45 cells/λ, 5.9 M cells | 5.266 | 5.364 | 5.315 | 0.098 |

| RO3003 under the patch | Fmin | Fmax | Fc | BW |
|---|---|---|---|---|
| Measured | 5.603 | 5.758 | 5.681 | 0.155 |
| HFSS | 5.47 | 5.608 | 5.539 | 0.138 |
| CST MWS | 5.43 | 5.555 | 5.493 | 0.125 |
| FEKO | 5.322 | 5.437 | 5.38 | 0.115 |
| IE3D | 5.47 | 5.611 | 5.541 | 0.141 |
| MAGMAS | 5.55 | 5.68 | 5.615 | 0.13 |
| **This solver**, 30 cells/λ, 2.4 M cells | 5.465 | 5.587 | 5.526 | 0.122 |

So on these two it lands with the commercial solvers: within 0.3 % of HFSS on both boards, and within
1 % of CST. Like all of them it's about 3 % from what was built. How to read that:

- **Those HFSS and CST figures are from 2006 versions, run by the paper's authors, and aren't runs made
  here.** They aren't a parity test on the same mesh and model, and no bar is claimed.
- **The plain board's gap looks like the material, not the solver.** The paper simulates RO4003 at 3.38,
  the datasheet's process figure. With Rogers' design figure of 3.55 this solver gives 5.193 GHz, 0.48 %
  from the measured 5.168. That's one antenna and one change, and it could be a coincidence, but it's what
  Rogers publishes the design figure for, and it's why the library's `RO4003C` uses it.
- **The insert's gap goes the other way**, measured higher than every solver but none of them modelled
  how it was made: a piece fitted into a hole, soldered, likely with air around it. Nothing here can say.
- The paper names the connector, which nobody modelled, as the largest difference.

What the paper doesn't give made no difference: a board margin of 15, 30 or 60 mm gives 5.310, 5.311 and
5.311 GHz, and copper for PEC 5.310. A lumped feed in place of the wave port gives 5.314 GHz at 30 cells/λ,
but a deeper notch, -16.3 dB against -13.4 and a band 9 % wider, because it isn't launching the line's mode.

## The idea that makes it different

The unknowns are grid voltages, so Faraday's and Ampere's laws are exact integer stencils, and all
geometry and material sit in two diagonal matrices. Anything the grid can't resolve goes in those
diagonals, which keeps the update stable for any time step below a limit measured from the real
operator (`stable_dt`), whatever the subcell corrections have done.

- **Thin metal.** Near a sharp edge the field goes as `r^-1/2`, and that causes the first-order error
  that makes a flat trace converge slowly. Across a cell the field is close to the static one, which
  is known in closed form in a cross-section (`edge.rs`), so the true capacitance and inductance of
  those cells are computed from it. The thirds rule is the special case of placing the mesh line where
  no correction is needed.
- **Thin layers.** Paper, PET and glue far thinner than a cell are averaged into it, in series along
  the field and in parallel across it, with no mesh lines through them.
- **Wires.** A wire thinner than a cell uses the same idea with a logarithmic field.
- **Curved and off-grid metal.** Conformal cut cells, with slivers sealed by metal around them.
- **Real metal.** A series R and L on metal edges, implicit in time so a small inductance can't limit
  the step, fitted at the design frequency. Unlike the openEMS path, this also works on metal bodies.

## Measured against exact answers

All in `crates/fdtd_kernel/src/tests.rs` and `tests_canon.rs`.

| Test | Exact | Result |
|---|---|---|
| Zero-thickness stripline, 8 cells across, edge anywhere between lines | Cohn's closed form, 65.354 ohm | worst 0.09 %, plain grid 6.03 % |
| Same, 4 cells across | same | worst 0.36 %, plain grid 11.85 % |
| Rectangular cavity TE101, 15 and 30 cells | closed form | -0.066 %, -0.017 %: second order |
| Walls between mesh lines at 2, 1, 0.5 mm | closed form | +0.143, +0.011, -0.015 % |
| Hollow sphere TM011, 16, 24, 32, 48 cells across | ka = 2.7437 | -2.2, -1.5, -1.1, -0.7 %: **first order** |
| Dielectric layer 0.3 cell thick, across the field | closed form | -0.024 % |
| Same layer, along the field | transverse resonance | shift -6.520 % of -6.543 % |
| Debye-filled cavity | complex root | frequency -0.03 %, decay -0.0 % |
| Lossy dielectric cavity | closed form | frequency -0.017 %, decay +0.00 % |
| Real metal walls, TE101 Q | Pozar | Q 195 against 200.7; frequency shift -0.27 % |
| Absorber, 8 cells | a larger domain | -85 dB |
| Single precision over 400,000 steps, against double | | 2e-8 in frequency, 0.04 % in energy |
| GPU against CPU over a run with real metal and a port | number for number | 0.01 % worst, energy 0.07 % |
| Microstrip wave port, 3 mm on 1.6 mm, er 4.4 | Hammerstad and Jensen | Z0 49.93 against 50.65 ohm; S11 -45 dB |
| Same, nothing drawn behind the ports, on a grid grading there | no reflection | S11 -44 to -47 dB, S21 -0.03 to -0.16 dB |
| Wave ports, GPU against CPU | number for number | differences of 1e-5 |

The sphere is first order, not the second order claimed for CST's PBA in its 1998 paper. That is a
real limit on curved metal.

## Antennas

In `crates/antenna_core/src/fit/bench_tests.rs`, to the same limits as the openEMS benchmarks.

| Test | Reference | Result |
|---|---|---|
| Strip dipole, 20 cells/λ | D 2.15 dBi, Z 74.6+4.3j from openEMS | D 2.139, Z 74.6+1.7j, efficiency 1.004 |
| Monopole on ground, 24 cells/λ | D 5.15 dBi | D 5.168, efficiency 1.005 |
| Patch on FR-4, 20 cells/λ | openEMS limits | 2.442 GHz at -31 dB, D 5.68, efficiency 0.451 |
| Two-port patches | reciprocity | S21 and S12 differ by under 0.005 % |

## Against openEMS and the method of moments

From `study_three_solvers`, on the same models, one after another on a machine shared with other work.
The built-in solver used about twice the threads openEMS was given (openEMS slows when oversubscribed,
as `threads_for` in `src/antenna/run.rs` records), so the times are the solvers as each would run here,
not a per-core comparison. Reactance in ohms at 915 MHz.

| Model | Mesh | Built-in | openEMS | MoM |
|---|---|---|---|---|
| Strip dipole | 12 cells/λ, 0.14 M cells | -41.8, 13 s | -35.6, 13 s | |
| | 20 cells/λ, 0.23 M | -47.6, 16 s | -38.6, 22 s | |
| | 36 cells/λ, 0.6 M | -50.5, 15 s | -43.1, 50 s | -53.1 at 192 edges/λ |
| 30 x 20 mm loop, 2 mm trace, 1.5 mm gap | 24 cells/λ, 1.9 M | +549.9, 31 s | +500.0, 212 s | |
| | 48 cells/λ, 7.4 M | +540.8, 123 s | +517.4, 545 s | +616.3 at 384 edges/λ |

The searched thin-trace tag (0.567 mm trace, 0.763 mm gap), Z at 915 MHz:

| Solver | Mesh | Z | Time |
|---|---|---|---|
| Built-in | 20 cells/λ, 0.35 M | 23.8 + 198.5j | 37 s |
| Built-in | 40 cells/λ, 0.89 M | 28.4 + 197.1j | 56 s |
| Built-in | 80 cells/λ, 3.25 M | 28.5 + 193.7j | 314 s |
| MoM | 192 edges/λ | 24.7 + 209.9j | |
| MoM, from earlier notes | 768 edges/λ | 27 + 203j | |
| openEMS, from earlier notes | 110 cells/λ | 37 + 184j | about 46 minutes |

The built-in solver has moved 2 % in reactance from 20 to 80 cells/λ. It sits between the other two, at
this gap, as the feed explanation below would have it.

## GPU

| Cells | GPU, RTX A2000 12 GB | CPU, 56 threads of a Xeon w9-3495X |
|---|---|---|
| 1 M | 531 MC/s | 571 MC/s |
| 8 M | 760 MC/s | 736 MC/s |
| 27 M | 871 MC/s | 676 MC/s |
| 64 M | 1069 MC/s | not run |

Both on a bare box with absorbers and a port, with other work on the machine, so treat them as rough.
A whole run with real metal gives the same S11 on both engines to 2e-4, and the same directivity and
efficiency to three decimals. An `Auto` run moves to the GPU from 0.3 M cells: with other work on the CPU,
a 0.23 M dipole took 2.4 s against 3.5 s, and a 0.6 M patch 16.7 s against 26.9 s. This card's memory
bandwidth is a limit: the one published Rust and wgpu figure, about 3700 MC/s on an RTX 4060, is on a faster
card, in vacuum, by another author.

Setting a run up used to cost 6 to 9 s whatever its size, measuring the time step on a pool of every core.
On a pool sized to the model it's a fraction of a second.

openEMS's own GPU engine, from its CI build v0.37.0-38 as no release has it yet, on the same card:

| Model | Built-in, CPU, before that fix | openEMS CPU | openEMS GPU |
|---|---|---|---|
| Dipole, 36 cells/λ | 21.5 s, 53.48-50.53j | 39.5 s, 54.63-43.05j | 4.8 s, same Z |
| Loop, 24 cells/λ | 41.5 s, 3.60+549.73j | 176.9 s, 3.48+499.84j | 65.7 s, 3.18+499.94j |

Its GPU engine's reactance matches its CPU engine's, so it doesn't change the gap discussed below.

## Wave ports

A feed can launch its line's own mode instead of driving a gap, on any quasi-TEM line: microstrip,
stripline or coax. Tick **Wave port** on a feed on the Draw page, or give a feed `"wave": true` in
`antenna_draw`, or list it in `wave_feeds` in `antenna_set`. The two ends of the feed join the line's two
conductors on the model's outer edge.

The line's cross-section is solved statically on the plane, using the same cell coefficients as the 3D run,
so its impedance and speed are what this mesh supports. S is referred to the line's own impedance, as an
HFSS wave port is by default, and to the plane the feed was drawn on. The cross-section is copied
behind the plane out through the absorber (`Extrude`), after assembly and scaled for the cell lengths, so a
wave leaves as if the line went on for ever. That's why the feed has to sit on the outer edge, and the line
has to run straight for a cell or more into the model. The near-field box leaves out the part the line
passes through: the guided wave there isn't radiation.

`two_wave_feeds_read_the_line_between_them` runs a plain line on the whole path a project takes, and
`a_wave_feed_gives_a_full_result_on_either_engine` a far field and both engines.

The other solvers run a wave port as a gap, and say so.

## Slanted traces

Metal sits on mesh edges, so the current in a slanting trace runs in steps. What that does, on a 1 mm
strip at 45 degrees, with the method of moments at 982.5 MHz as the reference:

| Cell around it | Across its width | X = 0 | Error |
|---|---|---|---|
| 10 mm | 0.1 | 672.9 MHz | -31.5 % |
| 5 mm | 0.2 | 764.5 MHz | -22.2 % |
| 2.5 mm | 0.4 | 885.2 MHz | -9.9 % |
| 1.25 mm | 0.8 | 860.2 MHz | -12.5 % |
| 0.8 mm | 1.25 | 955.4 MHz | -2.8 % |
| 0.5 mm | 2 | 988.5 MHz | +0.6 % |

So there are two rules. A trace always conducts: below two cells across it, where no edge is sure to lie
inside it and meet the next, its centre line goes down in steps along the nearest edges
(`a_slanting_strip_is_joined_up_at_any_width`). And the planner refines around any slanting trace to two
cells across, stopping at a 24th of the model's cell so one hairline doesn't set the time step of the whole
run. A warning says when it does either, and the first is a warning worth acting on.

## Adaptive passes

`simulate_adaptive` refines planes where the last pass held its energy, until S stops changing. On the
FR-4 patch it stopped after 6 passes at 2.87 M cells, within 0.015 of a uniform 45 cells/λ at 2.76 M cells.
So it stops correctly but saved nothing here, and the change between passes wasn't monotonic (0.23, 0.06,
0.10, 0.13, 0.016), which means a loose limit could stop it early. It isn't in the app. The app's existing
study, repeating a uniform mesh, works with this solver and is the better choice for now.

## The reactance gap: what it turned out to be

For a long time openEMS read a loop's reactance below the method of moments, about 18 % in capacitance
with inductance agreeing, and nobody knew which to believe. Two things explain it.

1. **A port's current.** Reading current as the loop of H around the port's cell leaves out the
   displacement current of that cell, and so part of the gap's capacitance. openEMS's probe works that
   way. This solver reads the current through the port's own resistor, which is what a chip sees.
2. **What a gap is.** MoM fills the drawn gap with a metal bridge and drives a delta gap across it,
   which behaves like a very narrow gap whatever was drawn, with a capacitance that keeps growing as
   its mesh refines: self-resonance 1439, 1431, 1411, then 1386 MHz at 96, 192, 384 and 768 edges/λ.

On the 30 x 20 mm loop, this solver's self-resonance follows the real gap:

| Real gap | X at 915 MHz | X at 1100 MHz | L | Self-resonance |
|---|---|---|---|---|
| 1.5 mm | 549.9 | 899.3 | 61.4 nH | 1528 MHz |
| 0.75 mm | 559.4 | 929.6 | 61.4 nH | 1506 MHz |
| 0.375 mm | 582.5 | 1006.0 | 61.6 nH | 1461 MHz |
| 0.2 mm | 590.0 | 1044.0 | 61.0 nH | 1437 MHz |
| 0.1 mm | 610.5 | 1124.0 | 61.0 nH | 1403 MHz |
| MoM, 384 edges/λ | 616.3 | 1123.2 | 62.1 nH | 1411 MHz |

So the physics agreed all along and the feed definition differed. A measured tag still has to say
which feed model matches a real chip, and it matters: on a gap wider than about 0.1 mm MoM would read
high, and for design the true pad gap has to be in the model.

## What an independent read found

A fresh reader went through this code without being told what to expect, and found real faults that the
tests had missed. All are fixed and pinned in `tests_review.rs` and `fit/study_tests.rs`: narrow-strip mesh
lines on the wrong axis, close parallel sheets merged into a short, a thin metal body vanishing, a sheet
off its line getting half the capacitance, loss left out of the subcell correction, a Debye branch sharing
an edge with a port or with metal, a divergence test that could never fire, and a time-step estimate that
could miss a few hot edges. The time step now comes from Lanczos in place of power iteration.

## Limits

- **Nothing built for this project has been measured.** The two patches above are someone else's.
- **Curved metal is first order**, about 1 % at 32 cells across a sphere.
- **Field-driven adaptive passes** saved no cells on the one model tried, and aren't in the app.
- **Slanted traces finer than the cap** run in steps and read low, as the table above shows, with a
  warning. Drawn along x or y they don't.
- **Wave ports are quasi-TEM only.** There is no waveguide mode solver, so a waveguide or a line well above
  its first higher mode can't use one.
- **Metal thickness** is not modelled in the field, only in the sheet's impedance, 35 um assumed. On a
  0.5 mm trace 35 um of copper widens the trace electrically by roughly 10 %, by Wheeler's formula.
- **Debye** is the one dispersive model. Lorentz and Drude aren't here.
- **Subgridding** isn't here. Grading a tensor grid is what stands in for it.
- **HFSS and CST haven't been run here.** The comparison above is with a paper's 2006 figures. The proposed
  bars, |S11| within 0.015 on the same model and resonance within 2 to 3 %, need someone with licences.

## Running the checks

```
cargo test -p fdtd_kernel --features gpu --lib -- --include-ignored  # exact answers
cargo test -p antenna_core --features gpu --lib fit:: -- --ignored --nocapture --skip study_
SENSESIM_FDTD=builtin cargo test --bin SenseSim -- --ignored fdtd_    # the openEMS benchmarks, without it
tools/simulation_gate.sh                                             # all of them, in the full gate
```

The `study_` tests print comparisons to read: `study_three_solvers` runs this solver, openEMS and MoM
on the same models, `study_feed_gap` is the table above, `study_searched_tag` a real thin-trace tag,
`study_measured_patches` the built antennas, `study_adaptive_savings` what the adaptive mesh saves.
