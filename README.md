# Sense Sim

![Sense Sim](assets/logo_wordmark.png)

Sense Sim is a Rust (Bevy + egui) viewer and scene builder for the NVIDIA
SimReady USD dataset. It reads USD with the pure-Rust `openusd` crate, so no
C++ or Python USD install is needed.

## Install

Run `dist\SenseSim-Setup-1.12.0.exe`. It installs per-user (no admin) into
`%LOCALAPPDATA%\Programs\Sense Sim`, adds a Start Menu entry (desktop shortcut
optional) and an uninstaller in *Apps & features*.

The app uses `D:\simready-assets` if it exists, otherwise
`%USERPROFILE%\SenseSim\simready-assets`. Pick another folder with the `+`
on the *Library* section or in Settings → Library; the choice is remembered.

## Use

- **Create a scene:** every session starts with this dialog: scene kinds on the left, the scenes
  of that kind in the middle, the highlighted one on the right with a render and a description.
  Hover to preview, click to select, then *Create Scene* (or double-click). Choose *Empty scene* or one
  of 9 samples: *UWB warehouse* (40 × 28 m, 72 racks, 13 anchors) and *RFID warehouse* (12 smart shelves,
  51 readers, 172 tags), one positioning technology each; *Hospital ward*, *Auto workshop*,
  *Office*, *Canteen*; and the plant systems *Pump station*, *Engine drive*, *Conveyor line*.
  *Have a layout?* opens a saved one. File → *New scene…* (Ctrl+N) brings the dialog back.
  `--sample N` (1–9) loads a sample directly. `tools/make_previews.sh` re-renders the pictures
  (`--sample N --preview --screenshot`).
- **Layout:** the 3D view fills the window; everything else floats over it as white cards.
  The top bar has the scene name (click to rename), Save, Settings, the File / Edit / View menus,
  Downloads, the AI assistant and Help (all keyboard and mouse shortcuts). The grips beside the
  cards resize them, and each section folds with its chevron.
- **Start:** the dialog at launch and from File → New asks what to work on: an antenna, which needs
  no scene, or a scene. At launch Antenna is already highlighted. Press Esc to skip it and start with a blank scene.
- **Workspaces (the tile at the left of the ribbon):** click it to change workspace. Each has its own ribbon
  tabs and project tree, and what is on offer depends on the project.
  A *scene project* has *Layout* to build the site (assets, walls, arranging), *Plan* to place positioning devices
  and plan coverage, *Simulation* for the live test, RF analysis, the RF solver, predictions and outputs, *Plant*
  to run connected equipment and *Factory* for the real hardware on the network. All five work on the same scene.
  An *antenna project* needs no scene and has its own *Antenna* workspace, which is Antenna Studio, with tabs for
  each stage: Design, Model (every modelling tool on one tab), Solve, Results, Analysis and Arrays.
  A project opens in the workspace it is for: an antenna project in Antenna, a scene with a plant in Plant, a
  scene with positioning devices in Plan, any other scene in Layout. Each scene tab keeps its own workspace. Each
  workspace remembers the card it had open, and a dot on the tile flags what goes on behind another one:
  alarms on Simulation, a running plant on Plant, failed devices on Factory.
- **Project tree (left):** the workspace's pages, whose nodes show how far they are set up
  (green check: done, red ring: needs something, blue ring: optional, amber ring: working) and open a
  detail card beside the tree; *Downloads*; and the *Job status* card with what is running.
- **Scene tree (right):** every placed object under the scene's name, filterable above a few objects.
  Click to select, double-click to focus. The eye hides an object (or, on the top row, all of them);
  hover a row for duplicate and delete. Hidden objects can't be picked, and hiding isn't saved.
- **Library:** search, filter *All / Local / Online*, switch grid or list view, click a card,
  then click in the scene to place it. Online assets have a blue name and a cloud badge; clicking
  one downloads it and then places it. After a search, *Get N* queues every online match.
  Downloads run two at a time and show progress on the card and under the Downloads button in the top bar.
  The folder is rescanned every 15 s.
- **View toolbar (top centre of the canvas):** frame all, grid, hover bounds, shadows, viewport settings.
- **Nav cube (bottom right):** click a face to look at it, drag it to orbit. The arrows step the view,
  the house goes back to the opening view with everything in frame. The bar at the bottom shows the scale.
- **Placing:** Q/E rotate the preview (Shift: 90 deg). Shift or *Multi-place* keeps placing.
  Esc or right-click cancels. Objects drop onto whatever is under the cursor.
- **Glass doors:** *Glass door* in the Layout ribbon places a closed door, 1.8 x 2.1 m. Stand it in a
  gap between walls: inside a solid wall, radio still pays for the wall. Radio predictions read it as
  glass, and RFID deployment puts portal antennas on it. *Radio* under Properties says what any object
  is made of, metal-coated (low-E) glass included.
- **Select and move:** click an object, drag it across the floor or onto other objects
  (Alt keeps height, Ctrl snaps).
  - **Orange ring:** rotate.
  - **Green arrow:** lift. A lifted (or Alt-moved) object stays in the air and gravity leaves it alone;
    **Drop** (Properties or right-click) lets it go, and it lands on whatever is under it.
  - **White dots:** moving parts (doors, drawers, wheels). Drag a dot to move that joint
    within its limits; the label shows the angle or travel.
- **Properties (beside the scene tree):** rename, quick actions (focus, duplicate, drop, reset or
  rotate, delete), X/Y/Z position, rotation and scale, sliders for every moving part, load info.
  The check keeps your changes and deselects; the X puts back what the object was when you selected it.
- **Camera:** right-drag orbit, middle-drag or Shift+right-drag pan, wheel zoom,
  WASD/Space/C move, F focus. *Help* in the top bar lists all keys.
- **File menu:** save (Ctrl+S) or open (Ctrl+O) layouts (JSON, joint poses included), or export
  a Y-up `.usda` that references the original assets.
- **Tabs:** each scene opens in its own tab. Pick several files in *Open layouts…*, or pass
  `--layout` more than once. Ctrl+N / Ctrl+T new, Ctrl+Tab / Ctrl+Shift+Tab switch, Ctrl+W or a
  middle-click close (asking first if there are unsaved changes). A layout is read the first
  time its tab shows, and a tab that isn't showing is paused: its simulations wait, its camera,
  selection and undo history are kept.
- Fonts: Lato (SIL OFL, `assets/fonts/Lato-LICENSE.txt`) and Phosphor Icons (MIT).
- Set `HF_TOKEN` (or run `hf auth login`) for higher Hugging Face rate limits.

## Graphics card (NVIDIA RTX and others)

Sense Sim renders on the high-performance GPU by default. That is the discrete NVIDIA RTX on machines
that also have integrated graphics. Any DirectX 12 or Vulkan GPU works (NVIDIA, AMD, Intel).
**Settings → Graphics & GPU**:

- **Active GPU:** name, kind (discrete/integrated/software), API, driver, and whether hardware ray tracing (RTX) is available.
- **Renderer:** GPU preference, graphics API (Auto/DX12/Vulkan/OpenGL) and a specific adapter. These apply after
  **Restart now**. If a chosen API or adapter fails to start, the next launch falls back to Auto.
  The `WGPU_BACKEND` / `WGPU_ADAPTER_NAME` environment variables still override them.
- **Frame rate:** vsync, and a frame cap to save power.
- **AI vision models:** DirectML on the fast GPU (default), on the power-saving GPU, or on the CPU.
- **Live telemetry** (NVIDIA, via `nvidia-smi` from the driver): power, load, temperature, memory, clock, fan.
- **OpenEMS link:** sends the telemetry to the `gpu0` meter of the OpenEMS that the RTLS outputs use (see below).

Settings are saved in `%APPDATA%\SenseSim\gpu.json`.

## Build

Needs the GNU Rust toolchain (pinned in `rust-toolchain.toml`) and WinLibs MinGW
on PATH. In Git Bash:

    . ./env.sh
    cargo build --release                         # target/release/SenseSim.exe
    cargo run --release --example make_icon       # regenerate icons from assets/logo.svg
    "$LOCALAPPDATA/Programs/Inno Setup 6/ISCC.exe" installer/SenseSim.iss   # dist/ installer

`cargo run --release --example probe -- D:/simready-assets` batch-checks the USD loader.

Scripted UI checks: `--screenshot out.png` also takes `--no-select`, `--shot-delay S` and `--open NAME`
(`deploy`, `test`, `devices`, `inspector`, `live`, `rf`, `planning`, `outputs`, `rtls-settings`, `openems`,
`fdtd`, `predictions`, `settings`, `settings-viewport`, `settings-graphics`, `settings-performance`, `settings-assistant`,
`assistant`, `downloads`, `jobs`, `start-empty`, `start-N`, `bench-layout`, `bench-plan`, `bench-antenna`,
`bench-simulation`, `bench-plant`, `bench-factory`). Debug builds also take
`--drive "wait:5;click:96,722;probe"`, which plays mouse input through the app without touching the real
pointer (see `src/drive.rs`).

## AI assistant

Click **AI Assistant** in the menu bar. Type an instruction ("place a row of 4 pallet racks
3 m apart"), or attach/drop a photo of a factory or warehouse and send it.

For a photo, positions are **measured, not guessed**: on-device vision models recover the
camera pose and metric depth, detect every object, and measure its floor position, width,
height and orientation. Assets are placed from those measurements, nudged until their outlines
match the photo, and the room shell (floor, walls, columns) is built around them. The assistant
asks when it can't decide (scale, which asset, missing assets) and finishes with a review.

**Self-check**: `verify_scene` measures the scene with ray casts. It finds objects that float,
are sunk, are cut by a shelf, or intersect each other, and how well each object lines up with
the photo (IoU). When the assistant tries to finish, the harness runs it and sends the
assistant back to fix what it finds (up to 4 rounds).

**Providers** (Settings → AI assistant; the model list loads from the provider): Anthropic
(`ANTHROPIC_API_KEY`, default `claude-opus-5-5`), OpenAI (`OPENAI_API_KEY`), Meta Muse
(`MODEL_API_KEY`, default `muse-spark-1.3`), Google Gemini (`GEMINI_API_KEY`), xAI Grok
(`XAI_API_KEY`), DeepSeek (`DEEPSEEK_API_KEY`), OpenRouter (`OPENROUTER_API_KEY`), and local
Ollama / LM Studio (no key). **Custom OpenAI-compatible** reaches any other server or vendor,
with a key if it needs one. Muse's `-contributor` models are cheaper because Meta may train on
what you send; the settings page warns when one is selected.

**Approvals** (Settings → AI assistant) set what the assistant may do without asking. The default
lets it edit the scene and project, and asks before it writes files elsewhere or uses an external
MCP tool. Anything that aims the app at an outside system (`rtls_configure_outputs`, starting
OpenEMS), or records a real measurement (`outcomes_check`, `site_survey_fit`, `site_anchor_fit`,
`site_acceptance_score`, drift tools, `design_fit_measured`), asks in every mode. MCP clients can't
be asked, so those calls are refused for them.

**Project files.** A project saves as one `.sesim` file, a compressed container like a `.docx`, holding the scene, devices, plant, simulation studies and factory view, or an antenna design, which opens in Antenna. The installer makes double-clicking one open the app. Layouts and `.antenna.json` files saved by earlier versions still open, and saving one writes a `.sesim` beside it.

**Files.** Each chat has a working folder under `%LOCALAPPDATA%/SenseSim/agent`. `open_file` opens
a file by type: a layout in a new tab, DXF or Gerber in Antenna Studio, IFC as a
building, an image for the assistant to see. `open_external` shows a document, folder or web page in
its own app; it can't run programs, and it asks first. Settings shows how much saved chats take up,
and can clear them.

**Python.** The first time the assistant wants a script, it asks to download Python (about 150 MB, about 400 MB on disk, in %LOCALAPPDATA%/SenseSim/python; your own Python isn't touched). uv and every package are pinned by hash. Scripts run in a Windows AppContainer: no network, including this computer's own ports, and no access to your files or API keys beyond the chat's working folder. A script reaches the app only through `sensesim.call`, with the same approvals as any call, and it can't record measurements. If the sandbox can't start, the script doesn't run.

**Long tasks.** A request stops and asks once it passes its budget (2M charged tokens, 600 model
calls or 3 hours by default; set in gent.json). The panel shows tokens used, and a cost for models
you give a price to under "prices" in gent.json. When a conversation nears the model's limit, old
images and tool results are trimmed, then its earlier part is condensed into notes; evidence labels
and the plan are kept word for word. Set "context_tokens" on a provider if its model holds more or
less than the default guess (200k for cloud, 32k for local servers). Chats are saved under
%LOCALAPPDATA%/SenseSim/agent, and the last one can be picked up again after a restart. Changing
model mid-chat hands the new one its earlier tool calls as plain text.

Local models must support tool calling and vision, for example `qwen2.5vl`. **Test connection**
checks the key and model. Settings are stored in `%APPDATA%/SenseSim/agent.json`; a copy next
to the exe takes precedence. Transient API errors (429/5xx) are retried with backoff.

**Vision models** run in-process on the GPU (DirectML; CPU fallback) via ONNX Runtime. On the
first photo the app downloads the runtime (~40 MB) and the models (~1 GB: Metric3D v2,
Grounding-DINO, CLIP) into `%LOCALAPPDATA%/SenseSim`. No Python needed.
`cargo run --release --example vision_photo -- photo.jpg [hfov] [labels…]` runs the
measurement on its own and writes `photo.jpg.overlay.png` / `.depth.png`.
**Settings → Measure all** records every local asset's size so search can rank by size.

**MCP server**: the viewer serves its tools at `http://127.0.0.1:8765/mcp` (`--mcp-port N`,
`--no-mcp`), so other agents can build scenes in it:

    claude mcp add --transport http sense-sim http://127.0.0.1:8765/mcp

**MCP client**: add external servers to `agent.json`; their tools show up for the assistant as
`<server>__<tool>`:

    "mcp_servers": {
      "blender": { "command": "uvx", "args": ["blender-mcp"] },
      "remote":  { "url": "http://localhost:9000/mcp" }
    }

### Knowledge base

The assistant has a built-in, offline library of sourced engineering notes on RF propagation, UWB, UHF RFID, antennas, BLE/WiFi positioning, and building and plant dimensions. It searches them with `search_knowledge` before choosing engineering numbers, and clearly relevant passages are attached to your messages automatically. MCP clients get the same tools.

To add your own documents, open Settings → AI assistant → Knowledge base → **Add your documents**, put `.md` or `.txt` files in that folder (one sub-folder per topic), then press **Reload documents**. PDFs must be converted to text first. See `crates/knowledge/README.md` for the format.

## Predictions: numbers you can commit to

Open **Predictions** in the *Simulation* workspace, or ask the assistant. Every recipe here runs the same loop (`crates/proof`): **predict with a range, measure, calibrate, predict again, decide**. Each number says what it rests on (*Estimate*, *Simulated*, *Calibrated*, *Measured*), what it assumes, and what makes it uncertain. Add a real measurement to any prediction and it gets scored. The ledger is kept in `%APPDATA%\SenseSim\predictions.json`.

**None of these models has been checked against real hardware yet.** They've been tested against known answers, the field solvers and real building files. The first measurements you add are what will show how far to trust them.

| The question | Tool | What it rests on |
|---|---|---|
| How much does this antenna's shape shift UWB ranges, by direction? | `design_uwb_bias` | Method of moments; FDTD for printed antennas |
| Will this RFID antenna read this shelf and not the next? | `design_shelf` | Method of moments for the metal; products as a loss range |
| How wrong is the angle from this array, and how much does a calibration table help? | `design_aoa` | Method of moments with coupling |
| What does the enclosure do to the antenna, and what fixes it? | `design_integrate` | Method of moments; FDTD for plastic |
| Which RFID tag, placed where, on this product? | `design_tag` | A quick estimate fitted to FDTD and MoM solves of a strip dipole, for ranking only |
| How much do the radios in this device hurt each other? | `design_coexist` | Link budget from typical radio figures |
| Does the built antenna match the model? | `design_fit_measured` | A VNA sweep (Touchstone) |
| How many manufactured units keep their range? | `design_field_yield` | An estimate: a formula fitted to method of moments solves around the design, good to about ±15 points of yield |
| What might these things be made of, from a photo? | `site_guess_materials` | CLIP on a photo: right about 60 % of the time for sites and 30 % for products on test photos, so only a suggestion |
| Where should I walk to learn this site's materials? | `site_survey_plan`, `site_survey_fit` | Your walk; whole spots are held back in turn to score what would be applied |
| How does the system do on bad days? | `site_stress` | The whole simulation, across full and empty racks, crowds, worse materials and a dead anchor |
| Has anything changed since it was commissioned? | `site_drift_baseline`, `site_drift_check` | Live ranges against the design |
| Which undecided material matters most in an unbuilt building? | `site_import_bim`, `site_material_ranges` | An IFC file and material ranges in dB |
| Will firefighters' radios work on every floor of a building not built yet? | `site_erces` | An IFC file, a signal arriving outside, and loss ranges. The share and level defaults are the commonly adopted ones: check your local code |
| Which technology for this site, and what does it cost to own? | `site_compare_tech` | A real deployment and simulation of each, your prices (placeholders until you give them), and typical battery figures |
| How many test points does a handover take, and did it pass? | `site_acceptance_plan`, `site_acceptance_preview`, `site_acceptance_score` | Exact statistics (59 points all passing shows 95 % at 95 % confidence). A preview is a forecast and can never pass |
| Where are the anchors really? | `site_anchor_fit`, `site_anchor_practice` | Ranges between anchors, knowing what each link crosses. Ranges fix the shape of the set; surveyed anchors tie it to the building |
| How fast can a tag report and still last? | `site_battery` | The simulator for accuracy at each rate; typical charge figures for life |
| Can every shelf label be reached, and how long does a price change take? | `site_esl` | Labels sampled along shelf faces, retries and shared airtime. Radio figures are placeholders |
| How fast can this RFID tunnel run? | `design_tunnel` | A quick estimate built on the tag model, not checked against a real tunnel |
| What radio does a robot see along its route? | `site_route_coverage` | Signal strength along the path, with racks full and empty. Congestion and real roaming aren't modelled |
| Which ward radios should a medical device be tested against first? | `design_medical_coexist` | A link budget from typical radio figures. For planning tests: it doesn't replace ANSI C63.27 or a risk analysis under AAMI TIR69 |
| Do forklift alerts fire in time at the real corners? | `site_proximity` | Encounters on the real layout. Supports a design: it can't show a system is safe |
| Roughly how much SAR will this body-worn antenna cause? | `design_sar` | Real FDTD with tissue beside the antenna, compared with reference dipoles (see below). A design check, never a compliance result |

Results are in `outcomes_ledger`; `outcomes_check` records a measurement. `design_*` jobs run in the background (`design_job`). `site_survey_practice` writes a synthetic walk to try the loop: it never counts as a calibration. `site_trace` lists what a ray crosses between two points, `site_set_material` says what an object is made of, and `site_set_antenna` gives a device an analysed antenna or marks a tag as worn on a person. `site_conditions` sets rack fill and crowd levels for a run.

**A survey walk** is a file of `device,x,y,z,kind,value` lines (`kind` is `range` or `rssi`), or JSON lines. Say whether you measured it (`origin: field`): nothing else can count as calibrated, and a practice walk is refused as field even converted to another format. A material's scale is applied, for its band in this tab, only if at least 5 spots and 2 anchors saw it, the scale is known to ±0.25, the fitted model explains the walk, and it predicts the held-out spots better than the built-in value. A position number is Calibrated only when the delay and loss of every material that matters in it pass, and only until the geometry changes. **A drift check** reads JSON lines of `{"device":"A1","tag":[x,y,z],"range_m":4.2,"expected_excess_m":0.3}`, where `expected_excess_m` is what known walls add on that link (leave it out and any wall reads as drift), from tags that cover at least 5 spots. With no file it's a drill on the running simulation, which never says healthy.

**What a range and a verdict mean.** A range spreads the assumed inputs only, not model or measurement error, so a real measurement can land outside it. From 19 runs up it holds about 90 % of runs from those inputs; below that it's the lowest and highest run. *Likely* takes a 95 % lower bound of at least 90 %, which starts at 29 runs out of 29, and more than an estimate behind it: an estimate is at best *Worth testing*, and one solve *Meets in this one solve*.

**BIM.** `site_import_bim` reads IFC2x3 and IFC4 (`crates/ifc`). Walls are cut around their doors and windows, and materials come from the element's layers and name. Each element is one upright box, so L-shaped floors and pitched roofs over-cover. `.ifczip` isn't read, and furniture and services aren't imported. Stated materials are saved with the layout.

**The SAR pre-check.** `design_sar` runs real FDTD with a slab of tissue beside the antenna and reads the field in two planes through it, estimating the standard's cube from how the field falls off in each. Checked against reference dipoles tuned to resonance, per watt: 10.81 W/kg over 1 g and 6.90 over 10 g at 900 MHz, 15 mm from the tissue, against 10.8 and 6.9; 52.11 and 23.87 at 2450 MHz, 10 mm from it, against 52.4 and 24. Those targets are IEEE 1528's as recalled, not read from the standard. Real dipole certificates list about 11 and 7, and 53.1 and 24.7 with about ±17 % (`fdtd_sar_reference_dipole*`, ignored by default, about 20 minutes). It's a design check: a hot spot off those planes isn't seen, tissue is flat and uniform, and only an accredited lab can show compliance.

**A handover test** (`site_acceptance_*`) is a file of `id,x,y,z` or `id,error_m` lines. Its points are drawn at random from equal patches of the free floor before anyone tests, because the "59 points shows 95 %" statistics only hold for a random draw. Results count as measured only with a `source` saying who measured them and how. **Ranges between anchors** (`site_anchor_fit`) are `a,b,range_m` lines. They fix the shape of the set, not where it sits, so survey two or more anchors and name them to tie it to the building. Anchors only move if `source` says who logged the ranges, the fit settled, and it predicts pairs it never saw better than the design does.

**Limits worth knowing.** Stress tests, drift checks and the new site recipes run on the main thread, so the window freezes until they finish. `site_compare_tech` judges accuracy with a tag standing still at the same spots for each technology, one run each, with typical battery figures and placeholder prices until you give real ones. `site_battery` adds to the simulated error how far a moving tag goes between fixes, and its charge figures are typical values. `site_esl` uses placeholder radio figures. `design_tunnel` and `design_medical_coexist` are estimates that haven't been checked against a real tunnel or ward, and `site_proximity` treats a blocked link as giving no range at all, where real reflections may give a late or long one. Drift checks read a file or the simulator, not a live feed. Predictions are shared across scenes in one ledger, and open tabs share one set of stated materials. Survey fits and FDTD calibrations stay with their tab and their band.

**Without Antenna Studio.** Recipes that solve plastic or printed boards need FDTD, which a `--no-default-features` build leaves out. Metal-only recipes work in every build.

### Antenna design with the assistant

Ask for an antenna by its requirements, for example "a 2.4 GHz patch on FR-4 with at least 2 dBi". The assistant sets the requirements (`antenna_spec`), picks a template, computes starting dimensions (`antenna_calc`), runs the real solvers and judges each run with `antenna_assess`, which reports a verdict, a diagnosis and what to change next. It's sent back until the assessor is satisfied, or until it says what limits the design. It can also sweep variables, study mesh convergence, design matching networks, and estimate yield and arrays. Results are simulated, not measured.

### Regulatory regions

Set the site's country under RTLS → Settings → Regulatory region, or ask the assistant (`rtls_set_region`). UWB then moves to an allowed channel and RFID readers are capped to the EIRP limit, across the simulation, coverage maps and auto deploy. Each rule shows how well it is verified, and a rule that could not be confirmed changes nothing and shows a warning. With no region selected, devices keep their profile values.

These are design inputs, not legal advice: confirm them with the national regulator. To add a country or correct a rule, put a JSON file in `%APPDATA%\SenseSim\regions`, in the same format as `assets/regions/regions.json`. A file with an existing `id` replaces that region.

## Connected systems: piping, engines, conveyors

Objects connect through **ports**: pipe nozzles, shafts, pulleys and conveyor ends. A link
always mates two ports exactly; nothing is lined up by eye.

- **Ports** come from three sources:
  - generated parts, which are exact;
  - the catalog `assets/plant/catalog.json` (extend it in `%APPDATA%\SenseSim\plant\catalog.json`
    or with `plant_define_port`);
  - detection on the mesh at load time: open circular rims of pumps, tanks and valves, the shafts
    of spinning joints, and conveyor ends taken from their rollers.

  `SenseSim.exe --ports asset.usd` prints what was found.
- **Generated parts** (pipe, elbow, tee, reducer, ball valve, pump, tank, motor, pulley, inline
  engine, roller conveyor) are small USD files with real physics joints. They go through the
  normal asset path.
- **Running plant** (▶ at the bottom of the viewport). Engines and motors drive what is coupled
  to their shaft (1:1) or belted to their pulley (diameter ratio). Rotors and rollers spin, and
  pistons and rods follow the crank. Items on conveyors travel and transfer to the next conveyor.
  - A hydraulic solve covers pipe losses, entry and exit losses, valves, pump curves and tank
    levels, and shows open ends as leaks. An open end only spills, unless `plant_set` declares it
    a supply. Pump curves without catalog figures are defaults, and the report says so.
  - Flow is drawn along the pipes in the liquid's colour, and tanks show their level.
  - Select an object to switch it on, set its rpm, valve opening or tank fill, and see its ports.
- **Assistant / MCP tools:**
  - Inspecting and connecting: `plant_list_ports`, `place_objects` with `connect_to`,
    `plant_add_part`, `plant_route_pipe` (orthogonal pipes and elbows around obstacles),
    `plant_connect`, `plant_disconnect`.
  - Building and operating: `plant_build` (`pump_loop`, `engine_drive`, `conveyor_line`),
    `plant_set`, `plant_run`, `plant_status`.
  - `verify_scene` also checks links, open pipe ends and undriven pumps. The harness sends the
    assistant back until `plant_run` reports `ok`.
- **Saving.** Links and settings are saved in the layout under `plant`. USDA export adds a
  `PhysicsFixedJoint` per link.
- **Samples:** *Pump station*, *Engine drive*, *Conveyor line*.
- **Headless test:** `SenseSim.exe --plant-test layout.json --duration 60 --report out.json`

## Antenna Studio

A workspace for designing one antenna, in the workflow of HFSS, CST Microwave Studio and FEKO: **Antenna** in the
start dialog or in the ribbon's workspace list; it needs no scene. While it is in front, the
top bar's name and Save, Ctrl+S and the File menu act on the antenna project. A `--no-default-features` build leaves it out.

**Flow.** Pick one of 44 templates, draw your own on the canvas, or import a DXF or Gerber drawing. Set the design
frequency and edit the variable table; everything is an expression (`0.47*lambda0`, `12mm`, `er`). Run. The results
pages, array, matching and exports all work from that run.

**The window.** On the left, the *Project* tree: objects with the steps that made them, coordinate systems, ports,
boundary conditions, meshing, results, materials and settings. Under it, *Parameters*: the numbers of whatever's picked
in the tree or on the model, in a grid to type in, including a sheet's thickness and the dimensions of the sketch it came
from. Along the bottom, *Variables* (name, expression, value, optimizable, min, max, and which part uses them), *Messages*
and *Jobs*. Every tool is on the ribbon; a page with more to say, like a plot, opens on the right and closes again.

**Modelling.** Pick a shape on the *Model* tab and click on the grid: a box takes a corner, a size and a height, with the
numbers there to type at each step, and what's typed stays as typed, so a variable's name stays a variable. Right-click any
number in Parameters for *Add variable*, which names it, or *Use variable*. Copy and paste make separate objects with their
own dimensions. A flat sheet can be given a thickness: FDTD and FDM cut the real slab, while the method of moments keeps a
sheet. New shapes go on the work plane (XY, XZ or YZ) in the coordinate system in use, in bold in the tree.

**Drawing flat parts.** *Flat parts* on the Design tab puts tools on the canvas: rectangle, polygon, strip, circle, and a Cut option for holes and
slots. Points snap to the grid and to existing corners, on whichever plane and height you choose. Select drags a shape
or its corners, double-clicking an edge adds a corner, and right-clicking a corner removes it; every corner can also be
typed exactly. Feed clicks across a gap and lands on the metal along an axis; Probe joins two sheets. Ctrl+Z undoes. A
template's parts can be turned into editable shapes. A drawn dipole, patch and slot solve to the same impedance as their
templates to the digits shown.

What's drawn keeps its dimensions: a rectangle has a centre and two sides, a circle a centre and a radius, a strip a width
and a path, a hole its corners, and a feed its two ends, width and impedance. Each is an expression, with variables
offered as you type, and `x=` beside a field makes a new variable from what's in it. Sweeps and the optimiser then work
on those variables by name, as they do for a template's. The mouse leaves alone any coordinate written in a variable, and
the page says so. An imported outline stays in numbers until *Give it dimensions*. The *Drawn* samples open on the
drawing with their dimensions already named.

**Importing.** DXF: `LINE`, `ARC`, `CIRCLE`, `ELLIPSE`, `LWPOLYLINE`, `POLYLINE`, `SPLINE`, `HATCH`, `SOLID`, `TRACE`,
flat `3DFACE`, blocks with `INSERT`; loose pieces chain into closed outlines, nesting makes holes, and paper space and
frozen layers stay out. Gerber: RS-274X flashes, draws, arcs and regions, one file per layer; aperture macros are
reported and left out. A layer named FEED or PORT marks feeds; without one, the narrowest gaps in the metal are offered.
DWG, binary DXF, STEP and other 3D solids can't be read; 3D comes in as STL. So far the DXF reader has been checked
against hand-written files in the R2000 layout, not files from other CAD programs.

**Reconfigurable antennas.** PIN diodes, RF switches, varactors and fixed parts go across gaps in the metal, and states say how each is set. Each gap is a port of the model, so one solve of all the ports answers for every state: closing a port with a part's impedance is algebra on the S-matrix and on the per-port far fields, exact for linear parts. So changing a part, a bias or a state re-judges at once, and a search can try every combination for the best one per target (band, match, gain, beam direction, polarisation). An ordinary run, sweep, optimisation or yield solves the active state, and any state can go to the result pages like any other antenna. For each state: match, band, realized gain, beam, polarisation, loss in the parts, and the RF volts and current on each part with the power at which it reaches a limit, including a varactor's RF swing passing its bias. There are varactor tuning curves, a coverage map across states, part-spread yield, a tuner that moves the metal with the settings re-searched at every step, and exports: a CSV bias truth table, Touchstone per state, and an HTML report. Mounts can add a DC block, a bias choke with its self-resonance, and pad capacitance. Templates: switched-length dipole (frequency), varactor-tuned PIFA (frequency), switched-beam parasitic array (pattern), and corner-switched patch (polarisation). Closing a port matches the same model solved with that resistance as its port, in the method of moments and against a real lumped element in a separate openEMS run (S11 within 0.002, complex far field within 0.4 %). A closed switch matches an unbroken wire, an open one a wire cut there, and power in equals power radiated plus power lost in the parts. Each state is judged at its own frequency, on a pattern interpolated between the 17 solved across the sweep (9 for FDTD, which pays run time for each): against patterns solved directly, the worst case in the templates was 0.5 % of the field and 0.04 dB of gain. If the power stops adding up, the page says so. Parts are linear: harmonics, intermodulation and an RF swing moving a varactor aren't modelled. Bias lines aren't in the model unless drawn. Library part values are typical datasheet figures, some assumed, and each says which. No reconfigurable design here has been measured.

**Phased arrays.** The *Phased array* page takes any layout: a line, a rectangular or triangular lattice, rings, a cylinder, multi-face panels, or a sparse line. Each element keeps its own orientation and the fields add as vectors, so panels facing different ways combine with the right polarisation, and on an array that sees all round the beam picks which elements light up. It takes several beams at once, digital or from one network, nulls, tapers, phase shifters and attenuators of finite bits, true time delay per element or subarray, hybrid arrays with analog aim and digital beams, random errors, and failed elements (click one to fail it). It reads realized gain, EIRP, beam widths, side lobes, grating lobes, pointing error, and instantaneous bandwidth. There's a beam table to load into a controller, a codebook of beams, a line's Butler beams, and a fit of weights to a shaped-beam mask. That's an array factor, with power conserved: coupling isn't in it. For coupling, solve a model with a port per element, such as the *Array* templates: one solve gives every port's far field and the S-matrix, so scanning is then superposition with no new solve, and it reads each port's active impedance and reflection, total active reflection, scan loss in two planes, blind spots, and the most any weights could realize in a direction. Checks: a uniform line gives its textbook 12.04 dB, −13.1 dB and 6.4°; squint follows sin θ · f₀/f; the band matches 0.886·λ/(L·sin θ₀) within 8 %; Butler beams are orthogonal and cross at −3.9 dB; Capon reaches its theoretical output SINR. On solved dipole arrays: radiated power matches accepted power within 4 % for random weights, nothing beats the conjugate-matched weights, and the middle element of a 9 × 9 has its cell's 4 π·dx·dy/λ² gain (Hannan's element-gain result) to within 0.15 dB once its own mismatch is counted. A 7 × 7's embedded element predicts a full 11 × 11 to 0.4 dB in the H plane and 1.1 dB scanned in the E plane, where the collinear dipoles nearly touch.

**Direction finding.** The *Direction finding* page generates what each channel receives from sources you place, then reads it with MUSIC, Root-MUSIC, ESPRIT, Capon, beamscan (a correlative interferometer, when the table is from a solve), MUSIC on a sparse line's coarray (more sources than sensors), monopulse, a phase interferometer, Watson-Watt or pseudo-Doppler. Each is offered only where the array suits it. Source counting is by MDL and AIC, and spatial smoothing with forward-backward averaging separates coherent sources such as multipath. Channel errors and converters as coarse as one bit can be added. With a solved array it uses the real coupling, and can compare a calibrated table against textbook phases on the same array. It flags directions the array can't tell apart, and an accuracy study runs Monte Carlo against the Cramér-Rao bound. It can also show what a frequency offset costs one receiver switched round the array, as a BLE locator has, and where bearings from several stations cross, with the error radius their geometry gives. The bound matches its closed form for one source, and MUSIC and Root-MUSIC come within 25 % of it at 10 dB SNR with ESPRIT within 50 %. Six sensors find seven sources on a nested line. It assumes narrowband signals across the array and far-field sources; with a solved array, each wave arrives in the polarisation the array mostly receives from there.

**Signal chain.** The *Signal chain* page takes a cascade of stages and a converter, by formula at once (Friis noise figure, cascaded intercept, noise floor, clip level, dynamic range, spur-free range, and whether the converter or the front end sets the noise), then sample by sample on demand: compression from each stage's intercept, oscillator phase noise, shared or per channel, I/Q imbalance, channel mismatch and calibration, clock jitter, quantisation and clipping, a decimating filter, and a steered or adaptive digital beamformer, against a QPSK, 16-QAM, OFDM, chirp, tone or noise signal and an optional jammer. It reads signal over noise at each point, array gain, EVM, spectra and the constellation, then runs the same signal through an amplifier across back-off, with and without predistortion. Checks: 6 dB per bit, the two-tone IM3 and 1 dB compression points, image rejection, jitter's ceiling and phase noise's level all match their formulas; the measured noise figure matches the budget's; eight channels gain 9 dB; a shared oscillator's phase noise survives the beam while separate ones average down by the channel count. The amplifier is memoryless with nothing to limit predistortion, so those curves stop at 0.01 % EVM and −90 dB: real memory effects and feedback noise would stop it well before that.

**Radar.** The *Radar* page simulates an FMCW MIMO radar from the raw beat signal through range and Doppler FFTs, CFAR, peak fitting, the time-division Doppler correction, and angle from the virtual array, sized by the radar equation. Targets come back where they were placed. A speed past the FFT's limit is read back from the transmitters' phases, up to their count times that limit, and folds beyond it. CFAR's threshold is worked out for all the channels added together, and its false-alarm rate reads within a factor of 4 of the design at rates high enough to count. The detected SNR agrees with the radar equation within 3 dB. Point targets in free space: no clutter, multipath or interference from other radars.

None of the array, direction-finding, chain or radar work has been measured on hardware. Solved arrays are labelled Simulated, and everything else Estimate.

**Designing an RFID tag.** The *Design an RFID tag* page, and the assistant's `antenna_tag_design`, search different structures rather than adjust a reference: T-match meander, a loop coupled to a separate radiator, a loop with two radiators, dogbone, bow-tie with a hairpin, slotted plate, square spiral, fractal arms and bent arms, plus any shape drawn for the job, against a chip, a band, a footprint and what the tag is really in. They're judged by forward-link read range across the band, and several options come back with what each stands out for and what it does if its surroundings change. A retune says what new surroundings do to an existing design, such as putting it under paper, and what small changes get back. *Evolve* lays out metal cell by cell with no assumed shape. The assistant can also draw any shape from a description with `antenna_draw`, every coordinate an expression over the variable table, so it can be swept, optimised or searched by name. A drawing with dimensions typed in as plain numbers is refused, so whatever it draws can be changed later by changing a variable, and one part can be replaced without redrawing the rest. Given a photo of a sketch, the assistant first turns and crops it with `antenna_sketch_view`, reads a closed outline as filled metal, and keeps any labels on it, such as A1, as the variables' names. Each drawing then comes back under that view, the same way up, with how symmetric it measures about its feed and a warning if it fits the pen strokes better flipped.

**Layers around flat metal.** Paper, PET, glue, a carton, glass, a bottle of water or a metal surface behind a spacer can lie over and under flat metal in one plane. The method of moments carries them in its Green's function (MPIE in layered media, Michalski and Zheng 1990), so a 20 µm film adds no unknowns and solves in under a second. Layers are as wide as the world there, and a surface wave along them counts as loss. FDTD builds the same layers to size, at a cost. Air layers land on the free-space solve, and metal behind air lands on the image solve to the digits shown; a strip on a thick slab shifts by the average permittivity's root (1.250 against 1.245). Against openEMS building the same layers, strip dipoles under 1 mm of paper and on 1.6 mm of FR-4 resonate within 0.23 % and 0.57 %. On a small loop the two disagree: openEMS reads reactance 15 to 20 % low near 915 MHz, moving towards the method of moments as its mesh refines, while the method of moments agrees with the thin-wire solver and the textbook inductance at low frequency. A tag's chip match depends on that reactance, so measure a built tag and retune before relying on a range figure. Paper, PET, adhesive and corrugated board values are typical or assumed, and no tag from here has been measured.

**RFID gate test.** The aisle of a gate sits inside the near field of its own antennas (a 1.5 m panel at 915 MHz has its
far field beyond about 14 m), so patterns and the array page can't say what reads there. This page takes any model with
ports, solves the field per port through the read zone, and combines the ports under each excitation state: the phases
and amplitudes a network or switch sets at one moment. A state list models a reader cycling through them. It reports the
share of tags read walking through in nine orientations, tags read on display nearby, the EIRP against a limit, the
reader power that keeps to it, and maps of where reads fail. Changing phases, power or the tag needs no new solve; *Find
better phases* searches them at the power each candidate may legally use. The `rfid_gate` template is five separately fed
elements on a panel, with `crates/antenna_core/fixtures/five_element_gate.dxf` as the same thing in a drawing. It models
UHF: E-field coupling to dipole tags. HF (13.56 MHz) loop gates are not covered.

| Feature | HFSS / CST / FEKO | Here |
|---|---|---|
| Parametric model, variable table with expressions and units | yes | yes |
| Antenna catalogue with synthesis formulas (Antenna Magus, Antenna Toolkit) | hundreds | 32: four reconfigurable, RFID gate, dipole, monopole, folded dipole, loop, Yagi, LPDA, axial and normal helix, turnstile, discone, meander, strip dipole, bow-tie, patch in air, PIFA, slot, corner reflector, horn, waveguide, probe, inset and circular patch, PCB IFA, UWB disc, Vivaldi, two-port patches, DRA |
| Add parts (box, cylinder, sphere, wire), import STL | full CAD with booleans | parts with expression coordinates and STL import; no booleans, no STEP |
| Draw shapes in 2D, edit corners, cut holes | yes | yes, on any axis plane, each dimension an expression; no sketch constraints, and circles are solved as polygons |
| Import DXF, Gerber | yes, and ODB++, STEP | DXF and Gerber; no ODB++ or STEP |
| Near-field read-zone test under several excitation states | scripted by hand | RFID gate test: read rate, stray reads, EIRP, phase search |
| Switches and varactors, every state from one solve | circuit co-simulation, set up by hand | Reconfigurable page: parts with bias and mounts, states, search per target, RF stress on each part, tuning curves |
| Material library, lossy dielectrics and metals | yes, with dispersion | 31 materials, constant εr and tan δ (water's loss follows Debye), conductivity. The built-in solver takes water as a true Debye material; Lorentz and Drude aren't available |
| Time-domain solver (CST T-solver) | FIT | built-in FDM with subcell metal and layers, on CPU or GPU, nothing to install; openEMS as an optional cross-check. On two patches someone else built and measured it lands within 0.3 % of that paper's HFSS and 1 % of its CST, and like them about 3 % from the hardware. HFSS and CST haven't been run here, and nothing built for this project has been measured: see [docs/fit](docs/fit/README.md) |
| Method of moments (FEKO) | MLFMM, multilayer | wire and RWG surface MoM, dense LU; metal in free space or over an infinite ground; no dielectrics, no MLFMM |
| Frequency-domain FEM (HFSS) | adaptive tetrahedra | **not available**; the mesh convergence study is the closest thing |
| Lumped ports, several per model, full S-matrix | yes | yes |
| Lumped RLC loads | yes | yes, as fixed parts on the Reconfigurable page |
| Waveguide, wave and microstrip ports | yes | wave ports on quasi-TEM lines (microstrip, stripline, coax) in the built-in solver, CPU and GPU, from the Draw page or the assistant; **no waveguide modes**; probes and lumped ports stand in |
| S-parameters, VSWR, Z, Y, group delay, Q, TDR, Smith chart | yes | yes |
| Far field: directivity, gain, realized gain, efficiencies, θ/φ, RHCP/LHCP, Ludwig 3, axial ratio, beam width, side lobes, front/back | yes | yes, with a 3D pattern and polar cuts |
| Near field and surface currents | yes | field planes (FDTD), currents per element (MoM) |
| Port excitation weights after the solve | yes | yes |
| Array factor on an element pattern, tapers, steering, grating lobes | yes | yes: Dolph-Chebyshev, Taylor, binomial, cosine, Hamming, Blackman, phase quantisation, element failures; no mutual coupling |
| Matching network synthesis (Optenni) | add-on | L, π, T, stub, quarter wave, broadband refinement, E-series parts, component Q, Bode-Fano limit |
| MIMO: ECC, diversity gain, TARC, active S | yes | yes |
| RCS | yes | monostatic and bistatic, MoM, metal only |
| Parameter sweep, optimiser, sensitivity, Monte-Carlo yield | yes | yes: 1D/2D sweeps; Nelder-Mead, particle swarm, differential evolution, surrogate; yield with sensitivities |
| Mesh refinement study | adaptive passes | yes, repeated solves; field-driven passes exist in the built-in solver's API, not yet in the app, and on the patch tried they saved no cells over uniform refinement |
| Touchstone, far-field files, report | yes | `.sNp`, `.ffe`, `.ffd`, CSV, one-page HTML |
| Fabrication output | yes | Gerber, DXF, STL |
| SAR | yes | a design-stage pre-check only (`design_sar`): flat tissue, field read in two planes, never a compliance result |
| Thermal, EMC, antenna placement on a platform | yes | **not available**, apart from using the pattern in this app's positioning planner |
| GPU acceleration, HPC | yes | the built-in solver on any Vulkan GPU, about 1 billion cell updates a second on an RTX A2000 at 64 M cells, and from 0.3 M cells it is the default; no HPC. openEMS here runs on CPU threads |

**Solvers.** *Automatic* picks MoM when a model is only metal and needs at most 6000 unknowns, and the built-in
finite-difference solver otherwise, which needs nothing installed. Everything else that runs a finite-difference model, the
RTLS antenna, through-object and whole-scene runs, the RFID gate's aisle field, SAR and the hardware recipes, runs on it too.
openEMS is optional: installed, it can be named on the Setup page or chosen on the field-solver card as a second opinion.
This changed the default from openEMS with no antenna of this project's measured yet, on the strength of the published
patches and the benchmarks in [docs/fit](docs/fit/README.md).

**How far to trust it.** Tests compare against textbook values, and `cargo test -p antenna_core` runs in under a
minute without building the app. These figures come from those tests; the FDTD rows are openEMS, with
`SENSESIM_FDTD=openems cargo test --bin SenseSim antenna_fdtd -- --ignored`:

| Check | Reference | Result |
|---|---|---|
| Wire MoM: λ/2 dipole, a = 0.001 λ | 86.2 + j46.2 Ω converged (exact-kernel Galerkin and Hallén), 2.15 dBi | 86.4 + j46.5 Ω at 160 segments/λ; 82.3 + j40.8 Ω at 20, which is 8 Ω of mesh error; 2.17 dBi |
| Wire MoM: power balance | radiated = accepted | ratio 1.00001 |
| Wire MoM: monopole over ground | half the dipole's impedance, 5.15 dBi | within 0.9 %, 5.18 dBi |
| Wire MoM: mutual impedance at 0.5 λ, a = 10⁻⁴ λ | −16.85 − j31.36 Ω converged (−12.5 − j29.9 Ω is the limit of a → 0) | −16.6 − j31.1 Ω at 40 segments/λ, −16.7 − j31.3 Ω at 80 |
| Surface MoM: PEC sphere RCS, ka = 1 and 2 | Mie series | −4.9 %, −7.9 % at 12 edges/λ; −1.7 %, −1.7 % at 24 |
| Surface MoM: 2 λ plate at normal incidence | 4πA²/λ² | −0.24 dB |
| FDTD: strip dipole | 2.15 dBi, radiated = accepted | 2.14 dBi, ratio 1.004, 74.6 + j4.3 Ω, 31 s |
| FDTD: monopole on PEC boundary | 5.15 dBi, radiated = accepted | 5.15 dBi, ratio 1.004, 26 s |
| FDTD against MoM: same strip dipole | agree | resonance within 3.0 % at 24 cells/λ and 12 edges/λ, 1.7 % at 36 and 96; directivity within 0.03 dB |
| FDTD against MoM: pyramidal horn | agree | 15.0 dBi against 15.8 dBi |
| FDTD: 2.45 GHz patch on 1.6 mm FR-4 | a lossy board: about 40-50 % efficient | 2.441 GHz, −28 dB, directivity 5.74 dBi, efficiency 45 %, 202 s |
| Drawn with the tools against the template: dipole, patch, slot | agree | 66.75−12.35j Ω, 39.5+9.9j Ω, 496.4−51.6j Ω, both ways |
| Gate field, 20 λ out, into a tag | Friis | −44.91 dB both |
| Gate states: radiated power at 0°, 90°, 180° | 1 − TARC² | 0.915, 0.801, 0.688 both |
| Gate states: beam from a phase ramp | theory −9.6°, −19.5° | −10°, −20° on a 5° grid |
| Gate read rate against mesh, 1098 to 9983 unknowns | settles | 98.7 % to 98.6 %; S11 moves more, −14.0 to −12.6 dB |
| FDTD against MoM: two interfering gate elements, field through the aisle | agree | 6.8 % complex difference, worst 1.45 dB; EIRP within 0.23 dB at 0°, 90°, 180° |

Known limits: MoM has no CFIE, so closed metal bodies show interior resonances near their cavity modes. FDTD runs
thin wires on the grid, so their radius follows the cell size, and tilted metal is staircased. FDTD accuracy on weak
coupling follows the stop level: −50 dB gives about 10 % at −30 dB. The array page ignores coupling between
elements, so close spacing is flagged; gain there is directivity times the element's efficiency. Dielectric loss is
exact at the design frequency only. Resonance depends on the mesh and the feed: on a strip dipole FDTD at 24 cells/λ
reads about 1.1 % low, MoM at 12 edges/λ about 0.5 % high, and even on the finest meshes tried about 1.3 % stays
between them, because a lumped port across a gap and an ideal delta gap are different feeds. That is 32 MHz at
2.45 GHz. No feed parasitics are modelled in either solver. Interior resonances don't show in the matrix's
conditioning, so nothing warns about them yet.

**Scripted screenshots.** `--open antenna-<page>` opens a page (`gallery`, `variables`, `parts`, `materials`, `setup`,
`sparams`, `smith`, `farfield`, `fields`, `array`, `phased`, `df`, `chain`, `radar`, `matching`, `mimo`, `link`, `rcs`, `sweep`, `optimize`, `converge`,
`yield`, `export`, `draw`, `gate`); `--open antenna-run-<template>` starts a run, `--open antenna-new-<template>` only
loads it, `--open antenna-import-<path>` opens a drawing, `--open antenna-gate-run` solves the gate test. Run test copies
with `tools\hidden_run.ps1`: on the normal desktop they catch whatever the person at the computer is clicking.
The assistant and MCP clients drive it with the `antenna_*` tools, including `antenna_import_cad`, `antenna_states`,
`antenna_gate_test`, `antenna_gate_results` and `antenna_find_phases`.

## RTLS simulator (UWB / BLE / RFID) and OpenEMS testing

The **RTLS** button (bottom-left of the viewport) opens a simulator for positioning hardware.
Radio links are ray cast against the placed USD objects, so shelves, cabinets and walls block them.

- **Devices.** UWB anchors and tags (two-way ranging, or TDoA blink), BLE beacons/gateways
  and tags (RSSI), UHF RFID readers/portals and passive tags (EPC Gen2 link budget with an
  antenna pattern and Q-slot collisions). Profiles are JSON (`assets/devices/generic.json`).
  Drop vendor profiles, such as specific Nextwaves models, into `%APPDATA%\SenseSim\devices\*.json`
  or a `devices\` folder next to the exe. A profile with the same `id` overrides the built-in one.
- **Placing.** Pick a profile, then click the scene: on the floor (mount height from the profile),
  on top of an object, or on its side.
  - Drag a device to move it. Alt+drag changes its height. Q/E rotate, Del deletes, Ctrl+D duplicates.
  - Tags can be static, follow a waypoint path (click to add waypoints), wander, or ride on a
    scene object (forklift, cart).
- **Engine.** TWR multilateration (Levenberg–Marquardt) with NLOS/outlier rejection, TDoA
  (hyperbolic), RSSI trilateration, a constant-velocity Kalman filter, and GDOP.
  - **Live tab:** RMSE, CEP50/95, fix rate, RFID reads/s, charts, alarms, zone occupancy and
    an event log.
  - **Inspector:** every link of a tag (true vs measured, LOS/NLOS, used/rejected).
- **Zones and alarms.** Zones can be restricted or have a capacity. Alarms fire for tags without
  a position ("lost"), entering a restricted zone, a zone over capacity, and devices switched
  off (offline).
- **Planning.** A coverage heatmap (UWB/BLE predicted error, RFID read zones), and a greedy
  optimiser that suggests where to add N anchors, beacons or readers.
- **Outputs (real time).** Every output runs on its own thread and reports its status:
  - **OpenEMS Edge REST** (`Controller.Api.Rest.ReadWrite`): writes metrics to
    `Component/Channel` and reads channels back to show the EMS reaction.
  - **Modbus/TCP server:** register map in `src/rtls/io/modbus.rs`. Writing register 1000 or
    1001 pauses the simulation or sets its speed.
  - **MQTT 3.1.1:** `<prefix>/tag/<id>/position`, `/rfid/<reader>/read`, `/zone/<z>/event`,
    `/alarm`, `/snapshot`. The simulation listens on `<prefix>/control`.
  - **HTTP webhook**, **UDP** (JSON or CSV), and a **JSONL recorder**.
- **Saving.** Devices and zones are saved in the layout file under `rtls`; older layouts still
  load. Settings and outputs are kept in `%APPDATA%\SenseSim\rtls.json`.
- **Assistant / MCP.** `rtls_add_device`, `rtls_update_device`, `rtls_add_zone`, `rtls_run`,
  `rtls_status`, `rtls_coverage`, `rtls_optimize_anchors`, `rtls_configure_outputs`, …
- **Headless regression test.** This loads the layout, simulates 60 s and writes a JSON report:

      SenseSim.exe --rtls-test examples/rtls/warehouse.json --duration 60 --report out.json

### One-button deployment, built-in OpenEMS (RTLS v2)

The steps run across two workspaces, each opening a card beside the tree: *Deploy* is in **Plan**, with
devices & zones, inspector, planning and settings; *Test live* and *OpenEMS Edge* are in **Simulation**, with
live data, RF analysis and outputs:

1. **Deploy.** Pick UWB / BLE / RFID and an accuracy target, then click **Auto deploy**. It places:
   - UWB anchors: on the ceiling first, then columns/walls, then rack tops and aisle faces. Never in mid-air: a scene with none of those gets nothing placed and is told to add them.
     It adds anchors until ≥95 % of the walkable floor has a usable fix (3 anchors heard, in line of sight or
     not, 4 when racks are present, with 95 % of readings predicted within 3× the target) and, across that floor,
     95 % of readings are predicted within the target. The plan is then spread out with swap refinement.
   - BLE beacons.
   - A smart-shelf RFID antenna inside every rack, plus inventory tags on each shelf level.
   - A reader per zone elsewhere.
   - UWB tags on vehicles, RFID tags on goods, and workers walking A* paths through the aisles.

   A 30 s simulation then checks the result. The target is judged on the *simulated* p95 of each technology,
   moving and parked tags apart, an update with no fix counting as a miss, and anchors are added where it misses,
   up to three times. This samples the routes the tags took, not the floor. The result shows its sample counts and
   availability. Running it again replaces only what it placed.

   **Floor map** (shown as soon as any anchor, beacon or reader is placed, by hand or by Auto deploy):
   - **Quality:** green where at least N % of position readings (75 % by default) land within the accuracy
     target. The target is the one picked above (e.g. 30 cm). The panel shows the share of the floor that passes.
   - **Accuracy:** predicted error in cm.
   - **Signal:** scanned signal strength.

   The map rescans by itself when you add, move or switch off infrastructure.
2. **Test live.** Run/pause, speed, tags located, error, alarms.
3. **OpenEMS.** **Install & start OpenEMS** downloads a portable Java 21 and the official `openems-edge.jar`
   into `%LOCALAPPDATA%\SenseSim\openems` (no admin rights). It preseeds a configuration: scheduler,
   `Controller.Api.Rest.ReadWrite` on :8084, a small energy simulation, and **`Rtls.SenseSim`** (bundle source
   in `tools/openems_rtls`, built with `tools/openems_rtls/build.sh`). It then starts OpenEMS hidden and
   connects the simulator. OpenEMS stops when Sense Sim closes. Felix console: http://127.0.0.1:8080
   (admin/admin).
   - `rtls0` channels: `TagsTotal`, `TagsLocated`, `TagsLost`, `InfraOnline`, `AlarmsActive`, `RmseCm`,
     `Cep95Cm`, `RfidReadsPerSec`, `Zone1..8Occupancy`.
   - OpenEMS can drive the simulation through `SimRunning`/`SimSpeedX10`.
   - **`Actuator1..8`** move the doors/gates/shutters of objects standing in zone N. Built-in rule: open while
     the zone is occupied. Set `Automation` = 0 to drive them yourself.
   - **`gpu0`** (`Gpu.SenseSim`) is the graphics card as a consumption meter. Its `ActivePower` is the GPU
     board power, so it shows up in the energy flow. The other channels are `PowerW`, `PowerLimitW`,
     `UtilizationPct`, `MemoryUtilPct`, `TemperatureC`, `MemoryUsedMb`/`MemoryTotalMb`, `ClockMhz`, `FanPct`,
     `GpuCount`, `RenderFps`, `RayTracing`, `VisionOnGpu` and `Online`. OpenEMS can write **`FpsLimit`** to cap
     Sense Sim's frame rate, and with it the GPU load (0 = no cap). Older installs get `gpu0` added automatically.

Devices are small 3D models (anchor housing, badge tag, beacon, reader antenna panel, RFID label). Drag them
onto racks, walls or the ceiling: they snap to the surface and face away from it. Shift+drag keeps the height,
Alt+drag changes it, and Q/E rotate.

Headless: `SenseSim.exe --rtls-test layout.json --rtls-deploy --duration 30 --report out.json`. It also
writes `out.layout.json` with the deployed devices.
