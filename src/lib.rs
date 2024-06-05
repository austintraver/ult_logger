#![feature(repr_simd)]
#![feature(simd_ffi)]

use lazy_static::lazy_static;
use ninput::Controller;
use serde::{Deserialize, Serialize};
use skyline;
use skyline::nn::time;
use smash::app::{sv_information, sv_module_access, sv_system, FighterInformation, Item};
use smash::app::{FighterEntryID, Fighter_get_id_from_entry_id};
use smash::lib::lua_const::*;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{atomic::AtomicU32, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

lazy_static! {
    static ref FILE_PATH: Mutex<String> = Mutex::new(String::new());
    static ref FIGHTER_LOG_COUNT: Mutex<usize> = Mutex::new(0);
    static ref BUFFER: Mutex<String> = Mutex::new(String::new());
    static ref FIGHTER_1: Mutex<String> = Mutex::new(String::new());
    static ref FIGHTER_2: Mutex<String> = Mutex::new(String::new());
}

#[repr(simd)]
pub struct SimdVector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

extern "C" {
    #[link_name = "\u{1}_ZN3app14sv_information27get_remaining_time_as_frameEv"]
    pub fn get_remaining_time_as_frame() -> u32;

    #[link_name = "\u{1}_ZN3app14sv_information8stage_idEv"]
    pub fn get_stage_id() -> i32;

    #[link_name = "\u{1}_ZN3app17sv_camera_manager7get_posEv"]
    pub fn get_camera_pos() -> SimdVector3;

    #[link_name = "\u{1}_ZN3app17sv_camera_manager16get_internal_posEv"]
    pub fn get_internal_camera_pos() -> SimdVector3;

    #[link_name = "\u{1}_ZN3app17sv_camera_manager10get_targetEv"]
    pub fn get_camera_target() -> SimdVector3;

    #[link_name = "\u{1}_ZN3app17sv_camera_manager7get_fovEv"]
    pub fn get_camera_fov() -> f32;
}

// 0 - we haven't started logging.
// 1 - we are actively logging.
// 2 - we have finished logging.
static LOGGING_STATE: AtomicU32 = AtomicU32::new(0);

static mut FIGHTER_MANAGER_ADDR: usize = 0;
static mut ITEM_MANAGER_ADDR: usize = 0;

#[skyline::hook(replace = smash::lua2cpp::L2CFighterBase_global_reset)]
fn on_match_start_or_end(fighter: &mut smash::lua2cpp::L2CFighterBase) -> smash::lib::L2CValue {
    println!(
        "[ult-logger] Hit on_match_start_or_end with logging_state = {}",
        LOGGING_STATE.load(Ordering::SeqCst)
    );
    let fighter_manager = unsafe { *(FIGHTER_MANAGER_ADDR as *mut *mut smash::app::FighterManager) };
    let is_ready_go = unsafe { smash::app::lua_bind::FighterManager::is_ready_go(fighter_manager) };
    let is_result_mode = unsafe { smash::app::lua_bind::FighterManager::is_result_mode(fighter_manager) };

    if !is_ready_go && !is_result_mode && LOGGING_STATE.load(Ordering::SeqCst) != 1 {
        // We are in the starting state, it's time to create a log.
        println!("[ult-logger] Starting");
        LOGGING_STATE.store(1, Ordering::SeqCst);
    }

    if is_result_mode && LOGGING_STATE.load(Ordering::SeqCst) == 1 {
        println!("[ult-logger] Flushing to log!");
        LOGGING_STATE.store(2, Ordering::SeqCst);

        let mut buffer = BUFFER.lock().unwrap();

        let mut file_path = FILE_PATH.lock().unwrap();

        let fighter1 = FIGHTER_1.lock().unwrap();

        let fighter2 = FIGHTER_2.lock().unwrap();

        unsafe {
            time::Initialize();
        }
        let event_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis();
        *file_path = format!("sd:/fight-{}-vs-{}-{}.txt", fighter1, fighter2, event_time);
        File::create(&*file_path);

        let file = OpenOptions::new()
            .write(true)
            .append(true)
            .open(&*file_path);

        let mut file = match file {
            Err(e) => panic!("Couldn't open file: {}", e),
            Ok(file) => file,
        };

        if let Err(e) = write!(file, "{}", buffer.as_str()) {
            panic!("Couldn't write to file: {}", e);
        }

        println!("[ult-logger] Wrote to {}", file_path.to_string());
        // Clear the buffer after writing
        buffer.clear();
    }

    original!()(fighter)
}

macro_rules! actionable_statuses {
    () => {
        vec![
            FIGHTER_STATUS_TRANSITION_TERM_ID_CONT_ESCAPE_AIR,
            FIGHTER_STATUS_TRANSITION_TERM_ID_CONT_ATTACK_AIR,
            FIGHTER_STATUS_TRANSITION_TERM_ID_CONT_GUARD_ON,
            FIGHTER_STATUS_TRANSITION_TERM_ID_CONT_ESCAPE,
        ]
    };
}

unsafe fn can_act(module_accessor: *mut smash::app::BattleObjectModuleAccessor) -> bool {
    smash::app::lua_bind::CancelModule::is_enable_cancel(module_accessor)
        || actionable_statuses!().iter().any(|actionable_transition| {
            smash::app::lua_bind::WorkModule::is_enable_transition_term(module_accessor, **actionable_transition)
        })
}

#[derive(Serialize, Deserialize, Debug)]
struct Coordinate {
    x: f32,
    y: f32,
    z: f32,
}

// The X position ranges between -1 and 1.
// -1.0 is left, 1.0 is right
// The Y position ranges between -1 and 1.
// -1.0 is full-down, 1.0 is full-up.
#[derive(Serialize, Deserialize, Debug)]
struct StickPosition {
    x: f32,
    y: f32,
}

#[derive(Serialize, Deserialize, Debug)]
struct LogEntry {
    num_frames_left: u32,
    fighter_id: i32,
    fighter_name: i32,
    stock_count: u8,
    status_kind: i32,
    motion_kind: u64,
    damage: f32,
    shield_size: f32,
    facing: f32,
    pos_x: f32,
    pos_y: f32,
    hitstun_left: f32,
    attack_connected: bool,
    animation_frame_num: f32,
    can_act: bool,
    camera_position: Coordinate,
    camera_target_position: Coordinate,
    camera_fov: f32,
    stage_id: i32,
}

pub fn once_per_frame_per_fighter(fighter: &mut smash::lua2cpp::L2CFighterCommon) {
    let mut fighter_log_count = FIGHTER_LOG_COUNT.lock().unwrap();
    *fighter_log_count += 1;

    let mut buffer = BUFFER.lock().unwrap();

    let lua_state = fighter.lua_state_agent;
    let module_accessor = unsafe { sv_system::battle_object_module_accessor(lua_state) };
    let fighter_manager = unsafe { *(FIGHTER_MANAGER_ADDR as *mut *mut smash::app::FighterManager) };
    let fighter_entry_id = unsafe { smash::app::lua_bind::WorkModule::get_int(module_accessor, *FIGHTER_INSTANCE_WORK_ID_INT_ENTRY_ID) as i32 };
    let fighter_skin = unsafe { smash::app::lua_bind::WorkModule::get_int(module_accessor, *FIGHTER_INSTANCE_WORK_ID_INT_COLOR) as i32 };
    let fighter_kind = unsafe { smash::app::utility::get_kind(module_accessor) };
    let is_ready_go = unsafe { smash::app::lua_bind::FighterManager::is_ready_go(fighter_manager) };
    let is_training_mode = unsafe { smash::app::smashball::is_training_mode() };


    unsafe {
        if !is_ready_go && !is_training_mode {
            dbg!(!is_ready_go);
            println!("[ult-logger] Game has not started yet");
            return;
        }
        // log_stick_inputs(fighter);
        // log_button_presses(fighter);
        // log_status(fighter);
        // log_item(fighter);

        // Have a 1/60 chance of triggering the code below:
        if let rand = rand::random::<u8>() % 60 {
            log_status(fighter);
            log_all_items(fighter);
        }
    }
}

unsafe fn log_entry(fighter: &mut smash::lua2cpp::L2CFighterCommon) {

        let boma = smash::app::sv_system::battle_object_module_accessor(fighter.lua_state_agent);

        let fighter_manager = *(FIGHTER_MANAGER_ADDR as *mut *mut smash::app::FighterManager);
        // Get the fighter's battle object module accessor.

        let num_frames_left = get_remaining_time_as_frame();

        let fighter_id = smash::app::lua_bind::WorkModule::get_int(
            boma,
            *smash::lib::lua_const::FIGHTER_INSTANCE_WORK_ID_INT_ENTRY_ID,
        );

        let fighter_information = smash::app::lua_bind::FighterManager::get_fighter_information(
            fighter_manager,
            smash::app::FighterEntryID(fighter_id)
        ) as *mut FighterInformation;



        let stock_count = smash::app::lua_bind::FighterInformation::stock_count(fighter_information) as u8;
        let fighter_status_kind = smash::app::lua_bind::StatusModule::status_kind(boma);
        let fighter_name = smash::app::utility::get_kind(boma);
        let fighter_motion_kind = smash::app::lua_bind::MotionModule::motion_kind(boma);
        let fighter_damage = smash::app::lua_bind::DamageModule::damage(boma, 0);
        let fighter_shield_size = smash::app::lua_bind::WorkModule::get_float(
            boma,
            *FIGHTER_INSTANCE_WORK_ID_FLOAT_GUARD_SHIELD,
        );
        let attack_connected = smash::app::lua_bind::AttackModule::is_infliction_status(
            boma,
            *COLLISION_KIND_MASK_HIT,
        );
        let hitstun_left = smash::app::lua_bind::WorkModule::get_float(
            boma,
            *FIGHTER_INSTANCE_WORK_ID_FLOAT_DAMAGE_REACTION_FRAME,
        );
        let can_act = can_act(boma);
        let pos_x = smash::app::lua_bind::PostureModule::pos_x(boma);
        let pos_y = smash::app::lua_bind::PostureModule::pos_y(boma);
        let facing = smash::app::lua_bind::PostureModule::lr(boma);
        let cam_pos = get_camera_pos();
        let cam_target = get_camera_target();
        let camera_fov = get_camera_fov();
        let stage_id = get_stage_id();
        let animation_frame_num = smash::app::lua_bind::MotionModule::frame(boma);

        // TODO: make a suggestion: renaming 'fighter ID' to 'player ID'
        //  for instance, in my replay, I saw that P1 has fighter ID 0 and P2 has fighter ID 1
        //  but the characters also have IDs, for instance, peach has ID 13, and mii swordfighter has ID 73
        //  but currently, the character ID is named 'fighter_name'
        if fighter_id == 0 {
            let mut fighter1 = FIGHTER_1.lock().unwrap();
            *fighter1 = format!("{}", fighter_name);
        }

        if fighter_id == 1 {
            let mut fighter2 = FIGHTER_2.lock().unwrap();
            *fighter2 = format!("{}", fighter_name);
        }

        let entry = LogEntry {
            num_frames_left,
            fighter_id,
            fighter_name,
            stock_count,
            status_kind: fighter_status_kind,
            motion_kind: fighter_motion_kind,
            damage: fighter_damage,
            shield_size: fighter_shield_size,
            facing,
            pos_x,
            pos_y,
            hitstun_left,
            attack_connected,
            animation_frame_num,
            can_act,
            camera_position: Coordinate {
                x: cam_pos.x,
                y: cam_pos.y,
                z: cam_pos.z,
            },
            camera_target_position: Coordinate {
                x: cam_target.x,
                y: cam_target.y,
                z: cam_target.z,
            },
            camera_fov,
            stage_id,
        };

        // let log_line = serde_json::to_string(&log_entry).unwrap();
        // println!("{}", log_line);
        // buffer.push_str(&format!("{}\n", log_line));

}

// Log the current position of the player's control stick.
unsafe fn log_stick_inputs(fighter: &mut smash::lua2cpp::L2CFighterCommon) {

    let boma = smash::app::sv_system::battle_object_module_accessor(fighter.lua_state_agent);
    let x = smash::app::lua_bind::ControlModule::get_stick_x(boma);
    let y = smash::app::lua_bind::ControlModule::get_stick_y(boma);
    println!("(in-game): x: {:.2}, y: {:.2}", x, y);

    return;
}

// Log current controller stick inputs.
unsafe fn log_realtime_stick_inputs(fighter: &mut smash::lua2cpp::L2CFighterCommon) {

    if let Some(controller) = Controller::get_from_id(0) {
        let left_stick = controller.left_stick;
        println!("#{}: x: {:.2}, y: {:.2}", 1, left_stick.x, left_stick.y);
    }

    return;
}

// Log the current buttons being pressed by the player.
unsafe fn log_button_presses(fighter: &mut smash::lua2cpp::L2CFighterCommon) {
    let boma = smash::app::sv_system::battle_object_module_accessor(fighter.lua_state_agent);

    // Check which buttons are being pressed by the player:
    let pressing_attack =
        smash::app::lua_bind::ControlModule::check_button_on(boma, *CONTROL_PAD_BUTTON_ATTACK);
    let pressing_special =
        smash::app::lua_bind::ControlModule::check_button_on(boma, *CONTROL_PAD_BUTTON_SPECIAL);
    let pressing_shield =
        smash::app::lua_bind::ControlModule::check_button_on(boma, *CONTROL_PAD_BUTTON_GUARD);
    let pressing_jump =
        smash::app::lua_bind::ControlModule::check_button_on(boma, *CONTROL_PAD_BUTTON_JUMP);
    let pressing_grab =
        smash::app::lua_bind::ControlModule::check_button_on(boma, *CONTROL_PAD_BUTTON_CATCH);

    println!(
        "Attack: {}, Special: {}, Shield: {}, Jump: {}, Grab: {}",
        pressing_attack, pressing_special, pressing_shield, pressing_jump, pressing_grab
    );
    return;
}

unsafe fn log_status(fighter: &mut smash::lua2cpp::L2CFighterBase) {
    let boma = smash::app::sv_system::battle_object_module_accessor(fighter.lua_state_agent);
    if smash::app::lua_bind::StatusModule::status_kind(boma) == FIGHTER_PEACH_STATUS_KIND_UNIQ_FLOAT {
        println!("Peach is floating.");
    }
    if smash::app::lua_bind::StatusModule::status_kind(boma) == FIGHTER_STATUS_KIND_CLIFF_WAIT {
        println!("Hanging on the ledge.");
    }
    if smash::app::lua_bind::StatusModule::status_kind(boma) == FIGHTER_STATUS_KIND_ITEM_THROW {
        println!("Peach is throwing an item.");
    }

    return;
}

unsafe fn log_item(fighter: &mut smash::lua2cpp::L2CFighterBase) {
    let boma = smash::app::sv_system::battle_object_module_accessor(fighter.lua_state_agent);

    // If the fighter doesn't have an item, return.
    if !smash::app::lua_bind::ItemModule::is_have_item(boma, 0) {
        return;
    }
    let item_kind = smash::app::lua_bind::ItemModule::get_have_item_kind(boma, 0);

    if item_kind == *ITEM_KIND_PEACHDAIKON {
        // let item_id = ItemModule::get_have_item_id(fighter.module_accessor, 0);
        // println!("Peach is holding a turnip with item ID {}", item_id);
        // println!("Diakon #1: {}", *ITEM_VARIATION_PEACHDAIKON_1);
        // println!("Diakon #2: {}", *ITEM_VARIATION_PEACHDAIKON_2);
        // println!("Diakon #3: {}", *ITEM_VARIATION_PEACHDAIKON_3);
        // println!("Diakon #4: {}", *ITEM_VARIATION_PEACHDAIKON_4);
        // println!("Diakon #5: {}", *ITEM_VARIATION_PEACHDAIKON_5);
        // println!("Diakon #6: {}", *ITEM_VARIATION_PEACHDAIKON_6);
        // println!("Diakon #7: {}", *ITEM_VARIATION_PEACHDAIKON_7);
        // println!("Diakon #8: {}", *ITEM_VARIATION_PEACHDAIKON_8);
    }
    if item_kind == *ITEM_KIND_BOMBHEI {
        println!("Peach is holding a bomb.");
    }
    if item_kind == *ITEM_KIND_DOSEISAN {
        println!("Peach is holding a Mr. Saturn.");
    }
    return;
}

unsafe fn log_all_items(fighter: &mut smash::lua2cpp::L2CFighterCommon) {
    let fighter_boma = smash::app::sv_system::battle_object_module_accessor(fighter.lua_state_agent);

    let item_manager = *(ITEM_MANAGER_ADDR as *mut *mut smash::app::ItemManager);
    (0..smash::app::lua_bind::ItemManager::get_num_of_active_item_all(item_manager)).for_each(|item_idx| {
        let item = smash::app::lua_bind::ItemManager::get_active_item(item_manager, item_idx);
        if item != 0 {
            let item = item as *mut Item;
            let item_battle_object_id = smash::app::lua_bind::Item::get_battle_object_id(item) as u32;
            let article_boma = smash::app::sv_system::battle_object_module_accessor(item_battle_object_id.into());
            dbg!(article_boma);
        }
    });
}

#[repr(C)]
#[derive(Debug)]
pub struct FixedBaseString<const N: usize> {
    fnv: u32,
    string_len: u32,
    string: [u8; N],
}

#[repr(C)]
#[derive(Debug)]
pub struct SceneQueue {
    end: *const u64,
    start: *const u64,
    count: usize,
    active_scene: FixedBaseString<64>,
    previous_scene: FixedBaseString<64>,
}

// Use this for general per-frame weapon-level hooks
// Reference: https://gist.github.com/jugeeya/27b902865408c916b1fcacc486157f79
pub fn once_per_weapon_frame(fighter_base: &mut smash::lua2cpp::L2CFighterBase) {
    unsafe {
        let boma = smash::app::sv_system::battle_object_module_accessor(fighter_base.lua_state_agent);
        println!(
            "[ult-logger] Frame : {}",
            smash::app::lua_bind::MotionModule::frame(boma)
        );
    }
}

fn nro_main(nro: &skyline::nro::NroInfo<'_>) {
    match nro.name {
        "common" => {
            skyline::install_hooks!(on_match_start_or_end);
        }
        _ => (),
    }
}

#[skyline::main(name = "ult_logger")]
pub fn main() {
    println!("[ult-logger] !!! v16 !!!");

    unsafe {
        skyline::nn::ro::LookupSymbol(
            &mut FIGHTER_MANAGER_ADDR,
            "_ZN3lib9SingletonIN3app14FighterManagerEE9instance_E\u{0}"
                .as_bytes()
                .as_ptr(),
        );
    }

    skyline::nro::add_hook(nro_main).unwrap();

    acmd::add_custom_hooks!(once_per_frame_per_fighter);

    std::panic::set_hook(Box::new(|info| {
        let location = info.location().unwrap();

        let msg = match info.payload().downcast_ref::<&'static str>() {
            Some(s) => *s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => &s[..],
                None => "Box<Any>",
            },
        };

        let err_msg = format!("thread has panicked at '{}', {}", msg, location);
        skyline::error::show_error(
                69,
                "Skyline plugin has panicked! Please open the details and send a screenshot to the developer, then close the game.\n\0",
                err_msg.as_str()
            );
    }));

    // input::init();
}
