/* Copyright (C) 2018-2020 Open Information Security Foundation
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

use crate::applayer::{self, *};
use crate::core;
use crate::core::{ALPROTO_UNKNOWN, AppProto, Flow, IPPROTO_UDP};
use crate::core::{sc_detect_engine_state_free, sc_app_layer_decoder_events_free_events};
use crate::dhcp::parser::*;
use std;
use std::ffi::CString;

static mut ALPROTO_DHCP: AppProto = ALPROTO_UNKNOWN;

static DHCP_MIN_FRAME_LEN: u32 = 232;

pub const BOOTP_REQUEST: u8 = 1;
pub const BOOTP_REPLY: u8 = 2;

// DHCP option types. Names based on IANA naming:
// https://www.iana.org/assignments/bootp-dhcp-parameters/bootp-dhcp-parameters.xhtml
pub const DHCP_OPT_SUBNET_MASK: u8 = 1;
pub const DHCP_OPT_ROUTERS: u8 = 3;
pub const DHCP_OPT_DNS_SERVER: u8 = 6;
pub const DHCP_OPT_HOSTNAME: u8 = 12;
pub const DHCP_OPT_REQUESTED_IP: u8 = 50;
pub const DHCP_OPT_ADDRESS_TIME: u8 = 51;
pub const DHCP_OPT_TYPE: u8 = 53;
pub const DHCP_OPT_SERVER_ID: u8 = 54;
pub const DHCP_OPT_PARAMETER_LIST: u8 = 55;
pub const DHCP_OPT_RENEWAL_TIME: u8 = 58;
pub const DHCP_OPT_REBINDING_TIME: u8 = 59;
pub const DHCP_OPT_CLIENT_ID: u8 = 61;
pub const DHCP_OPT_END: u8 = 255;

/// DHCP message types.
pub const DHCP_TYPE_DISCOVER: u8 = 1;
pub const DHCP_TYPE_OFFER: u8 = 2;
pub const DHCP_TYPE_REQUEST: u8 = 3;
pub const DHCP_TYPE_DECLINE: u8 = 4;
pub const DHCP_TYPE_ACK: u8 = 5;
pub const DHCP_TYPE_NAK: u8 = 6;
pub const DHCP_TYPE_RELEASE: u8 = 7;
pub const DHCP_TYPE_INFORM: u8 = 8;

/// DHCP parameter types.
/// https://www.iana.org/assignments/bootp-dhcp-parameters/bootp-dhcp-parameters.txt
pub const DHCP_PARAM_SUBNET_MASK: u8 = 1;
pub const DHCP_PARAM_ROUTER: u8 = 3;
pub const DHCP_PARAM_DNS_SERVER: u8 = 6;
pub const DHCP_PARAM_DOMAIN: u8 = 15;
pub const DHCP_PARAM_ARP_TIMEOUT: u8 = 35;
pub const DHCP_PARAM_NTP_SERVER: u8 = 42;
pub const DHCP_PARAM_TFTP_SERVER_NAME: u8 = 66;
pub const DHCP_PARAM_TFTP_SERVER_IP: u8 = 150;

#[derive(AppLayerEvent)]
pub enum DHCPEvent {
    TruncatedOptions,
    MalformedOptions,
}

/// The concept of a transaction is more to satisfy the Suricata
/// app-layer. This DHCP parser is actually stateless where each
/// message is its own transaction.
pub struct DHCPTransaction {
    tx_id: u64,
    pub message: DHCPMessage,
    de_state: Option<*mut core::DetectEngineState>,
    events: *mut core::AppLayerDecoderEvents,
    tx_data: applayer::AppLayerTxData,
}

impl DHCPTransaction {
    pub fn new(id: u64, message: DHCPMessage) -> DHCPTransaction {
        DHCPTransaction {
            tx_id: id,
            message: message,
            de_state: None,
            events: std::ptr::null_mut(),
            tx_data: applayer::AppLayerTxData::new(),
        }
    }

    pub fn free(&mut self) {
        if self.events != std::ptr::null_mut() {
            sc_app_layer_decoder_events_free_events(&mut self.events);
        }
        match self.de_state {
            Some(state) => {
                sc_detect_engine_state_free(state);
            }
            _ => {}
        }
    }

}

impl Drop for DHCPTransaction {
    fn drop(&mut self) {
        self.free();
    }
}

export_tx_get_detect_state!(rs_dhcp_tx_get_detect_state, DHCPTransaction);
export_tx_set_detect_state!(rs_dhcp_tx_set_detect_state, DHCPTransaction);

#[derive(Default)]
pub struct DHCPState {
    // Internal transaction ID.
    tx_id: u64,

    // List of transactions.
    transactions: Vec<DHCPTransaction>,

    events: u16,
}

impl DHCPState {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn parse(&mut self, input: &[u8]) -> bool {
        match dhcp_parse(input) {
            Ok((_, message)) => {
                let malformed_options = message.malformed_options;
                let truncated_options = message.truncated_options;
                self.tx_id += 1;
                let transaction = DHCPTransaction::new(self.tx_id, message);
                self.transactions.push(transaction);
                if malformed_options {
                    self.set_event(DHCPEvent::MalformedOptions);
                }
                if truncated_options {
                    self.set_event(DHCPEvent::TruncatedOptions);
                }
                return true;
            }
            _ => {
                return false;
            }
        }
    }

    pub fn get_tx(&mut self, tx_id: u64) -> Option<&DHCPTransaction> {
        for tx in &mut self.transactions {
            if tx.tx_id == tx_id + 1 {
                return Some(tx);
            }
        }
        return None;
    }

    fn free_tx(&mut self, tx_id: u64) {
        let len = self.transactions.len();
        let mut found = false;
        let mut index = 0;
        for i in 0..len {
            let tx = &self.transactions[i];
            if tx.tx_id == tx_id + 1 {
                found = true;
                index = i;
                break;
            }
        }
        if found {
            self.transactions.remove(index);
        }
    }

    fn set_event(&mut self, event: DHCPEvent) {
        if let Some(tx) = self.transactions.last_mut() {
            core::sc_app_layer_decoder_events_set_event_raw(
                &mut tx.events, event as u8);
            self.events += 1;
        }
    }

    fn get_tx_iterator(&mut self, min_tx_id: u64, state: &mut u64) ->
        Option<(&DHCPTransaction, u64, bool)>
    {
        let mut index = *state as usize;
        let len = self.transactions.len();

        while index < len {
            let tx = &self.transactions[index];
            if tx.tx_id < min_tx_id + 1 {
                index += 1;
                continue;
            }
            *state = index as u64;
            return Some((tx, tx.tx_id - 1, (len - index) > 1));
        }
        
        return None;
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_probing_parser(_flow: *const Flow,
                                         _direction: u8,
                                         input: *const u8,
                                         input_len: u32,
                                         _rdir: *mut u8) -> AppProto
{
    if input_len < DHCP_MIN_FRAME_LEN {
        return ALPROTO_UNKNOWN;
    }

    let slice = build_slice!(input, input_len as usize);
    match parse_header(slice) {
        Ok((_, _)) => {
            return ALPROTO_DHCP;
        }
        _ => {
            return ALPROTO_UNKNOWN;
        }
    }
}

#[no_mangle]
pub extern "C" fn rs_dhcp_tx_get_alstate_progress(_tx: *mut std::os::raw::c_void,
                                                  _direction: u8) -> std::os::raw::c_int {
    // As this is a stateless parser, simply use 1.
    return 1;
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_state_get_tx(state: *mut std::os::raw::c_void,
                                       tx_id: u64) -> *mut std::os::raw::c_void {
    let state = cast_pointer!(state, DHCPState);
    match state.get_tx(tx_id) {
        Some(tx) => {
            return tx as *const _ as *mut _;
        }
        None => {
            return std::ptr::null_mut();
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_state_get_tx_count(state: *mut std::os::raw::c_void) -> u64 {
    let state = cast_pointer!(state, DHCPState);
    return state.tx_id;
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_parse(_flow: *const core::Flow,
                                state: *mut std::os::raw::c_void,
                                _pstate: *mut std::os::raw::c_void,
                                input: *const u8,
                                input_len: u32,
                                _data: *const std::os::raw::c_void,
                                _flags: u8) -> AppLayerResult {
    let state = cast_pointer!(state, DHCPState);
    let buf = build_slice!(input, input_len as usize);
    if state.parse(buf) {
        return AppLayerResult::ok();
    }
    return AppLayerResult::err();
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_state_tx_free(
    state: *mut std::os::raw::c_void,
    tx_id: u64)
{
    let state = cast_pointer!(state, DHCPState);
    state.free_tx(tx_id);
}

#[no_mangle]
pub extern "C" fn rs_dhcp_state_new(_orig_state: *mut std::os::raw::c_void, _orig_proto: AppProto) -> *mut std::os::raw::c_void {
    let state = DHCPState::new();
    let boxed = Box::new(state);
    return Box::into_raw(boxed) as *mut _;
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_state_free(state: *mut std::os::raw::c_void) {
    std::mem::drop(Box::from_raw(state as *mut DHCPState));
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_state_get_events(tx: *mut std::os::raw::c_void)
                                           -> *mut core::AppLayerDecoderEvents
{
    let tx = cast_pointer!(tx, DHCPTransaction);
    return tx.events;
}

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_state_get_tx_iterator(
    _ipproto: u8,
    _alproto: AppProto,
    state: *mut std::os::raw::c_void,
    min_tx_id: u64,
    _max_tx_id: u64,
    istate: &mut u64)
    -> applayer::AppLayerGetTxIterTuple
{
    let state = cast_pointer!(state, DHCPState);
    match state.get_tx_iterator(min_tx_id, istate) {
        Some((tx, out_tx_id, has_next)) => {
            let c_tx = tx as *const _ as *mut _;
            let ires = applayer::AppLayerGetTxIterTuple::with_values(
                c_tx, out_tx_id, has_next);
            return ires;
        }
        None => {
            return applayer::AppLayerGetTxIterTuple::not_found();
        }
    }
}

export_tx_data_get!(rs_dhcp_get_tx_data, DHCPTransaction);

const PARSER_NAME: &'static [u8] = b"dhcp\0";

#[no_mangle]
pub unsafe extern "C" fn rs_dhcp_register_parser() {
    SCLogDebug!("Registering DHCP parser.");
    let ports = CString::new("[67,68]").unwrap();
    let parser = RustParser {
        name: PARSER_NAME.as_ptr() as *const std::os::raw::c_char,
        default_port       : ports.as_ptr(),
        ipproto            : IPPROTO_UDP,
        probe_ts           : Some(rs_dhcp_probing_parser),
        probe_tc           : Some(rs_dhcp_probing_parser),
        min_depth          : 0,
        max_depth          : 16,
        state_new          : rs_dhcp_state_new,
        state_free         : rs_dhcp_state_free,
        tx_free            : rs_dhcp_state_tx_free,
        parse_ts           : rs_dhcp_parse,
        parse_tc           : rs_dhcp_parse,
        get_tx_count       : rs_dhcp_state_get_tx_count,
        get_tx             : rs_dhcp_state_get_tx,
        tx_comp_st_ts      : 1,
        tx_comp_st_tc      : 1,
        tx_get_progress    : rs_dhcp_tx_get_alstate_progress,
        get_de_state       : rs_dhcp_tx_get_detect_state,
        set_de_state       : rs_dhcp_tx_set_detect_state,
        get_events         : Some(rs_dhcp_state_get_events),
        get_eventinfo      : Some(DHCPEvent::get_event_info),
        get_eventinfo_byid : Some(DHCPEvent::get_event_info_by_id),
        localstorage_new   : None,
        localstorage_free  : None,
        get_files          : None,
        get_tx_iterator    : Some(rs_dhcp_state_get_tx_iterator),
        get_tx_data        : rs_dhcp_get_tx_data,
        apply_tx_config    : None,
        flags              : APP_LAYER_PARSER_OPT_UNIDIR_TXS,
        truncate           : None,
    };

    let ip_proto_str = CString::new("udp").unwrap();

    if AppLayerProtoDetectConfProtoDetectionEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
        let alproto = AppLayerRegisterProtocolDetection(&parser, 1);
        ALPROTO_DHCP = alproto;
        if AppLayerParserConfParserEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
            let _ = AppLayerRegisterParser(&parser, alproto);
        }
    } else {
        SCLogDebug!("Protocol detector and parser disabled for DHCP.");
    }
}
