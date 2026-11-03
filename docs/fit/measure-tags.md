# Measuring tags: stage 0 of the solver plan

Simulation here has never been checked against hardware. This is the one job a solver can't do for
itself. It settles which feed model matches a real chip, and it gives every later solver change
something real to answer to.

## What to make

Make three tags on the same stock. They're chosen so the feed effect, the thin-trace effect and the
real design can each be told apart. `fit_write_measurement_kit` writes the outlines as Gerber and DXF:

```
cargo test -p antenna_core --features fit --release fit_write_measurement_kit -- --ignored --nocapture
```

They land in `output/measurement-kit`.

| File | What it is | What it isolates |
|---|---|---|
| `loop_2mm` | 30 x 20 mm loop, 2 mm trace, 1.5 mm gap | The bare loop every solver study here uses, so no radiator confuses it |
| `loop_2mm_narrow` | The same, with a 0.2 mm gap | The feed capacitance alone: only the gap changes |
| `tag_thin` | The searched tag, 0.567 mm main trace | A real thin-trace design |

Have each made twice, so that a difference between two copies shows the fabrication spread. Etched
copper on thin FR-4 or polyimide is fine. Measure the board's thickness, the copper's thickness and
each trace's real width, because all three go into the model. Don't measure a tag on a metal bench.

## What to measure with

- A VNA calibrated to the probe tips (SOL, or SOLT for two ports), about 300 MHz to 2 GHz so the
  loops' self-resonance is in the sweep, IF bandwidth 1 kHz or less, at least 401 points.
- A way to feed a balanced gap. A two-port differential measurement is best: two semi-rigid coaxes
  with their shields soldered together and their centre pins to the two pads, and
  `Zdiff = 2 Z0 (1 - S11 S22 + S12 S21 - S12 - S21) / ((1 - S11)(1 - S22) - S12 S21)`, or the same
  thing from the VNA's own differential mode. A single coax with a balun also works but adds the
  balun's own reactance, which has to be de-embedded.
- Foam or a low-loss stand at least 100 mm from anything, with the coax leaving at right angles to
  the loop. A hand or a bench near the tag moves these results by more than the solvers differ.

## What to record

- Save `.s1p` or `.s2p` Touchstone, and photograph the setup. Keep the raw file: don't only export a
  chart.
- Measure each tag three times, re-soldered once, to find the setup's own repeatability.
- Note the temperature if it's far from 20 C, because polyimide and paper move with humidity too.

## Comparing

`antenna_core::fit::compare_measured` reads the measured network against a simulated one and returns
the worst reactance error, the resonance error and the worst S error across the band:

```rust
let measured = Network::from_touchstone(&text)?;
let c = antenna_core::fit::compare_measured(&measured, &simulated, 0.8e9, 1.03e9);
```

Report the figures against the repeatability you measured. A solver can't be shown to be better than
the difference between two copies of the same tag.

## What the result decides

- Whether `loop_2mm` follows the built-in solver with its real 1.5 mm gap (self-resonance near
  1528 MHz), or MoM's delta gap (near 1400 MHz). If the built-in solver is right, MoM's delta gap is
  reading high on any gap wider than about 0.1 mm, and tag design should use the built-in solver or
  a corrected MoM.
- Whether narrowing the gap moves the resonance by what the solver says, about 90 MHz from
  1.5 mm to 0.2 mm. That is the most direct test of the feed model, and it doesn't depend on knowing
  the stock's permittivity, because both copies share it.
- Whether the thin-trace tag lands within the 3 % reactance target set for stage 2. If it doesn't,
  the solver plan's stop rule applies: keep MoM for thin traces until the reason is known.
