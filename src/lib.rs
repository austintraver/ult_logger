#![feature(repr_simd)]
#![feature(simd_ffi)]

use {
    lazy_static::lazy_static,
    serde::{Serialize, Deserialize},
    skyline::{self, nn::time},
    smash::app,
    smash::app::{utility, lua_bind, FighterManager, FighterEntryID, FighterInformation, BattleObjectModuleAccessor},
    smash::app::lua_bind::*,
    smash::lib::{L2CValue, lua_const},
    smash::lib::lua_const::*,
    smash::lua2cpp::{L2CFighterBase, L2CFighterCommon, L2CFighterBase_global_reset},
    std::fs::{File, OpenOptions},
    std::io::Write,
    std::sync::Mutex,
    std::sync::atomic::{AtomicU32, Ordering},
    std::time::{SystemTime, UNIX_EPOCH},
};

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

static mut FIGHTER_MANAGER_ADDR: usize = 0;

// 0 - we haven't started logging.
// 1 - we are actively logging.
// 2 - we have finished logging.
static LOGGING_STATE: AtomicU32 = AtomicU32::new(0);

// This gets called whenever a match starts or ends. Still gets called once per fighter which is odd.
// A typical fight will have the following logs.
//   HIT on_match_start_or_end
//   HIT on_match_start_or_end
//   HIT on_match_start_or_end
//   HIT on_match_start_or_end
//   HIT on_match_start_or_end
//   In is_ready_go
//   HIT on_match_start_or_end
//   In is_ready_go
//   ...
//   HIT on_match_start_or_end
//   In is_ready_go
//   ... once the match ends ..
//   HIT on_match_start_or_end
//   In is_result_mode
//   HIT on_match_start_or_end
//   In is_result_mode
//   HIT on_match_start_or_end
//   In is_result_mode
#[skyline::hook(replace = L2CFighterBase_global_reset)]
pub fn on_match_start_or_end(fighter: &mut L2CFighterBase) -> L2CValue {
    println!("[ult-logger] Hit on_match_start_or_end with logging_state = {}", LOGGING_STATE.load(Ordering::SeqCst));
    let fighter_manager = unsafe { *(FIGHTER_MANAGER_ADDR as *mut *mut app::FighterManager) };
    let is_ready_go = unsafe { lua_bind::FighterManager::is_ready_go(fighter_manager) };
    let is_result_mode = unsafe { lua_bind::FighterManager::is_result_mode(fighter_manager) };

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

unsafe fn can_act(module_accessor: *mut BattleObjectModuleAccessor) -> bool {
    lua_bind::CancelModule::is_enable_cancel(module_accessor)
        || actionable_statuses!().iter().any(|actionable_transition| {
            WorkModule::is_enable_transition_term(
                module_accessor,
                **actionable_transition,
            )
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
    stick_position: StickPosition,
    pressing_attack: bool,
    pressing_special: bool,
    pressing_shield: bool,
    pressing_jump: bool,
    pressing_grab: bool,
}

pub fn once_per_frame_per_fighter(fighter: &mut L2CFighterCommon) {
    let mut fighter_log_count = FIGHTER_LOG_COUNT.lock().unwrap();
    *fighter_log_count += 1;

    let mut buffer = BUFFER.lock().unwrap();

    unsafe {
        let module_accessor =
            smash::app::sv_system::battle_object_module_accessor(fighter.lua_state_agent);

        let fighter_manager = *(FIGHTER_MANAGER_ADDR as *mut *mut FighterManager);

        // If True, the game has started and the characters can move around.  Otherwise, it's still loading with the
        // countdown.
        let game_started = lua_bind::FighterManager::is_ready_go(fighter_manager);

        if !game_started {
            // println!("[ult-logger] Game not ready yet");
            return;
        }

        let num_frames_left = get_remaining_time_as_frame();

        let fighter_id = lua_bind::WorkModule::get_int(
            module_accessor,
            *lua_const::FIGHTER_INSTANCE_WORK_ID_INT_ENTRY_ID,
        ) as i32;

        let fighter_information = lua_bind::FighterManager::get_fighter_information(
            fighter_manager,
            FighterEntryID(fighter_id),
        ) as *mut FighterInformation;
        let stock_count = lua_bind::FighterInformation::stock_count(fighter_information) as u8;
        let fighter_status_kind = lua_bind::StatusModule::status_kind(module_accessor);
        let fighter_name = utility::get_kind(module_accessor);
        let fighter_motion_kind = lua_bind::MotionModule::motion_kind(module_accessor);
        let fighter_damage = lua_bind::DamageModule::damage(module_accessor, 0);
        let fighter_shield_size = lua_bind::WorkModule::get_float(
            module_accessor,
            *lua_const::FIGHTER_INSTANCE_WORK_ID_FLOAT_GUARD_SHIELD,
        );
        let attack_connected = lua_bind::AttackModule::is_infliction_status(
            module_accessor,
            *lua_const::COLLISION_KIND_MASK_HIT,
        );
        let hitstun_left = lua_bind::WorkModule::get_float(
            module_accessor,
            *lua_const::FIGHTER_INSTANCE_WORK_ID_FLOAT_DAMAGE_REACTION_FRAME,
        );
        let can_act = can_act(module_accessor);
        let pos_x = lua_bind::PostureModule::pos_x(module_accessor);
        let pos_y = lua_bind::PostureModule::pos_y(module_accessor);
        let facing = lua_bind::PostureModule::lr(module_accessor);
        let cam_pos = get_camera_pos();
        let cam_target = get_camera_target();
        let camera_fov = get_camera_fov();
        let stage_id = get_stage_id();
        let animation_frame_num = smash::app::lua_bind::MotionModule::frame(module_accessor);

        if fighter_id == 0 {
            let mut fighter1 = FIGHTER_1.lock().unwrap();
            *fighter1 = format!("{}", fighter_name);
        }

        if fighter_id == 1 {
            let mut fighter2 = FIGHTER_2.lock().unwrap();
            *fighter2 = format!("{}", fighter_name);
        }

        let stick_position = StickPosition {
            x: ControlModule::get_stick_x(fighter.module_accessor),
            y: ControlModule::get_stick_y(fighter.module_accessor),
        };

        // Check which buttons are being pressed by the player:
        let pressing_attack = ControlModule::check_button_on(fighter.module_accessor, *CONTROL_PAD_BUTTON_ATTACK);
        let pressing_special = ControlModule::check_button_on(fighter.module_accessor, *CONTROL_PAD_BUTTON_SPECIAL);
        let pressing_shield = ControlModule::check_button_on(fighter.module_accessor, *CONTROL_PAD_BUTTON_GUARD);
        let pressing_jump = ControlModule::check_button_on(fighter.module_accessor, *CONTROL_PAD_BUTTON_JUMP);
        let pressing_grab = ControlModule::check_button_on(fighter.module_accessor, *CONTROL_PAD_BUTTON_CATCH);

        let log_entry = LogEntry {
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
            stick_position,
            pressing_attack,
            pressing_special,
            pressing_shield,
            pressing_jump,
            pressing_grab,
        };

        let log_line = serde_json::to_string(&log_entry).unwrap();
        println!("{}", log_line);
        buffer.push_str(&format!("{}\n", log_line));
    }
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
pub fn once_per_weapon_frame(fighter_base: &mut L2CFighterBase) {
    unsafe {
        let module_accessor =
            smash::app::sv_system::battle_object_module_accessor(fighter_base.lua_state_agent);
        println!("[ult-logger] Frame : {}", smash::app::lua_bind::MotionModule::frame(module_accessor));
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

    std::panic::set_hook(
        Box::new(|info| {
            let location = info.location().unwrap();

            let msg = match info.payload().downcast_ref::<&'static str>() {
                Some(s) => *s,
                None => {
                    match info.payload().downcast_ref::<String>() {
                        Some(s) => &s[..],
                        None => "Box<Any>",
                    }
                }
            };

            let err_msg = format!("thread has panicked at '{}', {}", msg, location);
            skyline::error::show_error(
                69,
                "Skyline plugin has panicked! Please open the details and send a screenshot to the developer, then close the game.\n\0",
                err_msg.as_str()
            );
        })
    );
}
