/* Copyright (C) 2017-2020 Open Information Security Foundation
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

// written by Pierre Chifflier  <chifflier@wzdftpd.net>

extern crate ntp_parser;
use self::ntp_parser::*;
use crate::core;
use crate::core::{AppProto,Flow,ALPROTO_UNKNOWN,ALPROTO_FAILED};
use crate::applayer::{self, *};
use std;
use std::ffi::CString;

use nom;

#[derive(AppLayerEvent)]
pub enum NTPEvent {
    UnsolicitedResponse ,
    MalformedData,
    NotRequest,
    NotResponse,
}

pub struct NTPState {
    /// List of transactions for this session
    transactions: Vec<NTPTransaction>,

    /// Events counter
    events: u16,

    /// tx counter for assigning incrementing id's to tx's
    tx_id: u64,
}

#[derive(Debug)]
pub struct NTPTransaction {
    /// The NTP reference ID
    pub xid: u32,

    /// The internal transaction id
    id: u64,

    /// The detection engine state, if present
    de_state: Option<*mut core::DetectEngineState>,

    /// The events associated with this transaction
    events: *mut core::AppLayerDecoderEvents,

    tx_data: applayer::AppLayerTxData,
}



impl NTPState {
    pub fn new() -> NTPState {
        NTPState{
            transactions: Vec::new(),
            events: 0,
            tx_id: 0,
        }
    }
}

impl NTPState {
    /// Parse an NTP request message
    ///
    /// Returns 0 if successful, or -1 on error
    fn parse(&mut self, i: &[u8], _direction: u8) -> i32 {
        match parse_ntp(i) {
            Ok((_,ref msg)) => {
                // SCLogDebug!("parse_ntp: {:?}",msg);
                if msg.mode == NtpMode::SymmetricActive || msg.mode == NtpMode::Client {
                    let mut tx = self.new_tx();
                    // use the reference id as identifier
                    tx.xid = msg.ref_id;
                    self.transactions.push(tx);
                }
                0
            },
            Err(nom::Err::Incomplete(_)) => {
                SCLogDebug!("Insufficient data while parsing NTP data");
                self.set_event(NTPEvent::MalformedData);
                -1
            },
            Err(_) => {
                SCLogDebug!("Error while parsing NTP data");
                self.set_event(NTPEvent::MalformedData);
                -1
            },
        }
    }

    fn free(&mut self) {
        // All transactions are freed when the `transactions` object is freed.
        // But let's be explicit
        self.transactions.clear();
    }

    fn new_tx(&mut self) -> NTPTransaction {
        self.tx_id += 1;
        NTPTransaction::new(self.tx_id)
    }

    pub fn get_tx_by_id(&mut self, tx_id: u64) -> Option<&NTPTransaction> {
        self.transactions.iter().find(|&tx| tx.id == tx_id + 1)
    }

    fn free_tx(&mut self, tx_id: u64) {
        let tx = self.transactions.iter().position(|tx| tx.id == tx_id + 1);
        debug_assert!(tx != None);
        if let Some(idx) = tx {
            let _ = self.transactions.remove(idx);
        }
    }

    /// Set an event. The event is set on the most recent transaction.
    pub fn set_event(&mut self, event: NTPEvent) {
        if let Some(tx) = self.transactions.last_mut() {
            let ev = event as u8;
            core::sc_app_layer_decoder_events_set_event_raw(&mut tx.events, ev);
            self.events += 1;
        }
    }
}

impl NTPTransaction {
    pub fn new(id: u64) -> NTPTransaction {
        NTPTransaction {
            xid: 0,
            id: id,
            de_state: None,
            events: std::ptr::null_mut(),
            tx_data: applayer::AppLayerTxData::new(),
        }
    }

    fn free(&mut self) {
        if self.events != std::ptr::null_mut() {
            core::sc_app_layer_decoder_events_free_events(&mut self.events);
        }
    }
}

impl Drop for NTPTransaction {
    fn drop(&mut self) {
        self.free();
    }
}

/// Returns *mut NTPState
#[no_mangle]
pub extern "C" fn rs_ntp_state_new(_orig_state: *mut std::os::raw::c_void, _orig_proto: AppProto) -> *mut std::os::raw::c_void {
    let state = NTPState::new();
    let boxed = Box::new(state);
    return Box::into_raw(boxed) as *mut _;
}

/// Params:
/// - state: *mut NTPState as void pointer
#[no_mangle]
pub extern "C" fn rs_ntp_state_free(state: *mut std::os::raw::c_void) {
    let mut ntp_state = unsafe{ Box::from_raw(state as *mut NTPState) };
    ntp_state.free();
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_parse_request(_flow: *const core::Flow,
                                       state: *mut std::os::raw::c_void,
                                       _pstate: *mut std::os::raw::c_void,
                                       input: *const u8,
                                       input_len: u32,
                                       _data: *const std::os::raw::c_void,
                                       _flags: u8) -> AppLayerResult {
    let buf = build_slice!(input,input_len as usize);
    let state = cast_pointer!(state,NTPState);
    if state.parse(buf, 0) < 0 {
        return AppLayerResult::err();
    }
    AppLayerResult::ok()
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_parse_response(_flow: *const core::Flow,
                                       state: *mut std::os::raw::c_void,
                                       _pstate: *mut std::os::raw::c_void,
                                       input: *const u8,
                                       input_len: u32,
                                       _data: *const std::os::raw::c_void,
                                       _flags: u8) -> AppLayerResult {
    let buf = build_slice!(input,input_len as usize);
    let state = cast_pointer!(state,NTPState);
    if state.parse(buf, 1) < 0 {
        return AppLayerResult::err();
    }
    AppLayerResult::ok()
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_state_get_tx(state: *mut std::os::raw::c_void,
                                      tx_id: u64)
                                      -> *mut std::os::raw::c_void
{
    let state = cast_pointer!(state,NTPState);
    match state.get_tx_by_id(tx_id) {
        Some(tx) => tx as *const _ as *mut _,
        None     => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_state_get_tx_count(state: *mut std::os::raw::c_void)
                                            -> u64
{
    let state = cast_pointer!(state,NTPState);
    state.tx_id
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_state_tx_free(state: *mut std::os::raw::c_void,
                                       tx_id: u64)
{
    let state = cast_pointer!(state,NTPState);
    state.free_tx(tx_id);
}

#[no_mangle]
pub extern "C" fn rs_ntp_tx_get_alstate_progress(_tx: *mut std::os::raw::c_void,
                                                 _direction: u8)
                                                 -> std::os::raw::c_int
{
    1
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_state_set_tx_detect_state(
    tx: *mut std::os::raw::c_void,
    de_state: &mut core::DetectEngineState) -> std::os::raw::c_int
{
    let tx = cast_pointer!(tx,NTPTransaction);
    tx.de_state = Some(de_state);
    0
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_state_get_tx_detect_state(
    tx: *mut std::os::raw::c_void)
    -> *mut core::DetectEngineState
{
    let tx = cast_pointer!(tx,NTPTransaction);
    match tx.de_state {
        Some(ds) => ds,
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_ntp_state_get_events(tx: *mut std::os::raw::c_void)
                                          -> *mut core::AppLayerDecoderEvents
{
    let tx = cast_pointer!(tx, NTPTransaction);
    return tx.events;
}

static mut ALPROTO_NTP : AppProto = ALPROTO_UNKNOWN;

#[no_mangle]
pub extern "C" fn ntp_probing_parser(_flow: *const Flow,
        _direction: u8,
        input:*const u8, input_len: u32,
        _rdir: *mut u8) -> AppProto
{
    let slice: &[u8] = unsafe { std::slice::from_raw_parts(input as *mut u8, input_len as usize) };
    let alproto = unsafe{ ALPROTO_NTP };
    match parse_ntp(slice) {
        Ok((_, ref msg)) => {
            if msg.version == 3 || msg.version == 4 {
                return alproto;
            } else {
                return unsafe{ALPROTO_FAILED};
            }
        },
        Err(nom::Err::Incomplete(_)) => {
            return ALPROTO_UNKNOWN;
        },
        Err(_) => {
            return unsafe{ALPROTO_FAILED};
        },
    }
}

export_tx_data_get!(rs_ntp_get_tx_data, NTPTransaction);

const PARSER_NAME : &'static [u8] = b"ntp\0";

#[no_mangle]
pub unsafe extern "C" fn rs_register_ntp_parser() {
    let default_port = CString::new("123").unwrap();
    let parser = RustParser {
        name               : PARSER_NAME.as_ptr() as *const std::os::raw::c_char,
        default_port       : default_port.as_ptr(),
        ipproto            : core::IPPROTO_UDP,
        probe_ts           : Some(ntp_probing_parser),
        probe_tc           : Some(ntp_probing_parser),
        min_depth          : 0,
        max_depth          : 16,
        state_new          : rs_ntp_state_new,
        state_free         : rs_ntp_state_free,
        tx_free            : rs_ntp_state_tx_free,
        parse_ts           : rs_ntp_parse_request,
        parse_tc           : rs_ntp_parse_response,
        get_tx_count       : rs_ntp_state_get_tx_count,
        get_tx             : rs_ntp_state_get_tx,
        tx_comp_st_ts      : 1,
        tx_comp_st_tc      : 1,
        tx_get_progress    : rs_ntp_tx_get_alstate_progress,
        get_de_state       : rs_ntp_state_get_tx_detect_state,
        set_de_state       : rs_ntp_state_set_tx_detect_state,
        get_events         : Some(rs_ntp_state_get_events),
        get_eventinfo      : Some(NTPEvent::get_event_info),
        get_eventinfo_byid : Some(NTPEvent::get_event_info_by_id),
        localstorage_new   : None,
        localstorage_free  : None,
        get_files          : None,
        get_tx_iterator    : None,
        get_tx_data        : rs_ntp_get_tx_data,
        apply_tx_config    : None,
        flags              : APP_LAYER_PARSER_OPT_UNIDIR_TXS,
        truncate           : None,
    };

    let ip_proto_str = CString::new("udp").unwrap();
    if AppLayerProtoDetectConfProtoDetectionEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
        let alproto = AppLayerRegisterProtocolDetection(&parser, 1);
        // store the allocated ID for the probe function
        ALPROTO_NTP = alproto;
        if AppLayerParserConfParserEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
            let _ = AppLayerRegisterParser(&parser, alproto);
        }
    } else {
        SCLogDebug!("Protocol detector and parser disabled for NTP.");
    }
}


#[cfg(test)]
mod tests {
    use super::NTPState;

    #[test]
    fn test_ntp_parse_request_valid() {
        // A UDP NTP v4 request, in client mode
        const REQ : &[u8] = &[
            0x23, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x18, 0x57, 0xab, 0xc3, 0x4a, 0x5f, 0x2c, 0xfe
        ];

        let mut state = NTPState::new();
        assert_eq!(0, state.parse(REQ, 0));
    }
}
