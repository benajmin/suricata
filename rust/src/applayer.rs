/* Copyright (C) 2017-2021 Open Information Security Foundation
 *
 * You can copy, redistribute or modify this Program under the terms of
 * the GNU General Public License version 2 as published by the Free
 * Software Foundation.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * version 2 along with this program; if not, write to the Free Software
 * Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA
 * 02110-1301, USA.
 */

//! Parser registration functions and common interface

use std;
use crate::core::{self,DetectEngineState,Flow,AppLayerEventType,AppLayerDecoderEvents,AppProto};
use crate::filecontainer::FileContainer;
use crate::applayer;
use std::os::raw::{c_void,c_char,c_int};
use crate::core::SC;
use std::ffi::CStr;

#[repr(C)]
#[derive(Default, Debug,PartialEq)]
pub struct AppLayerTxConfig {
    /// config: log flags
    log_flags: u8,
}

impl AppLayerTxConfig {
    pub fn new() -> Self {
        Self {
            log_flags: 0,
        }
    }

    pub fn add_log_flags(&mut self, flags: u8) {
        self.log_flags |= flags;
    }
    pub fn set_log_flags(&mut self, flags: u8) {
        self.log_flags = flags;
    }
    pub fn get_log_flags(&self) -> u8 {
        self.log_flags
    }
}

#[repr(C)]
#[derive(Default, Debug,PartialEq)]
pub struct AppLayerTxData {
    /// config: log flags
    pub config: AppLayerTxConfig,

    /// logger flags for tx logging api
    logged: LoggerFlags,

    /// track file open/logs so we can know how long to keep the tx
    pub files_opened: u32,
    pub files_logged: u32,
    pub files_stored: u32,

    /// detection engine flags for use by detection engine
    detect_flags_ts: u64,
    detect_flags_tc: u64,
}

impl AppLayerTxData {
    pub fn new() -> Self {
        Self {
            config: AppLayerTxConfig::new(),
            logged: LoggerFlags::new(),
            files_opened: 0,
            files_logged: 0,
            files_stored: 0,
            detect_flags_ts: 0,
            detect_flags_tc: 0,
        }
    }
    pub fn init_files_opened(&mut self) {
        self.files_opened = 1;
    }
    pub fn incr_files_opened(&mut self) {
        self.files_opened += 1;
    }
}

#[macro_export]
macro_rules!export_tx_data_get {
    ($name:ident, $type:ty) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(tx: *mut std::os::raw::c_void)
            -> *mut crate::applayer::AppLayerTxData
        {
            let tx = &mut *(tx as *mut $type);
            &mut tx.tx_data
        }
    }
}

#[repr(C)]
#[derive(Default,Debug,PartialEq,Copy,Clone)]
pub struct AppLayerResult {
    pub status: i32,
    pub consumed: u32,
    pub needed: u32,
}

impl AppLayerResult {
    /// parser has successfully processed in the input, and has consumed all of it
    pub fn ok() -> Self {
        Default::default()
    }
    /// parser has hit an unrecoverable error. Returning this to the API
    /// leads to no further calls to the parser.
    pub fn err() -> Self {
        return Self {
            status: -1,
            ..Default::default()
        };
    }
    /// parser needs more data. Through 'consumed' it will indicate how many
    /// of the input bytes it has consumed. Through 'needed' it will indicate
    /// how many more bytes it needs before getting called again.
    /// Note: consumed should never be more than the input len
    ///       needed + consumed should be more than the input len
    pub fn incomplete(consumed: u32, needed: u32) -> Self {
        return Self {
            status: 1,
            consumed: consumed,
            needed: needed,
        };
    }

    pub fn is_ok(self) -> bool {
        self.status == 0
    }
    pub fn is_incomplete(self) -> bool {
        self.status == 1
    }
    pub fn is_err(self) -> bool {
        self.status == -1
    }
}

impl From<bool> for AppLayerResult {
    fn from(v: bool) -> Self {
        if v == false {
            Self::err()
        } else {
            Self::ok()
        }
    }
}

impl From<i32> for AppLayerResult {
    fn from(v: i32) -> Self {
        if v < 0 {
            Self::err()
        } else {
            Self::ok()
        }
    }
}

/// Rust parser declaration
#[repr(C)]
pub struct RustParser {
    /// Parser name.
    pub name:               *const c_char,
    /// Default port
    pub default_port:       *const c_char,

    /// IP Protocol (core::IPPROTO_UDP, core::IPPROTO_TCP, etc.)
    pub ipproto:            c_int,

    /// Probing function, for packets going to server
    pub probe_ts:           Option<ProbeFn>,
    /// Probing function, for packets going to client
    pub probe_tc:           Option<ProbeFn>,

    /// Minimum frame depth for probing
    pub min_depth:          u16,
    /// Maximum frame depth for probing
    pub max_depth:          u16,

    /// Allocation function for a new state
    pub state_new:          StateAllocFn,
    /// Function called to free a state
    pub state_free:         StateFreeFn,

    /// Parsing function, for packets going to server
    pub parse_ts:           ParseFn,
    /// Parsing function, for packets going to client
    pub parse_tc:           ParseFn,

    /// Get the current transaction count
    pub get_tx_count:       StateGetTxCntFn,
    /// Get a transaction
    pub get_tx:             StateGetTxFn,
    /// Function called to free a transaction
    pub tx_free:            StateTxFreeFn,
    /// Progress values at which the tx is considered complete in a direction
    pub tx_comp_st_ts:      c_int,
    pub tx_comp_st_tc:      c_int,
    /// Function returning the current transaction progress
    pub tx_get_progress:    StateGetProgressFn,

    /// Function called to get a detection state
    pub get_de_state:       GetDetectStateFn,
    /// Function called to set a detection state
    pub set_de_state:       SetDetectStateFn,

    /// Function to get events
    pub get_events:         Option<GetEventsFn>,
    /// Function to get an event id from a description
    pub get_eventinfo:      Option<GetEventInfoFn>,
    /// Function to get an event description from an event id
    pub get_eventinfo_byid: Option<GetEventInfoByIdFn>,

    /// Function to allocate local storage
    pub localstorage_new:   Option<LocalStorageNewFn>,
    /// Function to free local storage
    pub localstorage_free:  Option<LocalStorageFreeFn>,

    /// Function to get files
    pub get_files:          Option<GetFilesFn>,

    /// Function to get the TX iterator
    pub get_tx_iterator:    Option<GetTxIteratorFn>,

    pub get_tx_data: GetTxDataFn,

    // Function to apply config to a TX. Optional. Normal (bidirectional)
    // transactions don't need to set this. It is meant for cases where
    // the requests and responses are not sharing tx. It is then up to
    // the implementation to make sure the config is applied correctly.
    pub apply_tx_config: Option<ApplyTxConfigFn>,

    pub flags: u32,

    /// Function to handle the end of data coming on one of the sides
    /// due to the stream reaching its 'depth' limit.
    pub truncate: Option<TruncateFn>,
}

/// Create a slice, given a buffer and a length
///
/// UNSAFE !
#[macro_export]
macro_rules! build_slice {
    ($buf:ident, $len:expr) => ( std::slice::from_raw_parts($buf, $len) );
}

/// Cast pointer to a variable, as a mutable reference to an object
///
/// UNSAFE !
#[macro_export]
macro_rules! cast_pointer {
    ($ptr:ident, $ty:ty) => ( &mut *($ptr as *mut $ty) );
}

pub type ParseFn      = unsafe extern "C" fn (flow: *const Flow,
                                       state: *mut c_void,
                                       pstate: *mut c_void,
                                       input: *const u8,
                                       input_len: u32,
                                       data: *const c_void,
                                       flags: u8) -> AppLayerResult;
pub type ProbeFn      = unsafe extern "C" fn (flow: *const Flow, flags: u8, input:*const u8, input_len: u32, rdir: *mut u8) -> AppProto;
pub type StateAllocFn = extern "C" fn (*mut c_void, AppProto) -> *mut c_void;
pub type StateFreeFn  = unsafe extern "C" fn (*mut c_void);
pub type StateTxFreeFn  = unsafe extern "C" fn (*mut c_void, u64);
pub type StateGetTxFn            = unsafe extern "C" fn (*mut c_void, u64) -> *mut c_void;
pub type StateGetTxCntFn         = unsafe extern "C" fn (*mut c_void) -> u64;
pub type StateGetProgressFn = unsafe extern "C" fn (*mut c_void, u8) -> c_int;
pub type GetDetectStateFn   = unsafe extern "C" fn (*mut c_void) -> *mut DetectEngineState;
pub type SetDetectStateFn   = unsafe extern "C" fn (*mut c_void, &mut DetectEngineState) -> c_int;
pub type GetEventInfoFn     = unsafe extern "C" fn (*const c_char, *mut c_int, *mut AppLayerEventType) -> c_int;
pub type GetEventInfoByIdFn = unsafe extern "C" fn (c_int, *mut *const c_char, *mut AppLayerEventType) -> i8;
pub type GetEventsFn        = unsafe extern "C" fn (*mut c_void) -> *mut AppLayerDecoderEvents;
pub type LocalStorageNewFn  = extern "C" fn () -> *mut c_void;
pub type LocalStorageFreeFn = extern "C" fn (*mut c_void);
pub type GetFilesFn         = unsafe
extern "C" fn (*mut c_void, u8) -> *mut FileContainer;
pub type GetTxIteratorFn    = unsafe extern "C" fn (ipproto: u8, alproto: AppProto,
                                             state: *mut c_void,
                                             min_tx_id: u64,
                                             max_tx_id: u64,
                                             istate: &mut u64)
                                             -> AppLayerGetTxIterTuple;
pub type GetTxDataFn = unsafe extern "C" fn(*mut c_void) -> *mut AppLayerTxData;
pub type ApplyTxConfigFn = unsafe extern "C" fn (*mut c_void, *mut c_void, c_int, AppLayerTxConfig);
pub type TruncateFn = unsafe extern "C" fn (*mut c_void, u8);


// Defined in app-layer-register.h
extern {
    pub fn AppLayerRegisterProtocolDetection(parser: *const RustParser, enable_default: c_int) -> AppProto;
    pub fn AppLayerRegisterParserAlias(parser_name: *const c_char, alias_name: *const c_char);
}

#[allow(non_snake_case)]
pub unsafe fn AppLayerRegisterParser(parser: *const RustParser, alproto: AppProto) -> c_int {
    (SC.unwrap().AppLayerRegisterParser)(parser, alproto)
}

// Defined in app-layer-detect-proto.h
extern {
    pub fn AppLayerProtoDetectPPRegister(ipproto: u8, portstr: *const c_char, alproto: AppProto,
                                         min_depth: u16, max_depth: u16, dir: u8,
                                         pparser1: ProbeFn, pparser2: ProbeFn);
    pub fn AppLayerProtoDetectPPParseConfPorts(ipproto_name: *const c_char, ipproto: u8,
                                               alproto_name: *const c_char, alproto: AppProto,
                                               min_depth: u16, max_depth: u16,
                                               pparser_ts: ProbeFn, pparser_tc: ProbeFn) -> i32;
    pub fn AppLayerProtoDetectPMRegisterPatternCSwPP(ipproto: u8, alproto: AppProto,
                                                     pattern: *const c_char, depth: u16,
                                                     offset: u16, direction: u8, ppfn: ProbeFn,
                                                     pp_min_depth: u16, pp_max_depth: u16) -> c_int;
    pub fn AppLayerProtoDetectConfProtoDetectionEnabled(ipproto: *const c_char, proto: *const c_char) -> c_int;
    pub fn AppLayerProtoDetectConfProtoDetectionEnabledDefault(ipproto: *const c_char, proto: *const c_char, default: bool) -> c_int;
}

// Defined in app-layer-parser.h
pub const APP_LAYER_PARSER_EOF_TS : u8 = BIT_U8!(5);
pub const APP_LAYER_PARSER_EOF_TC : u8 = BIT_U8!(6);
pub const APP_LAYER_PARSER_NO_INSPECTION : u8 = BIT_U8!(1);
pub const APP_LAYER_PARSER_NO_REASSEMBLY : u8 = BIT_U8!(2);
pub const APP_LAYER_PARSER_NO_INSPECTION_PAYLOAD : u8 = BIT_U8!(3);
pub const APP_LAYER_PARSER_BYPASS_READY : u8 = BIT_U8!(4);

pub const APP_LAYER_PARSER_OPT_ACCEPT_GAPS: u32 = BIT_U32!(0);
pub const APP_LAYER_PARSER_OPT_UNIDIR_TXS: u32 = BIT_U32!(1);

pub type AppLayerGetTxIteratorFn = unsafe extern "C" fn (ipproto: u8,
                                                  alproto: AppProto,
                                                  alstate: *mut c_void,
                                                  min_tx_id: u64,
                                                  max_tx_id: u64,
                                                  istate: &mut u64) -> applayer::AppLayerGetTxIterTuple;

extern {
    pub fn AppLayerParserStateSetFlag(state: *mut c_void, flag: u8);
    pub fn AppLayerParserStateIssetFlag(state: *mut c_void, flag: u8) -> c_int;
    pub fn AppLayerParserSetStreamDepth(ipproto: u8, alproto: AppProto, stream_depth: u32);
    pub fn AppLayerParserConfParserEnabled(ipproto: *const c_char, proto: *const c_char) -> c_int;
    pub fn AppLayerParserRegisterGetTxIterator(ipproto: u8, alproto: AppProto, fun: AppLayerGetTxIteratorFn);
    pub fn AppLayerParserRegisterOptionFlags(ipproto: u8, alproto: AppProto, flags: u32);
}

#[repr(C)]
pub struct AppLayerGetTxIterTuple {
    tx_ptr: *mut std::os::raw::c_void,
    tx_id: u64,
    has_next: bool,
}

impl AppLayerGetTxIterTuple {
    pub fn with_values(tx_ptr: *mut std::os::raw::c_void, tx_id: u64, has_next: bool) -> AppLayerGetTxIterTuple {
        AppLayerGetTxIterTuple {
            tx_ptr: tx_ptr, tx_id: tx_id, has_next: has_next,
        }
    }
    pub fn not_found() -> AppLayerGetTxIterTuple {
        AppLayerGetTxIterTuple {
            tx_ptr: std::ptr::null_mut(), tx_id: 0, has_next: false,
        }
    }
}

/// LoggerFlags tracks which loggers have already been executed.
#[repr(C)]
#[derive(Default, Debug,PartialEq)]
pub struct LoggerFlags {
    flags: u32,
}

impl LoggerFlags {

    pub fn new() -> Self {
        Default::default()
    }

    pub fn get(&self) -> u32 {
        self.flags
    }

    pub fn set(&mut self, bits: u32) {
        self.flags = bits;
    }

}

/// Export a function to get the DetectEngineState on a struct.
#[macro_export]
macro_rules!export_tx_get_detect_state {
    ($name:ident, $type:ty) => (
        #[no_mangle]
        pub unsafe extern "C" fn $name(tx: *mut std::os::raw::c_void)
            -> *mut core::DetectEngineState
        {
            let tx = cast_pointer!(tx, $type);
            match tx.de_state {
                Some(ds) => {
                    return ds;
                },
                None => {
                    return std::ptr::null_mut();
                }
            }
        }
    )
}

/// Export a function to set the DetectEngineState on a struct.
#[macro_export]
macro_rules!export_tx_set_detect_state {
    ($name:ident, $type:ty) => (
        #[no_mangle]
        pub unsafe extern "C" fn $name(tx: *mut std::os::raw::c_void,
                de_state: &mut core::DetectEngineState) -> std::os::raw::c_int
        {
            let tx = cast_pointer!(tx, $type);
            tx.de_state = Some(de_state);
            0
        }
    )
}

/// AppLayerEvent trait that will be implemented on enums that
/// derive AppLayerEvent.
pub trait AppLayerEvent {
    /// Return the enum variant of the given ID.
    fn from_id(id: i32) -> Option<Self> where Self: std::marker::Sized;

    /// Convert the enum variant to a C-style string (suffixed with \0).
    fn to_cstring(&self) -> &str;

    /// Return the enum variant for the given name.
    fn from_string(s: &str) -> Option<Self> where Self: std::marker::Sized;

    /// Return the ID value of the enum variant.
    fn as_i32(&self) -> i32;

    unsafe extern "C" fn get_event_info(
        event_name: *const std::os::raw::c_char,
        event_id: *mut std::os::raw::c_int,
        event_type: *mut core::AppLayerEventType,
    ) -> std::os::raw::c_int;

    unsafe extern "C" fn get_event_info_by_id(
        event_id: std::os::raw::c_int,
        event_name: *mut *const std::os::raw::c_char,
        event_type: *mut core::AppLayerEventType,
    ) -> i8;
}

/// Generic `get_info_info` implementation for enums implementing
/// AppLayerEvent.
///
/// Normally usage of this function will be generated by
/// derive(AppLayerEvent), for example:
///
/// #[derive(AppLayerEvent)]
/// enum AppEvent {
///     EventOne,
///     EventTwo,
/// }
///
/// get_event_info::<AppEvent>(...)
#[inline(always)]
pub unsafe fn get_event_info<T: AppLayerEvent>(
    event_name: *const std::os::raw::c_char,
    event_id: *mut std::os::raw::c_int,
    event_type: *mut core::AppLayerEventType,
) -> std::os::raw::c_int {
    if event_name == std::ptr::null() {
        return -1;
    }

    let event = match CStr::from_ptr(event_name).to_str().map(T::from_string) {
        Ok(Some(event)) => event.as_i32(),
        _ => -1,
    };
    *event_type = core::APP_LAYER_EVENT_TYPE_TRANSACTION;
    *event_id = event as std::os::raw::c_int;
    return 0;
}

/// Generic `get_info_info_by_id` implementation for enums implementing
/// AppLayerEvent.
#[inline(always)]
pub unsafe fn get_event_info_by_id<T: AppLayerEvent>(
    event_id: std::os::raw::c_int,
    event_name: *mut *const std::os::raw::c_char,
    event_type: *mut core::AppLayerEventType,
) -> i8 {
    if let Some(e) = T::from_id(event_id as i32) {
        *event_name = e.to_cstring().as_ptr() as *const std::os::raw::c_char;
        *event_type = core::APP_LAYER_EVENT_TYPE_TRANSACTION;
        return 0;
    }
    return -1;
}
