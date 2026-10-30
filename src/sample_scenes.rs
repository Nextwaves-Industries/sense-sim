//! Object lists of the larger sample scenes, generated (rows of racks, desk
//! clusters, ward bays) rather than written out one by one. Coordinates are
//! meters on the floor (x, z); yaw in degrees (0 = the asset as authored,
//! 90 turns its +z towards +x); y > 0 drops the object onto what is under it.

use crate::samples::Item;

fn put(v: &mut Vec<Item>, folder: &'static str, x: f32, z: f32, yaw: f32) {
    v.push((folder, x, z, yaw, 0.0));
}

/// Placed `y` above the floor and dropped onto what is below.
fn drop_on(v: &mut Vec<Item>, folder: &'static str, x: f32, z: f32, yaw: f32, y: f32) {
    v.push((folder, x, z, yaw, y));
}

/// 40 × 28 m hall (x 0..40, z 0..28): three double rows of racks in two
/// blocks with a cross aisle, pallet staging, a packing area and dock gates.
pub fn uwb_warehouse() -> Vec<Item> {
    let mut v = Vec::new();
    let rows: [(f32, [&'static str; 2]); 3] =
        [(3.5, ["rack_composition_a04", "rack_composition_a05"]), (9.5, ["rack_composition_a05", "rack_composition_a04"]), (15.5, ["rack_bulk_storage_a03", "rack_bulk_storage_a03"])];
    for (z0, kinds) in rows {
        for block in [2.5f32, 22.5] {
            for i in 0..6 {
                let x = block + 1.25 + i as f32 * 2.5;
                // Back to back: each faces its own aisle.
                put(&mut v, kinds[i % 2], x, z0 - 0.66, 180.0);
                put(&mut v, kinds[(i + 1) % 2], x, z0 + 0.66, 0.0);
            }
        }
    }
    // Pallet staging in front of the racks.
    for (j, z) in [20.5f32, 22.3].into_iter().enumerate() {
        for i in 0..8 {
            put(&mut v, if (i + j) % 2 == 0 { "pallet_assembly_a05" } else { "pallet_assembly_a06" }, 3.0 + i as f32 * 1.6, z, 0.0);
        }
    }
    // Dock gates on the open front edge.
    for x in [24.0, 29.0, 34.0] {
        put(&mut v, "barrier_gate_safetyDock_a02", x, 27.6, 0.0);
    }
    // Packing area: crates, large boxes, drums, desks.
    for (x, z) in [(24.8, 19.6), (25.9, 19.6), (24.8, 20.7)] {
        put(&mut v, "crate_plywood_b01", x, z, 0.0);
    }
    for i in 0..4 {
        put(&mut v, "box_cardboard_large_a02", 28.0 + i as f32 * 1.1, 19.8, 0.0);
    }
    for i in 0..3 {
        for j in 0..2 {
            put(&mut v, "drum_steel_a01", 34.0 + i as f32 * 0.75, 19.4 + j as f32 * 0.75, 0.0);
        }
    }
    put(&mut v, "cabinet_drum_flammable_a02", 38.9, 19.4, 0.0);
    put(&mut v, "cabinet_drum_flammable_a02", 38.9, 20.4, 0.0);
    for x in [30.0, 32.0, 34.0] {
        put(&mut v, "desk_a01", x, 23.6, 90.0);
        put(&mut v, "chair_office_amf_black_a01", x, 24.5, 180.0);
        drop_on(&mut v, "computer_monitor_office_a01", x, 23.5, 0.0, 1.0);
    }
    // Vehicles (they carry the tags).
    put(&mut v, "cart_mobile_a01", 8.0, 6.5, 0.0);
    put(&mut v, "cart_mobile_a01", 28.0, 12.5, 0.0);
    put(&mut v, "cart_mobile_b01", 12.0, 25.0, 0.0);
    put(&mut v, "cart_mobile_b01", 31.0, 17.4, 90.0);
    put(&mut v, "equipment_robot_trolley_a01", 20.0, 24.0, 0.0);
    put(&mut v, "equipment_robot_trolley_a01", 26.0, 6.5, 90.0);
    put(&mut v, "fireExtinguisher_stand_a01", 18.2, 19.0, 0.0);
    put(&mut v, "fireExtinguisher_stand_a01", 0.4, 12.5, 0.0);
    put(&mut v, "construction_firstAidPoint_trolley_a01", 0.5, 19.0, 90.0);
    v
}

/// Steel columns of the UWB warehouse: in the cross aisle at the row lines and
/// along the two open sides of the hall.
pub const UWB_COLUMNS: &[(f32, f32)] = &[
    (20.0, 3.5),
    (20.0, 9.5),
    (20.0, 15.5),
    (40.3, 4.0),
    (40.3, 12.0),
    (40.3, 20.0),
    (40.3, 27.5),
    (4.0, 28.3),
    (12.0, 28.3),
    (20.0, 28.3),
];

/// 30 × 20 m (x 0..30, z 0..20): a 4 × 3 grid of smart shelves, a dock with
/// stocked pallets and crates, and a packing line.
pub fn rfid_warehouse() -> Vec<Item> {
    let mut v = Vec::new();
    for x in [3.0f32, 7.0, 11.0, 15.0] {
        for z in [3.0f32, 7.0, 11.0] {
            put(&mut v, "storage_shelf_basic_a01", x, z, 0.0);
        }
    }
    // Dock: pallets with boxes, crates, carts, gates on the open side.
    for x in [22.5f32, 24.5] {
        for z in [3.0f32, 5.2, 7.4] {
            put(&mut v, "pallet_rackable_blue_a01", x, z, 0.0);
            drop_on(&mut v, "box_cardboard_a03", x - 0.2, z, 0.0, 0.6);
            drop_on(&mut v, "box_cardboard_b02", x + 0.35, z, 0.0, 0.6);
        }
    }
    for z in [3.0f32, 4.2, 5.4, 6.6] {
        put(&mut v, "crate_plywood_b01", 27.3, z, 0.0);
    }
    put(&mut v, "cart_mobile_a01", 23.5, 11.0, 0.0);
    put(&mut v, "cart_mobile_a01", 25.5, 11.0, 0.0);
    for z in [4.5f32, 10.0, 15.5] {
        put(&mut v, "barrier_gate_safetyDock_a02", 29.8, z, 90.0);
    }
    // Packing line.
    for x in [4.0f32, 6.0, 8.0, 10.0] {
        put(&mut v, "desk_a01", x, 17.0, 90.0);
        put(&mut v, "chair_office_amf_black_a01", x, 17.9, 180.0);
    }
    for x in [13.0f32, 14.2, 15.4] {
        put(&mut v, "box_cardboard_large_a02", x, 17.0, 0.0);
    }
    for x in [22.5f32, 23.3, 24.1] {
        put(&mut v, "drum_plastic_blue_a01", x, 13.6, 0.0);
    }
    put(&mut v, "fireExtinguisher_stand_a01", 19.5, 17.5, 0.0);
    v
}

pub const RFID_COLUMNS: &[(f32, f32)] = &[(19.5, 2.0), (19.5, 8.0), (19.5, 14.0), (30.3, 1.0), (30.3, 19.0), (8.0, 20.3), (16.0, 20.3), (24.0, 20.3)];

/// Interior walls: (x, z, length, yaw, height).
pub const HOSPITAL_WALLS: &[(f32, f32, f32, f32, f32)] = &[
    (0.0, -4.0, 6.0, 90.0, 2.4),
    // Corridor, north side: doors into the wards at x = ±6.
    (-9.35, -1.0, 5.3, 0.0, 2.4),
    (0.0, -1.0, 10.6, 0.0, 2.4),
    (9.35, -1.0, 5.3, 0.0, 2.4),
    // South side: nurse station, supply room, lounge.
    (-10.35, 2.5, 3.3, 0.0, 2.4),
    (-4.5, 2.5, 5.6, 0.0, 2.4),
    (3.0, 2.5, 6.6, 0.0, 2.4),
    (9.85, 2.5, 4.3, 0.0, 2.4),
    (-4.0, 4.75, 4.5, 90.0, 2.4),
    (2.0, 4.75, 4.5, 90.0, 2.4),
];

/// 24 × 14 m (x -12..12, z -7..7): two six-bed wards, a corridor, nurse
/// station, supply room and family lounge.
pub fn hospital() -> Vec<Item> {
    let mut v = Vec::new();
    // Ward A (west) and ward B (east): beds head to the back wall, IV pole
    // and visitor chair beside each.
    for i in 0..6 {
        let x = -11.0 + i as f32 * 1.9;
        put(&mut v, "bed_hospital_standard_a01", x, -5.7, 90.0);
        put(&mut v, if i % 2 == 0 { "IV_pole_a01" } else { "IV_pole_a02" }, x + 0.75, -6.5, 0.0);
        put(&mut v, "chair_hospital_a01", x + 0.85, -4.0, 0.0);
        let x = 1.5 + i as f32 * 1.9;
        put(&mut v, "bed_hospital_standard_a02", x, -5.7, 90.0);
        put(&mut v, if i % 2 == 0 { "IV_pole_a02" } else { "IV_pole_a01" }, x - 0.75, -6.5, 0.0);
        put(&mut v, "chair_hospital_b01", x - 0.85, -4.0, 0.0);
    }
    put(&mut v, "cabinet_for_drugs_a02", -11.6, -2.2, 0.0);
    put(&mut v, "cart_medical_utility_a01", -3.0, -2.0, 0.0);
    put(&mut v, "cabinet_for_drugs_a02", 11.6, -2.2, 0.0);
    put(&mut v, "cart_medical_utility_a02", 3.0, -2.0, 0.0);
    // Corridor.
    put(&mut v, "chair_patient_mobile_a01", -2.0, 0.6, 90.0);
    put(&mut v, "chair_patient_mobile_a01", 8.5, 0.5, 90.0);
    put(&mut v, "cart_medical_utility_a03", 2.8, 1.7, 90.0);
    put(&mut v, "fireExtinguisher_stand_a01", -11.6, 0.8, 0.0);
    put(&mut v, "construction_firstAidPoint_trolley_a01", 11.5, 0.7, 90.0);
    // Nurse station.
    put(&mut v, "fixture_counter_a01", -8.0, 4.4, 90.0);
    for x in [-8.6f32, -7.4] {
        put(&mut v, "chair_office_fabric_manager_a01", x, 5.5, 180.0);
        drop_on(&mut v, "computer_monitor_office_a01", x, 4.4, 180.0, 1.2);
    }
    put(&mut v, "cabinet_for_drugs_a02", -11.6, 3.5, 0.0);
    put(&mut v, "cabinet_for_drugs_a02", -11.6, 5.0, 0.0);
    put(&mut v, "cabinet_file_a01", -5.0, 6.7, 90.0);
    put(&mut v, "cabinet_file_a02", -5.9, 6.7, 90.0);
    put(&mut v, "bin_waste_a02", -4.5, 3.0, 0.0);
    // Supply room.
    for z in [3.5f32, 5.2] {
        put(&mut v, "storage_shelf_basic_b01", -3.6, z, 0.0);
        put(&mut v, "storage_shelf_basic_b01", 1.6, z, 0.0);
    }
    put(&mut v, "cart_medical_utility_a01", -1.6, 4.6, 0.0);
    put(&mut v, "cart_medical_utility_a02", -0.4, 4.6, 0.0);
    // Family lounge.
    for x in [3.0f32, 3.7, 4.4, 5.1, 8.0, 8.7, 9.4, 10.1] {
        put(&mut v, "chair_office_amf_black_a01", x, 3.2, 0.0);
    }
    put(&mut v, "furniture_table_coffee_low_a01", 4.0, 4.6, 90.0);
    put(&mut v, "furniture_table_coffee_low_a01", 9.0, 4.6, 90.0);
    put(&mut v, "facilityEquipment_beverageFridge_display_a01", 6.5, 3.0, 0.0);
    put(&mut v, "digitalSystem_infoKiosk_vertical_a01", 11.6, 6.0, -90.0);
    put(&mut v, "container_recycle_blue_wheeled_a02", 11.5, 3.2, 0.0);
    v
}

pub const WORKSHOP_WALLS: &[(f32, f32, f32, f32, f32)] = &[(-4.0, -6.5, 3.0, 90.0, 1.8), (4.0, -6.5, 3.0, 90.0, 1.8)];

/// 24 × 16 m (x -12..12, z -8..8): three service bays with a car frame and
/// an engine on a lift table each, workbenches, parts storage, compressor
/// corner.
pub fn workshop() -> Vec<Item> {
    let mut v = Vec::new();
    let engines = ["engineAssembly_v8_engine_a01", "engineAssembly_v6_engine_a01", "engineAssembly_v12_engine_a01"];
    for (k, bx) in [-8.0f32, 0.0, 8.0].into_iter().enumerate() {
        put(&mut v, "carFrame_car_frame_assembled_a01", bx, -4.6, 90.0);
        put(&mut v, "equipment_liftTable_scissorMobile_a01", bx + 2.4, -3.0, 90.0);
        drop_on(&mut v, engines[k], bx + 2.4, -3.0, 0.0, 1.4);
        put(&mut v, "equipment_mechanic_tool_cart_g01", bx - 2.0, -5.0, 0.0);
        put(&mut v, "equipment_rolling_tool_cabinet_heavyDuty_b01", bx + 2.3, -7.4, 90.0);
    }
    // Parts storage.
    for z in [0.8f32, 3.2, 5.6] {
        put(&mut v, "storage_tire_rack_b01", -11.4, z, 90.0);
    }
    put(&mut v, "storage_shelf_basic_a01", -9.3, 1.5, 0.0);
    put(&mut v, "storage_shelf_basic_a01", -9.3, 4.3, 0.0);
    put(&mut v, "storage_tire_rack_a01", -9.3, 6.9, 0.0);
    // Workbench line.
    for i in 0..6 {
        put(&mut v, "furniture_workbench_modular_a01", -4.5 + i as f32 * 1.5, 1.2, -90.0);
    }
    put(&mut v, "equipment_rolling_tool_cabinet_a01", 5.0, 1.2, 90.0);
    put(&mut v, "equipment_mechanic_tool_cart_g01", -2.0, 2.7, 90.0);
    // Engines waiting on pallets.
    for (i, e) in engines.into_iter().enumerate() {
        let x = -4.0 + i as f32 * 1.6;
        put(&mut v, "pallet_rackable_blue_a01", x, 5.0, 0.0);
        drop_on(&mut v, e, x, 5.0, 0.0, 0.4);
    }
    // Compressor and fluids corner.
    put(&mut v, "equipment_airCompressor_a01", 10.5, 0.7, 0.0);
    put(&mut v, "equipment_parts_washer_cabinet_b01", 11.4, 2.5, 0.0);
    for (x, z) in [(9.2, 4.0), (9.9, 4.0), (9.2, 4.7), (9.9, 4.7)] {
        put(&mut v, "drum_steel_a01", x, z, 0.0);
    }
    put(&mut v, "cabinet_drum_flammable_a02", 11.5, 4.3, 0.0);
    put(&mut v, "cabinet_drum_flammable_a02", 11.5, 5.3, 0.0);
    put(&mut v, "container_recycle_blue_wheeled_a02", 11.4, 7.2, 0.0);
    put(&mut v, "fireExtinguisher_stand_a01", 7.5, 7.4, 0.0);
    put(&mut v, "fireExtinguisher_stand_a01", -11.6, 7.5, 0.0);
    put(&mut v, "climate_shopFan_pedestal_a01", 6.0, 3.5, 0.0);
    v
}

pub const OFFICE_WALLS: &[(f32, f32, f32, f32, f32)] = &[(4.0, -4.25, 5.5, 90.0, 2.4), (7.6, -1.5, 4.8, 0.0, 2.4)];

/// 20 × 14 m (x -10..10, z -7..7): four clusters of six desks, a walled
/// meeting room, a printer corner and a lounge.
pub fn office() -> Vec<Item> {
    let mut v = Vec::new();
    for (cx, cz) in [(-6.5f32, -3.6f32), (-1.5, -3.6), (-6.5, 2.8), (-1.5, 2.8)] {
        for dz in [-1.8f32, 0.0, 1.8] {
            let z = cz + dz;
            // Back to back: the left desk's chair faces +x, the right one's -x.
            put(&mut v, "desk_a01", cx - 0.4, z, 0.0);
            put(&mut v, "chair_office_fabric_manager_a01", cx - 1.4, z, 90.0);
            drop_on(&mut v, "computer_monitor_office_a01", cx - 0.6, z, -90.0, 1.0);
            put(&mut v, "desk_a01", cx + 0.4, z, 0.0);
            put(&mut v, "chair_office_fabric_manager_a01", cx + 1.4, z, -90.0);
            drop_on(&mut v, "computer_monitor_office_a01", cx + 0.6, z, 90.0, 1.0);
        }
    }
    // Lockers and file cabinets along the walls.
    for z in [-1.5f32, -0.45, 0.6, 1.65] {
        put(&mut v, "facilityFurniture_lockerUnit_multiDoor_a01", -9.7, z, 0.0);
    }
    for i in 0..6 {
        put(&mut v, if i % 2 == 0 { "cabinet_file_a01" } else { "cabinet_file_a02" }, -8.6 + i as f32 * 0.85, -6.75, 90.0);
    }
    // Meeting room: two desks as a table, eight chairs, a screen.
    put(&mut v, "desk_a01", 6.1, -4.3, 90.0);
    put(&mut v, "desk_a01", 7.9, -4.3, 90.0);
    for x in [5.6f32, 6.6, 7.4, 8.4] {
        put(&mut v, "chair_office_amf_black_a01", x, -5.3, 0.0);
        put(&mut v, "chair_office_amf_black_a01", x, -3.3, 180.0);
    }
    put(&mut v, "digitalSystem_infoKiosk_vertical_a01", 9.6, -6.4, -90.0);
    // Printer corner.
    put(&mut v, "desk_a01", 2.3, -6.5, 90.0);
    drop_on(&mut v, "computer_printer_office_a01", 2.0, -6.5, 0.0, 1.0);
    drop_on(&mut v, "computer_printer_office_a01", 2.7, -6.5, 0.0, 1.0);
    put(&mut v, "bin_waste_a02", 2.6, -2.2, 0.0);
    // Lounge.
    for tx in [6.0f32, 8.8] {
        put(&mut v, "furniture_table_coffee_low_a01", tx, 3.5, 0.0);
        for dz in [-0.3f32, 0.3] {
            put(&mut v, "chair_office_amf_black_a01", tx - 0.7, 3.5 + dz, 90.0);
            put(&mut v, "chair_office_amf_black_a01", tx + 0.7, 3.5 + dz, -90.0);
        }
    }
    put(&mut v, "facilityEquipment_beverageFridge_display_a01", 9.6, 6.5, 0.0);
    put(&mut v, "container_recycle_blue_wheeled_a02", 9.5, 0.2, 0.0);
    put(&mut v, "bin_waste_a02", -4.0, 6.5, 0.0);
    v
}

/// 16 × 12 m (x -8..8, z -6..6): a kitchen line on the back wall, islands,
/// eight four-seat tables, drinks and recycling along the side.
pub fn canteen() -> Vec<Item> {
    let mut v = Vec::new();
    put(&mut v, "fixture_counter_a01", -6.0, -5.6, 90.0);
    put(&mut v, "appliance_refrigerator_a01", -3.9, -5.5, 90.0);
    put(&mut v, "facilitySystem_kitchenette_module_a01", -2.3, -5.6, 90.0);
    put(&mut v, "furniture_counter_coffeeStation_a01", -0.35, -5.6, 90.0);
    put(&mut v, "facilitySystem_beverageStation_breakArea_a01", 1.2, -5.65, 90.0);
    put(&mut v, "facilityEquipment_beverageFridge_display_a01", 2.2, -5.7, 90.0);
    put(&mut v, "facilityEquipment_beverageFridge_display_a01", 2.75, -5.7, 90.0);
    put(&mut v, "appliance_dishwasher_a01", 3.6, -5.6, 90.0);
    put(&mut v, "appliance_refrigerator_a01", 4.6, -5.5, 90.0);
    put(&mut v, "fixture_island_a01", -5.0, -3.4, 90.0);
    put(&mut v, "fixture_island_a01", -3.6, -3.4, 90.0);
    put(&mut v, "fixture_island_a01", 0.4, -3.4, 90.0);
    for z in [-0.5f32, 3.0] {
        for x in [-5.4f32, -1.8, 1.8, 5.4] {
            put(&mut v, "furniture_table_coffee_low_a01", x, z, 0.0);
            for dz in [-0.3f32, 0.3] {
                put(&mut v, "chair_office_amf_black_a01", x - 0.7, z + dz, 90.0);
                put(&mut v, "chair_office_amf_black_a01", x + 0.7, z + dz, -90.0);
            }
        }
    }
    put(&mut v, "container_recycle_blue_wheeled_a02", 7.5, -2.0, 0.0);
    put(&mut v, "container_recycle_blue_wheeled_a02", 7.5, -1.0, 0.0);
    put(&mut v, "bin_waste_a02", 7.6, 0.1, 0.0);
    put(&mut v, "digitalSystem_infoKiosk_vertical_a01", 7.6, 1.6, -90.0);
    put(&mut v, "fireExtinguisher_stand_a01", 7.6, 5.5, 0.0);
    put(&mut v, "facilitySystem_beverageStation_breakArea_a01", -7.7, 1.5, 0.0);
    put(&mut v, "facilityEquipment_beverageFridge_display_a01", -7.7, 2.6, 0.0);
    put(&mut v, "bin_waste_a02", -7.6, 3.4, 0.0);
    v
}
