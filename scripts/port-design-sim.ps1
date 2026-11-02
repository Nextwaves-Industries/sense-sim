# Brings the Design workspace and simulation work into a Sense Sim checkout, including the newer UI
# shell (ribbon, top bar, trees). Run it from anywhere, naming the checkout to change. Safe to repeat:
# each edit checks whether it is already there, and it stops if a line it needs has moved.
#
#     powershell -ExecutionPolicy Bypass -File scripts\port-design-sim.ps1 -Target C:\path\to\checkout

param([Parameter(Mandatory = $true)][string]$Target)

$ErrorActionPreference = 'Stop'
$source = Split-Path -Parent $PSScriptRoot
if (-not (Test-Path (Join-Path $Target 'Cargo.toml'))) { throw "$Target is not a Sense Sim checkout" }

function Copy-Tree($rel, [string[]]$exclude = @()) {
    $from = Join-Path $source $rel
    $to = Join-Path $Target $rel
    $args = @($from, $to, '/E', '/NFL', '/NDL', '/NJH', '/NJS', '/NP', '/XD') + @('target', 'target-b', 'target-c', 'target-d', 'target-e', 'target-check') + $exclude
    & robocopy @args | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "copying $rel failed" }
    $global:LASTEXITCODE = 0
}

# Replaces one line's text, keeping the file's line endings. `$marker` says it's already done.
function Edit-File($rel, $find, $replace, $marker) {
    $path = Join-Path $Target $rel
    $text = [IO.File]::ReadAllText($path)
    if ($text.Contains($marker)) { return }
    $crlf = $text.Contains("`r`n")
    $normal = $text.Replace("`r`n", "`n")
    if (-not $normal.Contains($find)) { throw "${rel}: can't find: $find" }
    $normal = $normal.Replace($find, $replace)
    if ($crlf) { $normal = $normal.Replace("`n", "`r`n") }
    [IO.File]::WriteAllText($path, $normal)
}

Copy-Tree 'crates\sim-core'
Copy-Tree 'third_party\solidcraft'
Copy-Tree 'src\design'
Copy-Tree 'src\sim'
New-Item -ItemType Directory -Force (Join-Path $Target 'docs'), (Join-Path $Target 'examples') | Out-Null
foreach ($f in 'examples\design_host.rs', 'examples\s4_corpus.rs', 'docs\design-and-simulation.md') { Copy-Item (Join-Path $source $f) (Join-Path $Target $f) -Force }

# --- Cargo.toml. Other work adds to these sections too, so this merges rather than assumes them.
$examples = "[[example]]`nname = `"design_host`"`nrequired-features = [`"design`"]`n`n[[example]]`nname = `"s4_corpus`"`nrequired-features = [`"design`"]`n`n"
$designFeature = "# The embedded SolidCraft CAD workspace. Off = faster dev builds, no CAD or simulation.`ndesign = [`"dep:solidcraft-ui-egui`", `"dep:solidcraft-engine`", `"dep:solidcraft-gpu`", `"dep:egui`", `"dep:egui-wgpu`", `"dep:pollster`"]`n"
$feature = "default = [`"design`"]`n$designFeature"
$manifest = [IO.File]::ReadAllText((Join-Path $Target 'Cargo.toml'))
$normalManifest = $manifest.Replace("`r`n", "`n")
$existingDefault = [regex]::Match($normalManifest, '(?m)^default = \[(.*)\]$')
if ($normalManifest.Contains('dep:solidcraft-ui-egui')) {
    # Already ported.
} elseif ($existingDefault.Success) {
    # Other work has defaults of its own: join them, with one `default` line.
    $list = $existingDefault.Groups[1].Value.Trim()
    $joined = if ($list) { "$list, `"design`"" } else { '"design"' }
    Edit-File 'Cargo.toml' "default = [$list]`n" "default = [$joined]`n$designFeature" 'dep:solidcraft-ui-egui'
    Edit-File 'Cargo.toml' "[features]`n" "${examples}[features]`n" '"design_host"'
} elseif ($normalManifest.Contains('[features]')) {
    Edit-File 'Cargo.toml' "[features]`n" "${examples}[features]`n$feature" 'dep:solidcraft-ui-egui'
} else {
    Edit-File 'Cargo.toml' "[dependencies]`n" "${examples}[features]`n$feature`n[dependencies]`n" 'dep:solidcraft-ui-egui'
}
Edit-File 'Cargo.toml' "[dependencies]`n" "[dependencies]`nsim-core = { path = `"crates/sim-core`" }`nsolidcraft-engine = { path = `"third_party/solidcraft/crates/engine`", optional = true }`nsolidcraft-gpu = { path = `"third_party/solidcraft/crates/gpu`", optional = true }`nsolidcraft-ui-egui = { path = `"third_party/solidcraft/crates/ui-egui`", optional = true }`negui = { version = `"0.36`", default-features = false, optional = true }`negui-wgpu = { version = `"0.36`", optional = true }`npollster = { version = `"0.4`", optional = true }`n" 'sim-core = {'
# A workspace of its own must leave SolidCraft's out: that tree is a workspace already.
if ($manifest.Contains('[workspace]')) {
    Edit-File 'Cargo.toml' 'members = [' "exclude = [`"third_party`"]`nmembers = [`"crates/sim-core`", " 'exclude = ["third_party"]'
} else {
    Edit-File 'Cargo.toml' "[[bin]]`n" "[workspace]`nexclude = [`"third_party`"]`nmembers = [`"crates/sim-core`"]`n`n[[bin]]`n" 'exclude = ["third_party"]'
}
$profiles = (Get-Content (Join-Path $source 'Cargo.toml') -Raw).Replace("`r`n", "`n")
$a = $profiles.IndexOf("# truck's debug-only checks")
$b = $profiles.IndexOf('[profile.release]')
Edit-File 'Cargo.toml' '[profile.release]' ($profiles.Substring($a, $b - $a) + '[profile.release]') 'truck-base]'
$a = $profiles.IndexOf("# SolidCraft's local fixes")
$b = $profiles.IndexOf('# Ties the managed OpenEMS')
Edit-File 'Cargo.toml' '# Ties the managed OpenEMS' ($profiles.Substring($a, $b - $a) + '# Ties the managed OpenEMS') 'patch.crates-io'

# --- src/main.rs
Edit-File 'src\main.rs' "mod context_menu;`n" "mod context_menu;`n#[cfg(feature = `"design`")]`nmod design;`n" 'mod design;'
Edit-File 'src\main.rs' "mod scene;`n" "mod scene;`n#[cfg(feature = `"design`")]`nmod sim;`n" 'mod sim;'
Edit-File 'src\main.rs' 'pub const APP_NAME: &str = "Sense Sim";' "pub const APP_NAME: &str = `"Sense Sim`";`n`n/// File > Quit: with a design workspace, through its protection of unsaved work.`npub fn quit() {`n    #[cfg(feature = `"design`")]`n    design::request_quit();`n    #[cfg(not(feature = `"design`"))]`n    std::process::exit(0);`n}" 'pub fn quit()'
Edit-File 'src\main.rs' '    while let Some(a) = args.next() {' "    let mut design_test: Option<String> = None;`n    let mut sim_test: Option<String> = None;`n    while let Some(a) = args.next() {" 'let mut sim_test'
Edit-File 'src\main.rs' '            "--plant-test" => plant_test = args.next(),' "            `"--design-test`" => design_test = args.next(),`n            `"--sim-test`" => sim_test = args.next(),`n            `"--sim-test-all`" => {}`n            `"--plant-test`" => plant_test = args.next()," '"--sim-test"'
Edit-File 'src\main.rs' '    app.world_mut().resource_mut::<Library>().search = search;' "    #[cfg(feature = `"design`")]`n    {`n        app.add_plugins((design::DesignPlugin, sim::SimPlugin));`n        if let Some(out) = &sim_test {`n            design::use_test_autosave();`n            app.insert_resource(sim::autotest::SimTest::new(out.clone())).add_systems(Update, (sim::autotest::run.before(sim::persist::peek_actions), sim::autotest::tools_step));`n            app.world_mut().resource_mut::<ui::UiState>().skip_start(None);`n        }`n        if let Some(out) = &design_test {`n            design::use_test_autosave();`n            app.insert_resource(design::autotest::DesignTest::new(out.clone())).add_systems(Update, design::autotest::run);`n            app.world_mut().resource_mut::<ui::UiState>().skip_start(None);`n        }`n    }`n    #[cfg(not(feature = `"design`"))]`n    let _ = (&design_test, &sim_test);`n    app.world_mut().resource_mut::<Library>().search = search;" 'design::DesignPlugin'

# Long tests stay out of sight of whoever is using this desktop.
Edit-File 'src\main.rs' 'visible: rtls_test.is_none() && plant_test.is_none(),' 'visible: rtls_test.is_none() && plant_test.is_none() && std::env::var_os("SENSESIM_TEST_HIDDEN").is_none(),' 'SENSESIM_TEST_HIDDEN'

# --- src/agent/tools.rs: the assistant's design and simulation tools.
Edit-File 'src\agent\tools.rs' "    v.extend(crate::plant::tools::defs());`n" "    v.extend(crate::plant::tools::defs());`n    #[cfg(feature = `"design`")]`n    v.extend(crate::sim::tools::defs());`n" 'crate::sim::tools::defs()'
Edit-File 'src\agent\tools.rs' "        other => ToolOutput::err(format!(`"unknown tool '{other}'`")),`n    };`n    Some(out)" "        #[cfg(feature = `"design`")]`n        other if other.starts_with(`"design_`") || other.starts_with(`"sim_`") => crate::sim::tools::start(world, other, i),`n        other => ToolOutput::err(format!(`"unknown tool '{other}'`")),`n    };`n    Some(out)" 'crate::sim::tools::start'

# --- src/gpu.rs: SolidCraft's dependencies compile in wgpu's OpenGL backend.
Edit-File 'src\gpu.rs' '                Backend::Auto => {}' "                // Explicit: SolidCraft's dependencies compile in wgpu's OpenGL backend, and `"any`"`n                // would then let it be chosen, which is the mix SolidCraft reports crashing.`n                Backend::Auto => s.backends = Some(Backends::VULKAN | Backends::DX12)," 'Backends::VULKAN | Backends::DX12'
Edit-File 'src\gpu.rs' 'instance.enumerate_adapters(Backends::all())' 'instance.enumerate_adapters(Backends::VULKAN | Backends::DX12)' 'enumerate_adapters(Backends::VULKAN'

# --- src/ui: the requests the workspaces listen for, and the way in.
Edit-File 'src\ui\mod.rs' '    pub rtls_alerts: u32,' "    pub rtls_alerts: u32,`n    /// Asks for the Simulations panel.`n    pub open_simulations: bool,`n    /// A simulation result is on show: the empty-scene hint would cover it.`n    pub results_shown: bool," 'pub open_simulations'
Edit-File 'src\ui\mod.rs' 'object_count == 0 && place.asset.is_none() && !walls.active' 'object_count == 0 && place.asset.is_none() && !walls.active && !slot.results_shown' 'results_shown {'

# The way in. The Simulations panel joins the Simulation workbench's Solvers group. Design is a
# workbench of its own: the shell's tab appears once the design system says it is in this build.
$buttons = "    if theme::ribbon_button(ui, icon::STACK, `"Simulations`", true, false, `"Create and run simulations: structural, flow, thermal, electromagnetic`").clicked() {`n        slot.open_simulations = true;`n    }"
$ribbon = Join-Path $Target 'src\ui\ribbon.rs'
if (Test-Path $ribbon) {
    if (-not (Test-Path (Join-Path $Target 'src\ui\workbench.rs'))) { throw "this checkout's ribbon has no workbenches yet" }
    $rf = 'go(ui, icon::WAVE_SINE, "RF solver", "Simulate antennas and materials with the openEMS field solver", Page::Fdtd);'
    Edit-File 'src\ui\ribbon.rs' $rf "$rf`n$buttons" 'slot.open_simulations'
    Edit-File 'src\ui\topbar.rs' 'std::process::exit(0);' 'crate::quit();' 'crate::quit()'

    # The workspace is shown in the workbench's canvas, under the top bar and ribbon, and not over the
    # whole window. `Workspace` and the workbench hold the same fact: whichever changed last wins.
    $d = 'src\design\mod.rs'
    Edit-File $d "    if std::mem::take(&mut panel.open_design) {`n        *workspace = Workspace::Design;`n    }`n" "    panel.bench.design_ready = true;`n    if workspace.is_changed() {`n        panel.design_open = *workspace == Workspace::Design;`n    } else {`n        workspace.set_if_neq(if panel.design_open { Workspace::Design } else { Workspace::Scene });`n    }`n" 'panel.bench.design_ready = true'
    Edit-File $d 'let full = ctx.content_rect();' 'let full = panel.studio_rect.unwrap_or(ctx.content_rect());' 'panel.studio_rect'
    # Send and Licences move to the ribbon.
    Edit-File $d "    let mut send = false;`n" "    let send = std::mem::take(&mut panel.design_send);`n    slot.want_notices |= std::mem::take(&mut panel.design_licences);`n" 'panel.design_send'
    $strip = 'send = ui.add_enabled(slot.host.is_some(), egui::Button::new(crate::theme::label(crate::theme::icon::CUBE, "Send to scene"))).on_hover_text("Place this design''s bodies in the scene").clicked();'
    Edit-File $d "                    $strip`n                    if ui.button(`"Licences`").clicked() {`n                        slot.want_notices = true;`n                    }`n" '' '"design_strip")).order(egui::Order::Middle)'
    # The workbench tabs are the way back, and the shell's design strip takes SolidCraft's application
    # bar's place, its commands going through `shell.rs`.
    Edit-File $d 'const SHELL_HAS_WAY_BACK: bool = false;' 'const SHELL_HAS_WAY_BACK: bool = true;' 'SHELL_HAS_WAY_BACK: bool = true'
    Copy-Item (Join-Path $source 'scripts\design_shell.rs') (Join-Path $Target 'src\design\shell.rs') -Force
    Edit-File $d "pub mod input;`n" "pub mod input;`npub mod shell;`n" 'pub mod shell;'
    Edit-File $d "    h.resize(size);`n" "    h.resize(size);`n    if shell::exchange(&mut panel, &ctx, h, rect) {`n        *next_repaint = None;`n    }`n" 'shell::exchange'
    # Under the ribbon's menus and the shell's dialogs.
    Edit-File $d '"design_strip")).order(egui::Order::Foreground)' '"design_strip")).order(egui::Order::Middle)' '"design_strip")).order(egui::Order::Middle)'
    Edit-File $d '"design_workspace")).order(egui::Order::Foreground)' '"design_workspace")).order(egui::Order::Middle)' '"design_workspace")).order(egui::Order::Middle)'
    # Leaving from inside: `draw` won't see its own change to `Workspace` as one.
    Edit-File $d "        *workspace = Workspace::Scene;`n        return Ok(());" "        *workspace = Workspace::Scene;`n        panel.design_open = false;`n        return Ok(());" 'panel.design_open = false'
    # The test's clicks follow the workspace into the canvas.
    $t = 'src\design\autotest.rs'
    Edit-File $t "    scene_undo: Res<crate::undo::UndoStack>,`n) {`n    test.frame += 1;`n" "    scene_undo: Res<crate::undo::UndoStack>,`n    panel: Res<crate::ui::LeftPanelSlot>,`n) {`n    let new_sketch = panel.studio_rect.map_or(Vec2::ZERO, |r| Vec2::new(r.left(), r.top())) + NEW_SKETCH;`n    test.frame += 1;`n" 'let new_sketch'
    Edit-File $t 'position: NEW_SKETCH' 'position: new_sketch' 'position: new_sketch'
    # A checkout whose spawns can start locked: these aren't.
    if ((Get-Content (Join-Path $Target 'src\scene.rs') -Raw) -match 'struct PendingSpawn \{[^}]*pub locked: bool') {
        Edit-File $d 'joints: None, select: i == 0 }' 'joints: None, select: i == 0, locked: false }' 'select: i == 0, locked: false'
        Edit-File 'src\sim\autotest.rs' 'name: Some("Plank".into()), joints: None, select: false }' 'name: Some("Plank".into()), joints: None, select: false, locked: false }' 'select: false, locked: false'
    }
    # The test goes through the design strip too, with real clicks: see `test_step` in `shell.rs`.
    Edit-File $t "    panel: Res<crate::ui::LeftPanelSlot>,`n" "    mut panel: ResMut<crate::ui::LeftPanelSlot>,`n    status: Res<crate::scene::Status>,`n" 'status: Res<crate::scene::Status>'
    Edit-File $t 'if step == 3 || step == 24 {' 'if step == 3 || step == 24 || super::shell::TEST_PRESSES.contains(&step) {' 'TEST_PRESSES'
    Edit-File $t "        10 => {}`n" "        10 => test.step = super::shell::TEST_FIRST,`n        super::shell::TEST_FIRST..=super::shell::TEST_LAST => {`n            let out = test.out.clone();`n            let check = super::shell::test_step(step, &mut slot, &mut panel, &mut workspace, &status, &mut commands, &mut window_events, window, &out, &mut test.undo_before);`n            if let Some((name, pass, detail)) = check {`n                test.check(name, pass, detail);`n            }`n            if step == super::shell::TEST_LAST {`n                test.step = 11;`n            }`n        }`n" 'TEST_FIRST..='
    # The Simulations panel opens in the canvas, clear of the ribbon, the card and the scene tree.
    Edit-File 'src\sim\panel.rs' 'default_pos([70.0, 70.0])' 'default_pos([ctx.content_rect().right() - 640.0, 176.0])' 'right() - 640.0'
    # The Simulations panel belongs to its workbench.
    Edit-File 'src\sim\mod.rs' "    if *workspace != Workspace::Scene {`n        return Ok(());`n    }`n    if state.panel_open {" "    if *workspace != Workspace::Scene || panel_slot.bench.current != crate::ui::Workbench::Simulation {`n        return Ok(());`n    }`n    if state.panel_open {" 'crate::ui::Workbench::Simulation'
} else {
    Edit-File 'src\ui\mod.rs' 'std::process::exit(0);' 'crate::quit();' 'crate::quit()'
}
Write-Host "Ported into $Target"
