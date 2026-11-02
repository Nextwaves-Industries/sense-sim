---
title: OpenUSD and SimReady scene conventions
domain: structure
tags: OpenUSD, USD, SimReady, metersPerUnit, upAxis, kilogramsPerUnit, stage metadata, units, Y up, Z up, Isaac Sim, Omniverse, physics, UsdPhysics, rigid body, collider, collision, mass, joint, articulation, PhysicsScene, gravity, export_usda, scale, defaultPrim
aliases: đơn vị, mét, trục đứng, trục Y, trục Z, tỉ lệ, sai tỉ lệ, va chạm, khối lượng, vật lý, khớp, xuất USD, scene scale, asset scale, up axis, unit mismatch
verified: 2026-11-02
---
# OpenUSD and SimReady scene conventions

## metersPerUnit stage metadata

`metersPerUnit` is stage-level metadata that says how many metres one scene unit is: 0.01 means centimetres, 1.0 means metres [1][2]. If a stage authors no value, OpenUSD falls back to 0.01 [1][2]. It applies to world-space lengths such as points and extents, while raw attribute values and xform translations should not be read through it without care, since `xformOp:scale` changes the metric of authored values [1]. To get metres from an authored point: value_m = value x metersPerUnit (calculated), with any transforms applied. A wrong value shows up as an object 100 times too large or small.

## upAxis stage metadata

`upAxis` sets which axis points up in a stage. The only permitted values are "Y" and "Z" [2][3]. NVIDIA's learning material gives "Y" as the fallback when unauthored [2]. Like linear units, it is stage-wide, so it does not vary between referenced parts [1]. To bring a Z-up asset into a Y-up stage, rotate it -90 degrees about X, which maps +Z to +Y (standard rotation geometry, not from a cited source). Isaac Sim warns that an asset authored with another up axis imports rotated [4].

## Composition does not reconcile units

OpenUSD does not fix mismatched units or axes when one file references another [1][2]. `metersPerUnit`, `upAxis` and `kilogramsPerUnit` are not reconciled; values are copied literally, and only `timeCodesPerSecond` is scaled automatically [2]. Fixing it is the assembler's job [1]. NVIDIA's example: a 1 m cube authored with size 100 at 0.01 metres per unit looks 10 times too small when referenced into a stage at 0.001 metres per unit [2]. NVIDIA recommends setting one convention across an organisation, applying explicit scale correction on reference, normalising units at ingestion, and validating metadata before composition [2].

## SimReady Foundation unit requirements

NVIDIA's SimReady Foundation requires each stage to specify `upAxis` and `metersPerUnit`. The core units page additionally lists `upAxis` = "Z" (UN-006) and `metersPerUnit` = 1.0 (UN-007) [5]. `kilogramsPerUnit` must be specified if physics objects are present, and its sample stage uses 1 [5]. The page does not say which asset profiles apply these checks [5]. The older Kit asset requirements index lists the same items [6]. So real SimReady content is normally metres, kilograms and Z up, unlike Sense Sim's own Y-up stages. Sense Sim converts on load, so read sizes from the tools rather than from the file.

## SimReady physics requirements

The Kit asset requirements index states physics rules for assets [6]:

- Rigid bodies must be Xformable prims without skew, not inside a scene graph instance, and not nested unless the transform stack is reset.
- A static asset must not contain a rigid body, only colliders.
- Colliding geometry must apply the Collision API; mesh collision applies only to Mesh prims.
- Rigid bodies or their collision shapes should have a mass specification.
- Bodies that are not free-floating should be connected with joints, whose Body0 and Body1 targets must exist and be single.
- Kinematic chains should define an articulation, not on kinematic or static bodies, and not nested.

NVIDIA adds that a SimReady asset carries mass distribution, centre of gravity, friction and restitution, simplified collision meshes and semantic labels [7].

## UsdPhysics defaults for mass and gravity

UsdPhysics uses `metersPerUnit`, `timeCodesPerSecond` and `kilogramsPerUnit`, and angles are in degrees [8]. Gravity defaults to the negative stage up axis at Earth gravity [8]. A collider without a rigid body acts as static, with zero velocity and infinite mass [8]. Explicit mass overrides density, and with no density specified the default is 1000 kg/m³ [8], about water, so a hollow shelf modelled as solid gets far too heavy without a set mass (inference). A missing mass with no collision volume defaults to 1.0 [8]. A joint with one side unset attaches to the static world, and collisions between jointed bodies are off by default [8].

## Isaac Sim stage defaults

Isaac Sim's default stage is Z up, in metres; versions before 2022.1 used centimetres, and Omniverse Kit's own default remains centimetres, so USD values can look off by 100 times [4]. A new PhysicsScene sets gravity to 9.8 m/s² [4]. Create one through Create > Physics > Physics Scene [4]. CPU physics is suggested unless simulating hundreds of rigid bodies or robots, with GPU dynamics unchecked and broadphase set to MBP [4]. A Y-up stage from Sense Sim is therefore a mismatch that the importing user or a transform must handle. State this when the target is Isaac Sim.

## Sense Sim scene convention

Sense Sim's scene convention is metres, Y up, floor at y = 0. An asset's origin is the bottom centre of its bounding box, so y = 0 stands it on the floor. `yaw_deg` rotates about Y, and yaw 0 keeps the asset's default facing, +Z. For a photo, the camera stands above the origin looking towards -Z, so objects have negative z and +x is to the right. Generated structure and plant parts are written with `metersPerUnit` = 1 and `upAxis` = "Y", and the loader reads `upAxis` and `metersPerUnit` from any asset, rotating anything not marked Y up, so SimReady Z-up files load upright at true scale. A file with no `upAxis` is treated as Z up, so check it if it loads on its side. Those generated files author no `kilogramsPerUnit`, and plant joints are defined without rigid body or collision schemas, so they are not validated SimReady physics assets.

## Applying this in Sense Sim

Trust measured sizes. Call `get_asset_info` for the exact size in metres and `get_scene` for world sizes; never derive scale from raw USD numbers. Pass `size_m` to `search_assets` to find correctly sized assets. Avoid `scale` in `place_objects` and `update_object`, since a scaled asset no longer matches real dimensions. Use `verify_scene` to catch floating, sunk and intersecting objects, and report its numbers. `export_usda` writes a Y-up stage in metres that references the original assets; it opens in Isaac Sim, Omniverse and usdview. Tell the user the export is Y up where Isaac Sim's default is Z up, that a PhysicsScene is added in the target tool, and that generated shell pieces and plant parts carry no collision or mass, so physics-ready claims only cover library assets that provide them. `save_layout` keeps a Sense Sim layout JSON.

## Sources

1. UsdGeom linear units — OpenUSD — https://openusd.org/release/api/group___usd_geom_linear_units__group.html
2. Units in OpenUSD — NVIDIA Learn OpenUSD — https://docs.nvidia.com/learn-openusd/latest/beyond-basics/units.html
3. UsdGeom metrics header — OpenUSD — https://openusd.org/release/api/usd_geom_2metrics_8h_source.html
4. Tutorial 1: Stage Setup, Isaac Sim 6.2.0 — NVIDIA — https://docs.isaacsim.omniverse.nvidia.com/6.2.0/robot_setup_tutorials/tutorial_intro_environment_setup.html
5. Units requirements, SimReady Foundation 2026.07.1 — NVIDIA — https://nvidia.github.io/simready-foundation/2026.07.1/capabilities/core/units/requirements.html
6. Requirements Index, Asset Requirements 1.1.6 — NVIDIA Omniverse — https://docs.omniverse.nvidia.com/kit/docs/asset-requirements/1.1.6/indexes/requirements_index.html
7. What makes 3D assets simulation ready — NVIDIA — https://perspectives.nvidia.com/physical-ai/omniverse/task/blog/what-makes-3d-assets-simulation-ready-1/
8. Rigid Body Physics in USD proposal — OpenUSD — https://openusd.org/release/wp_rigid_body_physics.html
