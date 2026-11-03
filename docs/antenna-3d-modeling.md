# Modelling antennas in 3D

Antenna Studio's **3D model** page works like a mechanical CAD program: sketches on planes, a tree of
features built from them, faces and edges picked off the model. Every dimension is an expression over
the variable table, so sweeps and the optimiser move drawn geometry the same way they move a template.

## View and Draw

The switch at the top left of the canvas says who has the left mouse button.

| | Left button | Also |
| --- | --- | --- |
| **View** | Drag orbits, click selects, double-click a flat face to sketch on it | Nothing can be drawn by accident |
| **Draw** | Belongs to the tool in hand | Middle-drag or Alt-drag still orbits |

Right-drag pans and the wheel zooms towards the pointer in both. `V` flips the switch, `F` frames the
model. `Esc` backs out one step at a time: what's being drawn, the tool, the sketch, the selection.

The floor is the world's z = 0, and its red and green lines cross at the true origin. Every sample and
template stands there: on the floor, or through it where its feed is at the origin, like a dipole. Two
kinds are held up on purpose: a gate's panels, whose z is the height its test reads tags at, with a dashed
line to say how far, and dipoles a quarter wave over a ground plane, where `h` says so.

## Sketches

Start one on **Top**, **Front** or **Right**, or click a flat face with **On a face**. A sketch on a face
goes with that face when the model below it changes.

| Tool | Key | |
| --- | --- | --- |
| Select | `S` | Drag points and curves. Dimensions and constraints hold while you do |
| Line | `L` | A chain. Near level or upright it snaps and says so. Click where it began to close it |
| Rectangle, Centre rect | `R` | Click two corners, or drag |
| Circle | `C` | Centre, then rim, or drag |
| Arc | `A` | Centre, start, end, counter-clockwise |
| Spline | | Through points, `Enter` ends it |
| Point | `P` | Somewhere for a feed to go |
| Dimension | `D` | A line, a circle, two points, or a point and a line |
| Round corner | | Replaces a corner with a tangent arc whose radius is a dimension |
| Construction | | The selected curve becomes a reference line that makes no geometry |
| Fully define | | Adds the dimensions still needed, from where things are |

Shapes get their dimensions as they're drawn. **Click a dimension on the canvas to type it**: a plain
number is millimetres (an angle, degrees), and anything else is an expression like `Lp/2+1mm`. The `x=`
button next to a dimension in the panel makes it a variable of its own.

Blue can still move, black is held, red is at odds. The panel says how many ways the sketch can still
move, and lists every dimension and constraint so one can be removed. With the Select tool, select
points and curves (Ctrl for several) to get the relations that fit them: coincident, horizontal,
vertical, parallel, perpendicular, equal, tangent, midpoint, symmetric, fixed.

## Features

The tools are on the *Model* tab of the app's ribbon, in groups, all showing at once, on every page of an
antenna project. A tool clicked on another page, or on the Flat parts view, brings up the 3D model first and
then runs. *Bring in* turns a template's or the flat tools' parts into features.

| Group | Commands |
| --- | --- |
| Tasks | Simulation, Optimization, Stop |
| Clipboard | Paste, Cut, Copy, Delete |
| Drawing 3D | Box, Cylinder, Cone, Sphere, Torus, Helix |
| Drawing 2D | Rectangle, Circle, Ellipse, Polygon (flat metal with no sketch behind it), Sketch, On a face |
| Drawing 1D | Line, Polyline, B-spline, Arc, Wire, Make face |
| CAD files | Import, Export, Bring in |
| Boolean | Unite, Subtract, Intersect, Exclude, Cut, Shell |
| Modifications | Move, Rotate, Reflect, Clone line, Clone axis, Thicken |
| Fillet and extrude | Fillet, Chamfer, Revolve, Extrude, Sweep, Loft |
| Surfaces | Bent sheet, Revolved, Lofted |
| Ports | Feed between two points, Probe through a sketch point |
| Coordinates | XY, XZ, YZ work plane; Add, Move, Rotate a coordinate system |
| Measure | Length, Angle, Section, Mesh |

While a sketch is open the tab shows what a sketch uses instead: Finish sketch, Select, Line, Rectangle, Centre
rect, Circle, Arc, Spline, Point, Dimension, Round corner, Construction, Fully define, Make face, Extrude, Wire.

Shapes in Drawing 3D, 2D and 1D are placed with the mouse, with their numbers to type at each step. Their
dimensions, and those of the sketch a sheet or extrusion came from, are in the Parameters panel. Right-click one
for *Add variable* or *Use variable*. A flat sheet or shape has a Thickness: a time-domain solver cuts the real
slab, and the method of moments keeps a sheet at its plane.

A feature is added at once with a sensible size and selected, so its dimensions can be typed straight
away. Extruding a sketch that's on a face adds to that body; Cut goes right through. A feature's panel
sets where its result goes (new body, add, cut, common), its material and its mesh size. Left to itself,
the result goes to "the latest body", the newest one that isn't a wire or a pattern's copy.

**Feeds.** Click two places. An end clicked on a sketch point stays tied to it: its dimensions move the
feed, and the far end follows at a fixed step, so the port stays on its axis at any value. An end clicked
on a face is a fixed position. **Probe** goes on a sketch point and runs from the metal on that sketch's
plane to the nearest metal below it, or above if there's none below. In a stacked patch that's ground to
driven patch, not through it to the parasitic one. With no metal on the plane, it joins the nearest
either side. A feed or probe whose sketch point has been deleted is marked as failed; it doesn't move.

**Section** cuts the view open on any axis without changing the model: closed bodies show a flat cut face
with an outline. It's only on the screen and the solvers never see it.

In the tree, the eye switches a step off and the curved arrow rolls the model back to before it. A step
that fails is marked in red with the reason, and the steps around it still build, so one bad dimension
doesn't take the model off the canvas. A model with a failed step won't solve until it's mended.

## What the solvers get

| Modelled as | Goes to the solvers as |
| --- | --- |
| Box, cylinder, sphere that haven't been cut | The exact shape |
| An extrusion or sheet on a plane square to the world | An exact outline, holes included |
| A surface of flat faces square to the world, cut or not | An exact outline per face |
| Anything else | Triangles: a closed solid for FDTD's polyhedron, or an open sheet |
| A wire or helix | A wire, which the wire solver takes |

Triangles are remade to the mesh's size before the method of moments sees them (`modeler/remesh.rs`),
because a sliver in that mesh is a wrong answer, not a slow one. A mesh size set on a body refines FDTD
around it. Flat faces of a triangle body ask for FDTD mesh lines of their own.

**Metal bodies that touch may have to be joined for the method of moments.** Current crosses only where
two surfaces share a mesh. Exact shapes and flat faces get imprinted on each other by the mesher, but a
surface it remakes as triangles only joins where a vertex happens to coincide. The page says when this
applies, even if they only share a face or a line, and Modify, Join does it. FDTD doesn't mind. A wire
is a route rather than a surface, so it can't be combined: patterned wires stay wires.

**Combining what's selected.** Ctrl or Shift and a click, on the model, in the tree or in the Bodies list,
selects several; Ctrl+A takes every body. Modify then has Join, Subtract, Intersect and Exclude, and the
panel offers them too. Subtract keeps the body picked first. Solids combine as volumes. Sheets on one plane
combine as areas, whichever way they face: joined they become one face, so a solver gets one outline with
no stray points, and a sheet can have a hole cut in it by another. Sheets on different planes can be joined
but not cut, since all they share is a line. On Flat parts the same selection and the same four words work
on the shapes there. The result is one or more drawn polygons, whose sizes are numbers.

## How far it has been checked

Models built with these features, against the same antennas from templates, by the method of moments:

| Antenna | Template | Modelled |
| --- | --- | --- |
| Strip dipole, 1 GHz | 66.75 − j12.35 Ω | 66.75 − j12.34 Ω |
| Wire dipole, 1 GHz | 67.03 − j14.25 Ω | 67.03 − j14.25 Ω |
| Slot, 2.45 GHz, hole in the sketch | 496.4 − j51.6 Ω | 496.4 − j51.6 Ω |
| Slot, cut by a solid | 496.4 − j51.6 Ω | 491.4 − j57.7 Ω |
| Patch on air, probe on a sketch point | 39.5 + j9.9 Ω | 39.5 + j9.9 Ω |
| PIFA as a bent sheet | 46.9 + j12.3 Ω | 46.9 + j12.3 Ω |
| Thick dipole, exact cylinders against triangles | 82.0 + j24.1 Ω | 83.1 + j22.4 Ω |

A bicone from revolves gives 153 + j100 Ω, near its characteristic impedance of about 150 Ω, moving
little with mesh (150 + j100 Ω at 12 edges per wavelength, 153 + j100 Ω at 20). There's no template of
it to compare with.

A patch on FR-4 by openEMS FDTD, its substrate going in as a polyhedron rather than a box:

| | Resonance | S11 | Gain | Cells | Time |
| --- | --- | --- | --- | --- | --- |
| Template | 2.4331 GHz | −28.9 dB | 2.26 dBi | 658,560 | 343 s |
| Modelled | 2.4400 GHz | −28.7 dB | 2.34 dBi | 596,160 | 264 s |

They're 0.28 % apart in frequency, on meshes that aren't quite the same. None of this has been measured
on a real antenna.

Booleans are checked on every use: a solid has to come out closed, with a volume in the range the
operation allows. Their tests run the identity V(A∪B) + V(A∩B) = V(A) + V(B) on 270 random cases,
including every coincidence of faces, edges and corners a grid can make.

## What it doesn't do

- **Fillets don't blend where they meet.** Each edge is rounded by its own tool, so where filleted edges
  meet at a corner they intersect rather than form a smooth patch. Only a solid's edges can be filleted:
  on a sheet, round the corner in its sketch.
- **Curves are facets.** A circle has 32 sides by default, set under Mesh, Curves. Solids stay meshes,
  not exact surfaces, past the first boolean.
- **No STEP.** STL comes in as a body, in millimetres. STEP is neither read nor written.
- **A sketch's curves can't branch or cross.** Three lines at a point, or two loops overlapping, are
  refused with a reason rather than guessed at: trim them, or make the extras construction lines.
- **No trim tool, and no patterns inside a sketch.**
- **Feeds lie along x, y or z,** which is what the solvers' ports can do.

## For the assistant

`antenna_model_add` takes steps by name, with every dimension over variables, and refuses a number no
variable can move. `antenna_model_get` reads the tree back, and `antenna_model_clear` removes it. Faces
are named `top`, `bottom` and `side N`. On a body made from a sketch, N counts that sketch's curves from
0, leaving out construction lines, and a face the sketch doesn't have is an error.
