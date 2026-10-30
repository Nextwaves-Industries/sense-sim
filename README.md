# Sense Sim

![Sense Sim](assets/logo_wordmark.png)

Sense Sim is a Rust (Bevy + egui) viewer and scene builder for the NVIDIA
SimReady USD dataset. It reads USD with the pure-Rust `openusd` crate, so no
C++ or Python USD install is needed.

## Install

Run `dist\SenseSim-Setup-1.2.0.exe`. It installs per-user (no admin) into
`%LOCALAPPDATA%\Programs\Sense Sim`, adds a Start Menu entry (desktop shortcut
optional) and an uninstaller in *Apps & features*.

The app uses `D:\simready-assets` if it exists, otherwise
`%USERPROFILE%\SenseSim\simready-assets`. Pick another folder with the `…`
button; the choice is remembered.

## Use

- **Start screen:** every session starts with *Create a scene*. Name it and create an empty
  scene, open a saved layout, or pick one of 9 samples, each card showing a render of the scene:
  *UWB warehouse* (40 × 28 m, 72 racks, 13 anchors) and *RFID warehouse* (12 smart shelves,
  51 readers, 172 tags), one positioning technology each; *Hospital ward*, *Auto workshop*,
  *Office*, *Canteen*; and the plant systems *Pump station*, *Engine drive*, *Conveyor line*.
  File → *New scene…* (Ctrl+N) brings it back. `--sample N` (1–9) loads a sample directly.
  `tools/make_previews.sh` re-renders the card pictures (`--sample N --preview --screenshot`).
- **Left rail:** switches the left panel between *Library*, *Scene* (all placed objects,
  filterable, hover a row for focus / duplicate / delete) and *Downloads*. Click the active
  icon again to collapse the panel. The gear opens **Settings** (dataset folder, viewport and
  lighting, texture quality, AI assistant).
- **Library:** search, filter *All / On disk / Online*, switch grid or list view, click a card,
  then click in the scene to place it. Online assets have a blue name and a cloud badge; clicking
  one downloads it and then places it. After a search, *Get N* queues every online match.
  Downloads run two at a time and show progress on the card and in the Downloads tab.
  The folder is rescanned every 15 s.
- **Viewport toolbar (top centre):** multi-place, undo/redo, frame all, grid, hover bounds, shadows.
- **Placing:** Q/E rotate the preview (Shift: 90 deg). Shift or *Multi-place* keeps placing.
  Esc or right-click cancels. Objects drop onto whatever is under the cursor.
- **Select and move:** click an object, drag it across the floor or onto other objects
  (Alt keeps height, Ctrl snaps).
  - **Orange ring:** rotate.
  - **Green arrow:** lift. A lifted (or Alt-moved) object stays in the air and gravity leaves it alone;
    **Drop** (Properties or right-click) lets it go, and it lands on whatever is under it.
  - **White dots:** moving parts (doors, drawers, wheels). Drag a dot to move that joint
    within its limits; the label shows the angle or travel.
- **Properties (right):** rename, quick actions (focus, duplicate, drop, reset or
  rotate, delete), X/Y/Z position, rotation and scale, sliders for every moving part, load info.
- **Camera:** right-drag orbit, middle-drag or Shift+right-drag pan, wheel zoom,
  WASD/Space/C move, F focus. *Shortcuts* in the status bar lists all keys.
- **File menu:** save (Ctrl+S) or open (Ctrl+O) layouts (JSON, joint poses included), or export
  a Y-up `.usda` that references the original assets.
- Fonts: Geist (SIL OFL, `assets/fonts/Geist-LICENSE.txt`) and Phosphor Icons (MIT).
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
(`ANTHROPIC_API_KEY`, default `claude-opus-5-5`), OpenAI (`OPENAI_API_KEY`), OpenRouter
(`OPENROUTER_API_KEY`), and local Ollama / LM Studio / any OpenAI-compatible server (no key).
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
  - A hydraulic solve covers pipe losses, valves, pump curves and tank levels, and shows open
    ends as leaks.
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

The **Positioning** tab on the left rail has three steps:

1. **Deploy.** Pick UWB / BLE / RFID and an accuracy target, then click **Auto deploy**. It places:
   - UWB anchors: on the ceiling first, then columns/walls, then rack tops and aisle faces, then the room perimeter.
     It adds anchors until ≥95 % of the walkable floor has 3 anchors in line of sight (4 when racks are
     present) and the target is met. The plan is then spread out with swap refinement.
   - BLE beacons.
   - A smart-shelf RFID antenna inside every rack, plus inventory tags on each shelf level.
   - A reader per zone elsewhere.
   - UWB tags on vehicles, RFID tags on goods, and workers walking A* paths through the aisles.

   A 30 s simulation then checks the result. The target is judged on the *simulated* UWB p95, and the plan is
   redone once if it misses. Running it again replaces only what it placed.

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
