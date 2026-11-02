# 3D design and simulation

Sense Sim has a parametric CAD workspace (SolidCraft's, embedded) and runs the 17 analysis types of
its Create Simulation dialog on open-source solvers and a built-in GPU solver. Studies run on design
bodies or on USD objects from the scene, from the Simulations panel or from the assistant's tools.

## Layout

| Where | What |
|---|---|
| `third_party/solidcraft` | SolidCraft, at upstream 176c22ac with two local changes: `SolveApp::host_chrome` and the workspace menu (see below). Plain source, not a submodule. Path dependencies; its build scripts need the whole tree. |
| `src/design/` | The Design workspace. `host.rs` runs SolidCraft's UI off-screen with no Bevy in it; `mod.rs` shows the picture and passes input. |
| `crates/sim-core/` | Solvers' input, output and running, and looking inside results (`post.rs`). No Bevy or egui, so its tests need no window. |
| `src/sim/` | The dialog, panel, result view, scene objects as bodies (`scene_body.rs`), saving (`persist.rs`), the assistant's tools (`tools.rs`). |

## How the design workspace is hosted

SolidCraft replaces fonts, theme and zoom on its egui context, and draws its viewport through
`egui_wgpu` paint callbacks, which `bevy_egui` skips. So it has an egui context, a renderer and a GPU
device of its own, renders off-screen, and its frames are read back and shown as an image. The cost
is a frame or two of latency. In exchange, a SolidCraft GPU error can't reach Bevy's error handler,
and egui and wgpu needn't stay in step between the two.

It should read as part of Sense Sim, not as a second program in the window. `host.rs` turns SolidCraft
to its light theme on the workbench's canvas colour, and sets `host_chrome`, the local change: no
application bar (name, version, Report a bug, its own tabs and menus), while the modelling ribbon stays.
SolidCraft's `embedded` flag won't do, because it takes the ribbon too. Its File menu is still SolidCraft's,
opened from the shell's design strip (`src/ui/design_bar.rs`), which replaces the scene tabs and has the
design tabs, Save, Undo, Redo, Send to scene, settings and Licences. The strip and the workspace talk
through `shell.rs`, as it needs the workbench shell's types.

The workspace menu is the second local change. Before adding to a row, check what upstream has by then, since
duplicating its work would fork both the code and users' files:

| Row | What it does | Why it stops there, at upstream 176c22ac |
|---|---|---|
| Design, Render | SolidCraft's own | |
| Animation | Motion tools, exploded views and storyboards | Upstream only has an RFC (#8330), so storyboards are built here. They live in design metadata under `com.sensesim.storyboards`, so the file format doesn't change. |
| Simulation | `host_workspace_request` asks the shell for Sense Sim's Simulation workbench, through `Benches::request` | Upstream's #4356 is airflow only and stale |
| Manufacture | Sending to a slicer, no toolpaths | The CAM PRs (#5751, #2881, #2620, #4158) are unmerged, and upstream hasn't assigned their format number |
| Drawing | Shown as Planned | Upstream is building it on its `drawings` branch (#5349, #4895). Take theirs. |

Keyboard input is taken from the host context before its UI runs (`steal_input`), so Ctrl+S in a
design doesn't also save the layout.

Unsaved work is written to `%LOCALAPPDATA%\SenseSim\design\autosave.sc` on the way out, and brought
back the next time the workspace opens, instead of asking on the way out. File > Quit must go through
`crate::quit()`, not `process::exit`, or this is skipped. Test runs use an autosave file of their own
(`use_test_autosave`): a test's parts must never come back as someone's unsaved work.

The CAD kernel runs in this process. `catch_unwind` around its frame turns a panic into a disabled workspace,
but nothing holds an abort or a bad access, so the scene is saved to `scene-before-design.json`, in the same
folder, each time the workspace opens. It opens like any saved layout.

Saving a layout writes the studies into it under `simulation`, and the design beside it as a `.sc` file
of the same name. Loading doesn't replace a design with unsaved changes; it says so.

## Solvers

They run as separate processes and are never linked. They're fetched from their own projects, with
pinned SHA-256 hashes, only after the user agrees. Licences are shown from the panel and the design
strip (`src/sim/notices.rs`).

| Solver | Where | Used for |
|---|---|---|
| Gmsh 4.15.2, CalculiX 2.23 | Native, `%LOCALAPPDATA%\SenseSim\solvers` | Static, Dynamic, Heat Transfer, Thermomechanical, Harmonic, Frequency, Nonlinear |
| OpenFOAM v2512 | WSL distro `SenseSimSolvers` | Incompressible, Multi-purpose, Compressible, Convective, Conjugate Heat Transfer, Multiphase |
| Elmer | WSL distro `SenseSimSolvers` | Electromagnetics |
| Built-in D3Q19 LBM | GPU | Incompressible (LBM), Pedestrian Wind Comfort, Conjugate Heat Transfer (voxel) |

Rules for WSL (`jobs/wsl.rs`): only ever touch `SenseSimSolvers`, never `wsl --shutdown`, never
`.wslconfig`. `wsl.exe` dying does not end the Linux process, so every solver runs under a supervisor
that ends its process group when its stdin closes or its heartbeat stops.

The VM's memory is shared with the user's own distros, and when it runs out the kernel kills something,
perhaps of theirs. So every run may address 80% of what's free when it starts, and each rank of a parallel
solver an eighth of that (`ulimit -v`, see `START` and `RANK_LIMIT`). Past it, the run's own allocations fail
and it says so. OpenFOAM studies also estimate their memory before they start, and refuse if it won't fit.
GPU solvers refuse a grid the card has no room for (`lbm/gpu.rs`).

## Faces: the part that must not go wrong

A load on the wrong face gives a plausible, wrong answer. So:

- The truth is SolidCraft's `3mf-tagged` export: every triangle of a watertight mesh names its B-rep
  face. STEP face order isn't used, because export merges and rewrites faces.
- `geometry/classify.rs` matches each mesh surface to those faces, then matches every tagged triangle
  back to the mesh. Disagreement, a wrong area or a missing face stops the run.
- A picked face stores a point on it. If an edit to the design moves the face, the run stops and asks
  for it to be picked again. There is no fallback that guesses. `check.rs` does this for every solver
  route, and `tests/moved_faces.rs` makes sure each one really stops.
- **Check faces** in the panel (`sim_check_faces` for the assistant) colours the body by condition and
  gives each one's area, before anything is solved.
- OpenFOAM meshes are audited the same way: patch areas from the mesh's own files against the design
  (`foam/mesh_audit.rs`), and a leak stops the run. `sim_the_audit_catches_a_real_leak` runs the real mesher
  on a closed cube and on the same cube with a wall gone.
- A flow refuses one face under two conditions: the mesher would get the same facets twice.
- Two bodies in contact are meshed apart, because where they touch, two faces lie on top of each other.
- A scene object doesn't say what it's made of, so analyses that depend on that ask for it to be confirmed.

A USD object has no faces, so `geometry/regions.rs` makes them: it welds the mesh, makes the windings
agree, turns it outside out if need be, and grows faces up to a 30 degree crease. Those are only as
meaningful as the mesh's creases. Structural analyses and Elmer need one closed shell, and say so
otherwise; the voxel solvers take anything. Gmsh remeshes such a surface, split along the same creases.

## Verification

`Readiness::Verified` in `catalog.rs` is earned, not assumed. Each one has benchmarks against known
answers, and they cover what the panel actually runs. They need the solvers, so they are `#[ignore]`:

```
cd crates/sim-core
cargo test --release --test structural -- --ignored --nocapture --test-threads=1
cargo test --release --test foam       -- --ignored --nocapture --test-threads=1
cargo test --release --test elmer      -- --ignored --nocapture --test-threads=1
cargo test --release --test lbm        -- --ignored --nocapture --test-threads=1
```

The whole path through the real app, on real CAD and USD:

```
SenseSim --no-mcp --design-test out/design                   # the workspace, through real window events
SenseSim --no-mcp --sim-test-all --sim-test out/all          # every solver family on modelled parts
```

`SENSESIM_TEST_FROM=<step>` starts that second one partway, and `SENSESIM_TEST_USD=1` runs just the USD
object and its face picking. `SENSESIM_SOAK_HOURS=<hours>` makes the first one repeat ordinary work for that
long in one process, with a row per cycle in `OUT_soak.csv`.

This desktop is shared, so test runs shouldn't get in anyone's way. `SENSESIM_TEST_HIDDEN=1` never shows the
window: everything still renders, including its screenshots, and tests use `TestPointer` so the real pointer
stays where its owner left it. Run a long test under a renamed copy of the exe too, since a peer's
`taskkill /IM SenseSim.exe` closes every copy. A run that stops early with "No windows are open, exiting" in
its stderr and no failed check was closed from outside.

### What the benchmarks gave

Error against the known answer, and what the plan allowed.

| Analysis | Benchmark | Result | Allowed |
|---|---|---|---|
| Static | Cantilever, 3.054 mm | 0.39% | 1% |
| | NAFEMS LE1, 92.7 MPa | 0.17% | 2% |
| | NAFEMS LE10, -5.38 MPa | 0.28% | 2% |
| | Same cantilever from a bare triangle mesh | 0.38% | 1% |
| Dynamic | Step load: period, peak | 0.20%, 2.1% | 1%, 5% |
| Heat Transfer | Transient slab, 62.9 C | 0.41% | 0.5% |
| | NAFEMS T4, 18.25 C | 0.32% | 2% |
| Thermomechanical | Constrained bar, -252 MPa | 0.001% | 1% |
| | NAFEMS LE11, -105 MPa | 0.73% | 3% |
| Harmonic | Resonance: frequency, amplitude | 0.03%, 0.32% | 1%, 3% |
| Frequency | First two modes | 0.01%, 1.2% | 1%, 2% |
| Nonlinear | Elastica: sideways, back | 0.15%, 0.35% | 2% |
| | Bar yielding at sigma_y | 0.000% | 1% |
| | Clamped plastic limit | in band | see below |
| | Hertz contact: peak pressure, radius | 0.10%, 1.2% | 5% |
| Incompressible | Ghia cavity; Poiseuille | 0.95%; 0.09% | 2%; 0.5% |
| Incompressible (LBM) | Taylor-Green order | 1.99 | 2 +/- 0.2 |
| | Schaefer-Turek 2D-1 C_D | 1.41% | 3% |
| | Against OpenFOAM, air past a cube at Re 100, worst of 5 places, as a share of the wind | 7.0% | 10% |
| Multi-purpose | MRF Taylor-Couette torque | 0.05% | 1% |
| | Choked nozzle flow rate | 0.26% | 1% |
| Compressible | Sod: p*, u*, shock | 0.02, 0.16, 0.06% | 1% |
| | Oblique shock: p2/p1, angle | 0.03%, 0.99% | 1% |
| | Mach 2 at a body in the app: pitot | 3.8% | 10% |
| Convective | de Vahl Davis Nu, Ra 1e5 | 0.67% | 2% |
| | de Vahl Davis Nu, Ra 1e6 | 0.89% | 2% |
| Conjugate HT | Isoflux channel: gap, bulk | 0.11%, 1.4% | 2% |
| | Heat balance on a modelled body in the app, over repeated runs | 2.5 to 3.5% | 5% |
| CHT (voxel) | Isoflux channel gap | 1.32% | 3% |
| | de Vahl Davis Nu, Ra 1e5 | 0.11% | 3% |
| Multiphase | Rising bubble: height, area, peak speed | 1.7%, 0.05%, 2.5% | 3%, 1%, 3% |
| | Droplet pressure, at 40 and 80 cells across | 4.9%, 0.75% | 5% |
| Electromagnetics | Coax capacitance | 0.00% | 1% |
| | Solenoid field | 0.15% | 1% |
| | Skin depth | 0.0% | 3% |
| | Transient diffusion, against erfc | 0.0003% | 1% |
| | Coil on a design body, against the closed form | 0.12% | 5% |
| Wind Comfort | Empty-domain log law, 6 cells and up | up to 3.4% | 5% |

Where this differs from the original plan, on purpose:

- **Plastic limit.** The plan asked for beam theory's 7.81 kN within 3%. A clamped root raises the limit
  above beam theory, by up to 2/sqrt(3), and halving the mesh didn't move the 8.33 kN measured, so that
  isn't element locking. The check is now a free bar yielding at exactly sigma_y, plus that band.
- **Transient magnetics.** The plan named an RL circuit; this uses magnetic diffusion into copper, also
  exact, with no circuit model in the way.
- **The rising bubble's circularity** isn't measured.

Things these tests found, worth knowing before changing the code near them:

- **`3 w_i` isn't exact in floats.** Using it in the LBM equilibrium adds momentum at every collision.
  See `alpha()` in `lbm/shader.rs`. Don't "simplify" it back; the Taylor-Green order test catches it.
- **Courant numbers don't protect a sharp interface.** Multiphase needs the capillary time-step limit;
  above it, a bubble rises 25% too slowly with no other sign of trouble.
- **Full interface compression costs surface tension.** At `cAlpha 1` a droplet's pressure is 12% low and
  a finer mesh doesn't help. At 0.25 it converges. The price is a slightly softer free surface.
- **A body held only by a force, waiting for contact, gives garbage with no error.** The first solve is
  singular, and stresses came out ten times too high and different on every run. Displacement-driven
  loading gave Hertz to 0.1%. `Study::problems` refuses the bad setup.
- **Don't start a supersonic flow at full speed.** The air leaving a rear face opens a vacuum at once, and
  `rhoCentralFoam` stops on a negative temperature. The inflow ramps up from rest.
- **Lattice speed error is first order on a fixed grid here.** Drag at Re 20 was 3.1%, 1.4% and 0.5% low
  at peak speeds 0.05, 0.025 and 0.0125, which isn't the second order of a plain compressibility error.
  Viscosity falls with the speed in that series too, so the cause isn't separated. See `scales`.
- **Momentum exchange counts what arrives at the wall cell**, not what's at the fluid node beside it.
  The difference is the viscous stress: drag came out at 2.4 times the right value at Re 20.
- **Wind directions combine beside the body, not in the tunnel.** The body turns in a fixed tunnel, so the
  same tunnel cell is a different place by the body in each run. See `add_direction`.
- **An exact result has a range.** The isoflux channel is within 0.4% of exact in its developed middle and
  10% off 10 mm from its far end. Measure where the formula applies.
- **CalculiX fields are 20 characters.** A longer number is a hard error, but only for the bodies that
  happen to have one.
- **Gmsh refuses a binary STL with an all-zero header**, with only "Error loading".
- **A repeated time in a `.frd` isn't always a complex part.** Only harmonic runs have those. Contact runs
  end with a tiny extra increment, and it's the answer.
- **Measure outflowing heat as a flow-weighted sum, and in one iteration.** Mass flow times temperature rise
  is a small difference of large numbers. Taking the last row with an assumed mass flow gave 1.61 to 2.43 W
  for a 2 W block. The flow-weighted temperature and the mass flow of the same iteration, minus ambient and
  averaged over the last 30% of the run, gave 1.95 W twice. See `carried_heat`.
- **Export polyhedra with `foamToVTK -poly-decomp`.** Without it, snappy's polyhedral cells are dropped with
  no warning, and show as dark gaps next to the body.
- **A wall in a rotating zone needs `rotatingWallVelocity`.** With `noSlip` the MRF wall reads back as still.
- **A coarse mesh hides swirl with no error.** With about 3 cells per radius at Re 1000 a rotor's swirl came
  out at a tenth of what it should be. The app's own check runs at 0.6 rpm and finds 0.0018 m/s against
  0.0022 m/s at r 0.07 m. The torque benchmark is Taylor-Couette at solver level.
- **A flow solver's first answer can be long before its settled one, with no sign of it.** Running both
  solvers on one cube showed the GPU route, at one pass of air through its tunnel, a factor of two off in the
  wake, and settled from 4 passes on. No benchmark caught this: they all ran longer than the panel did. So
  runs now say so if they may not have settled. The first thing suspected was OpenFOAM's convergence target,
  which turned out to change nothing measurable. Measure before blaming.
- **The same run of a coarse flow isn't the same number twice.** Over repeated body-fitted CHT runs on the
  coarse test mesh, the hottest rise was 6.79 to 7.40 K, because parallel partitioning differs and the wake
  wanders. The voxel route, at its settled length, gave 8.58 K, so the two routes are 14 to 21% apart there,
  not a single figure.

## Limits

- Wind Comfort stays Beta: not validated against AIJ benchmarks, and results within a couple of cells of
  the ground are less sure. For comparing layouts, not for checking a standard. Its wake runs long: behind
  a 2:1:1 building it reattaches at 3.39 D, where a wind tunnel measured 1.37 D (Tanaka et al. 2006, the same
  shape as AIJ's Case A, not its data) and other CFD gave 2.10 to 2.89 D, and it shows no reverse flow on the
  roof at 32 cells across D. The inlet has no turbulence, the likely cause, not isolated. AIJ's own data
  couldn't be fetched: its site returned nothing.
- Contact is between two design bodies of one material, each held by a support or a displacement. The
  element size is global, so a small contact patch needs a fine mesh everywhere.
- Electromagnetics in the panel is electrostatics and a coil's steady field, in air. Eddy currents and
  time-varying fields are checked at solver level, through Elmer decks, but aren't offered in the panel.
- Conjugate Heat Transfer (body-fitted) is one closed body in forced air, with its heat released evenly
  through it. Temperatures on faces aren't offered, and it's slow to settle. Its hottest rise came out 14
  to 21% below the voxel route's on one coarse block, and neither has a known answer there; the benchmark
  behind its accuracy claim is the channel.
- A rotor (Multi-purpose) needs a fine mesh: see the swirl note above.
- Compressible is inviscid: walls slip, so no boundary layers or skin friction.
- The voxel solvers' walls follow the voxels, so drag converges slowly with cell size.

Licences: SolidCraft is MIT OR Apache-2.0; its `NOTICE` and `THIRD-PARTY-LICENSES.txt` are shown in the
app. The GPL solvers stay separate processes fetched from upstream. Get this confirmed before a
commercial release.
