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

use std;
use std::ffi::CString;
use nom;
use nom::IResult;
use nom::number::streaming::be_u32;
use der_parser::der::der_read_element_header;
use der_parser::ber::BerClass;
use kerberos_parser::krb5_parser;
use kerberos_parser::krb5::{EncryptionType,ErrorCode,MessageType,PrincipalName,Realm};
use crate::applayer::{self, *};
use crate::core;
use crate::core::{AppProto,Flow,ALPROTO_FAILED,ALPROTO_UNKNOWN,STREAM_TOCLIENT,STREAM_TOSERVER,sc_detect_engine_state_free};

#[derive(AppLayerEvent)]
pub enum KRB5Event {
    MalformedData,
    WeakEncryption,
}

pub struct KRB5State {
    pub req_id: u8,

    pub record_ts: usize,
    pub defrag_buf_ts: Vec<u8>,
    pub record_tc: usize,
    pub defrag_buf_tc: Vec<u8>,

    /// List of transactions for this session
    transactions: Vec<KRB5Transaction>,

    /// tx counter for assigning incrementing id's to tx's
    tx_id: u64,
}

pub struct KRB5Transaction {
    /// The message type: AS-REQ, AS-REP, etc.
    pub msg_type: MessageType,

    /// The client PrincipalName, if present
    pub cname: Option<PrincipalName>,
    /// The server Realm, if present
    pub realm: Option<Realm>,
    /// The server PrincipalName, if present
    pub sname: Option<PrincipalName>,

    /// Encryption used (only in AS-REP and TGS-REP)
    pub etype: Option<EncryptionType>,

    /// Error code, if request has failed
    pub error_code: Option<ErrorCode>,

    /// The internal transaction id
    id: u64,

    /// The detection engine state, if present
    de_state: Option<*mut core::DetectEngineState>,

    /// The events associated with this transaction
    events: *mut core::AppLayerDecoderEvents,

    tx_data: applayer::AppLayerTxData,
}

pub fn to_hex_string(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        s.push_str(&format!("{:02X}", b));
    }
    s
}

impl KRB5State {
    pub fn new() -> KRB5State {
        KRB5State{
            req_id: 0,
            record_ts: 0,
            defrag_buf_ts: Vec::new(),
            record_tc: 0,
            defrag_buf_tc: Vec::new(),
            transactions: Vec::new(),
            tx_id: 0,
        }
    }

    /// Parse a Kerberos request message
    ///
    /// Returns 0 in case of success, or -1 on error
    fn parse(&mut self, i: &[u8], _direction: u8) -> i32 {
        match der_read_element_header(i) {
            Ok((_rem,hdr)) => {
                // Kerberos messages start with an APPLICATION header
                if hdr.class != BerClass::Application { return 0; }
                match hdr.tag.0 {
                    10 => {
                        self.req_id = 10;
                    },
                    11 => {
                        let res = krb5_parser::parse_as_rep(i);
                        if let Ok((_,kdc_rep)) = res {
                            let mut tx = self.new_tx();
                            tx.msg_type = MessageType::KRB_AS_REP;
                            tx.cname = Some(kdc_rep.cname);
                            tx.realm = Some(kdc_rep.crealm);
                            tx.sname = Some(kdc_rep.ticket.sname);
                            tx.etype = Some(kdc_rep.enc_part.etype);
                            self.transactions.push(tx);
                            if test_weak_encryption(kdc_rep.enc_part.etype) {
                                self.set_event(KRB5Event::WeakEncryption);
                            }
                        };
                        self.req_id = 0;
                    },
                    12 => {
                        self.req_id = 12;
                    },
                    13 => {
                        let res = krb5_parser::parse_tgs_rep(i);
                        if let Ok((_,kdc_rep)) = res {
                            let mut tx = self.new_tx();
                            tx.msg_type = MessageType::KRB_TGS_REP;
                            tx.cname = Some(kdc_rep.cname);
                            tx.realm = Some(kdc_rep.crealm);
                            tx.sname = Some(kdc_rep.ticket.sname);
                            tx.etype = Some(kdc_rep.enc_part.etype);
                            self.transactions.push(tx);
                            if test_weak_encryption(kdc_rep.enc_part.etype) {
                                self.set_event(KRB5Event::WeakEncryption);
                            }
                        };
                        self.req_id = 0;
                    },
                    14 => {
                        self.req_id = 14;
                    },
                    15 => {
                        self.req_id = 0;
                    },
                    30 => {
                        let res = krb5_parser::parse_krb_error(i);
                        if let Ok((_,error)) = res {
                            let mut tx = self.new_tx();
                            tx.msg_type = MessageType(self.req_id as u32);
                            tx.cname = error.cname;
                            tx.realm = error.crealm;
                            tx.sname = Some(error.sname);
                            tx.error_code = Some(error.error_code);
                            self.transactions.push(tx);
                        };
                        self.req_id = 0;
                    },
                    _ => { SCLogDebug!("unknown/unsupported tag {}", hdr.tag); },
                }
                0
            },
            Err(nom::Err::Incomplete(_)) => {
                SCLogDebug!("Insufficient data while parsing KRB5 data");
                self.set_event(KRB5Event::MalformedData);
                -1
            },
            Err(_) => {
                SCLogDebug!("Error while parsing KRB5 data");
                self.set_event(KRB5Event::MalformedData);
                -1
            },
        }
    }

    pub fn free(&mut self) {
        // All transactions are freed when the `transactions` object is freed.
        // But let's be explicit
        self.transactions.clear();
    }

    fn new_tx(&mut self) -> KRB5Transaction {
        self.tx_id += 1;
        KRB5Transaction::new(self.tx_id)
    }

    fn get_tx_by_id(&mut self, tx_id: u64) -> Option<&KRB5Transaction> {
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
    fn set_event(&mut self, event: KRB5Event) {
        if let Some(tx) = self.transactions.last_mut() {
            let ev = event as u8;
            core::sc_app_layer_decoder_events_set_event_raw(&mut tx.events, ev);
        }
    }
}

impl KRB5Transaction {
    pub fn new(id: u64) -> KRB5Transaction {
        KRB5Transaction{
            msg_type: MessageType(0),
            cname: None,
            realm: None,
            sname: None,
            etype: None,
            error_code: None,
            id: id,
            de_state: None,
            events: std::ptr::null_mut(),
            tx_data: applayer::AppLayerTxData::new(),
        }
    }
}

impl Drop for KRB5Transaction {
    fn drop(&mut self) {
        if self.events != std::ptr::null_mut() {
            core::sc_app_layer_decoder_events_free_events(&mut self.events);
        }
        if let Some(state) = self.de_state {
            sc_detect_engine_state_free(state);
        }
    }
}

/// Return true if Kerberos `EncryptionType` is weak
pub fn test_weak_encryption(alg:EncryptionType) -> bool {
    match alg {
        EncryptionType::AES128_CTS_HMAC_SHA1_96 |
        EncryptionType::AES256_CTS_HMAC_SHA1_96 |
        EncryptionType::AES128_CTS_HMAC_SHA256_128 |
        EncryptionType::AES256_CTS_HMAC_SHA384_192 |
        EncryptionType::CAMELLIA128_CTS_CMAC |
        EncryptionType::CAMELLIA256_CTS_CMAC => false,
        _ => true, // all other ciphers are weak or deprecated
    }
}





/// Returns *mut KRB5State
#[no_mangle]
pub extern "C" fn rs_krb5_state_new(_orig_state: *mut std::os::raw::c_void, _orig_proto: AppProto) -> *mut std::os::raw::c_void {
    let state = KRB5State::new();
    let boxed = Box::new(state);
    return Box::into_raw(boxed) as *mut _;
}

/// Params:
/// - state: *mut KRB5State as void pointer
#[no_mangle]
pub extern "C" fn rs_krb5_state_free(state: *mut std::os::raw::c_void) {
    let mut state: Box<KRB5State> = unsafe{Box::from_raw(state as _)};
    state.free();
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_state_get_tx(state: *mut std::os::raw::c_void,
                                      tx_id: u64)
                                      -> *mut std::os::raw::c_void
{
    let state = cast_pointer!(state,KRB5State);
    match state.get_tx_by_id(tx_id) {
        Some(tx) => tx as *const _ as *mut _,
        None     => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_state_get_tx_count(state: *mut std::os::raw::c_void)
                                            -> u64
{
    let state = cast_pointer!(state,KRB5State);
    state.tx_id
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_state_tx_free(state: *mut std::os::raw::c_void,
                                       tx_id: u64)
{
    let state = cast_pointer!(state,KRB5State);
    state.free_tx(tx_id);
}

#[no_mangle]
pub extern "C" fn rs_krb5_tx_get_alstate_progress(_tx: *mut std::os::raw::c_void,
                                                 _direction: u8)
                                                 -> std::os::raw::c_int
{
    1
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_state_set_tx_detect_state(
    tx: *mut std::os::raw::c_void,
    de_state: &mut core::DetectEngineState) -> std::os::raw::c_int
{
    let tx = cast_pointer!(tx,KRB5Transaction);
    tx.de_state = Some(de_state);
    0
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_state_get_tx_detect_state(
    tx: *mut std::os::raw::c_void)
    -> *mut core::DetectEngineState
{
    let tx = cast_pointer!(tx,KRB5Transaction);
    match tx.de_state {
        Some(ds) => ds,
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_state_get_events(tx: *mut std::os::raw::c_void)
                                          -> *mut core::AppLayerDecoderEvents
{
    let tx = cast_pointer!(tx, KRB5Transaction);
    return tx.events;
}

static mut ALPROTO_KRB5 : AppProto = ALPROTO_UNKNOWN;

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_probing_parser(_flow: *const Flow,
        _direction: u8,
        input:*const u8, input_len: u32,
        _rdir: *mut u8) -> AppProto
{
    let slice = build_slice!(input,input_len as usize);
    let alproto = ALPROTO_KRB5;
    if slice.len() <= 10 { return ALPROTO_FAILED; }
    match der_read_element_header(slice) {
        Ok((rem, ref hdr)) => {
            // Kerberos messages start with an APPLICATION header
            if hdr.class != BerClass::Application { return ALPROTO_FAILED; }
            // Tag number should be <= 30
            if hdr.tag.0 > 30 { return ALPROTO_FAILED; }
            // Kerberos messages contain sequences
            if rem.is_empty() || rem[0] != 0x30 { return ALPROTO_FAILED; }
            // Check kerberos version
            if let Ok((rem,_hdr)) = der_read_element_header(rem) {
                if rem.len() > 5 {
                    match (rem[2],rem[3],rem[4]) {
                        // Encoding of DER integer 5 (version)
                        (2,1,5) => { return alproto; },
                        _       => (),
                    }
                }
            }
            return ALPROTO_FAILED;
        },
        Err(nom::Err::Incomplete(_)) => {
            return ALPROTO_UNKNOWN;
        },
        Err(_) => {
            return ALPROTO_FAILED;
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_probing_parser_tcp(_flow: *const Flow,
        direction: u8,
        input:*const u8, input_len: u32,
        rdir: *mut u8) -> AppProto
{
    let slice = build_slice!(input,input_len as usize);
    if slice.len() <= 14 { return ALPROTO_FAILED; }
    match be_u32(slice) as IResult<&[u8],u32> {
        Ok((rem, record_mark)) => {
            // protocol implementations forbid very large requests
            if record_mark > 16384 { return ALPROTO_FAILED; }
            return rs_krb5_probing_parser(_flow, direction,
                    rem.as_ptr(), rem.len() as u32, rdir);
        },
        Err(nom::Err::Incomplete(_)) => {
            return ALPROTO_UNKNOWN;
        },
        Err(_) => {
            return ALPROTO_FAILED;
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_parse_request(_flow: *const core::Flow,
                                       state: *mut std::os::raw::c_void,
                                       _pstate: *mut std::os::raw::c_void,
                                       input: *const u8,
                                       input_len: u32,
                                       _data: *const std::os::raw::c_void,
                                       _flags: u8) -> AppLayerResult {
    let buf = build_slice!(input,input_len as usize);
    let state = cast_pointer!(state,KRB5State);
    if state.parse(buf, STREAM_TOSERVER) < 0 {
        return AppLayerResult::err();
    }
    AppLayerResult::ok()
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_parse_response(_flow: *const core::Flow,
                                       state: *mut std::os::raw::c_void,
                                       _pstate: *mut std::os::raw::c_void,
                                       input: *const u8,
                                       input_len: u32,
                                       _data: *const std::os::raw::c_void,
                                       _flags: u8) -> AppLayerResult {
    let buf = build_slice!(input,input_len as usize);
    let state = cast_pointer!(state,KRB5State);
    if state.parse(buf, STREAM_TOCLIENT) < 0 {
        return AppLayerResult::err();
    }
    AppLayerResult::ok()
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_parse_request_tcp(_flow: *const core::Flow,
                                       state: *mut std::os::raw::c_void,
                                       _pstate: *mut std::os::raw::c_void,
                                       input: *const u8,
                                       input_len: u32,
                                       _data: *const std::os::raw::c_void,
                                       _flags: u8) -> AppLayerResult {
    let buf = build_slice!(input,input_len as usize);
    let state = cast_pointer!(state,KRB5State);

    let mut v : Vec<u8>;
    let tcp_buffer = match state.record_ts {
        0 => buf,
        _ => {
            // sanity check to avoid memory exhaustion
            if state.defrag_buf_ts.len() + buf.len() > 100000 {
                SCLogDebug!("rs_krb5_parse_request_tcp: TCP buffer exploded {} {}",
                            state.defrag_buf_ts.len(), buf.len());
                return AppLayerResult::err();
            }
            v = state.defrag_buf_ts.split_off(0);
            v.extend_from_slice(buf);
            v.as_slice()
        }
    };
    let mut cur_i = tcp_buffer;
    while cur_i.len() > 0 {
        if state.record_ts == 0 {
            match be_u32(cur_i) as IResult<&[u8],u32> {
                Ok((rem,record)) => {
                    state.record_ts = record as usize;
                    cur_i = rem;
                },
                Err(nom::Err::Incomplete(_)) => {
                    state.defrag_buf_ts.extend_from_slice(cur_i);
                    return AppLayerResult::ok();
                }
                _ => {
                    SCLogDebug!("rs_krb5_parse_request_tcp: reading record mark failed!");
                    return AppLayerResult::err();
                }
            }
        }
        if cur_i.len() >= state.record_ts {
            if state.parse(cur_i, STREAM_TOSERVER) < 0 {
                return AppLayerResult::err();
            }
            state.record_ts = 0;
            cur_i = &cur_i[state.record_ts..];
        } else {
            // more fragments required
            state.defrag_buf_ts.extend_from_slice(cur_i);
            return AppLayerResult::ok();
        }
    }
    AppLayerResult::ok()
}

#[no_mangle]
pub unsafe extern "C" fn rs_krb5_parse_response_tcp(_flow: *const core::Flow,
                                       state: *mut std::os::raw::c_void,
                                       _pstate: *mut std::os::raw::c_void,
                                       input: *const u8,
                                       input_len: u32,
                                       _data: *const std::os::raw::c_void,
                                       _flags: u8) -> AppLayerResult {
    let buf = build_slice!(input,input_len as usize);
    let state = cast_pointer!(state,KRB5State);

    let mut v : Vec<u8>;
    let tcp_buffer = match state.record_tc {
        0 => buf,
        _ => {
            // sanity check to avoid memory exhaustion
            if state.defrag_buf_tc.len() + buf.len() > 100000 {
                SCLogDebug!("rs_krb5_parse_response_tcp: TCP buffer exploded {} {}",
                            state.defrag_buf_tc.len(), buf.len());
                return AppLayerResult::err();
            }
            v = state.defrag_buf_tc.split_off(0);
            v.extend_from_slice(buf);
            v.as_slice()
        }
    };
    let mut cur_i = tcp_buffer;
    while cur_i.len() > 0 {
        if state.record_tc == 0 {
            match be_u32(cur_i) as IResult<&[u8],_> {
                Ok((rem,record)) => {
                    state.record_tc = record as usize;
                    cur_i = rem;
                },
                Err(nom::Err::Incomplete(_)) => {
                    state.defrag_buf_tc.extend_from_slice(cur_i);
                    return AppLayerResult::ok();
                }
                _ => {
                    SCLogDebug!("reading record mark failed!");
                    return AppLayerResult::ok();
                }
            }
        }
        if cur_i.len() >= state.record_tc {
            if state.parse(cur_i, STREAM_TOCLIENT) < 0 {
                return AppLayerResult::err();
            }
            state.record_tc = 0;
            cur_i = &cur_i[state.record_tc..];
        } else {
            // more fragments required
            state.defrag_buf_tc.extend_from_slice(cur_i);
            return AppLayerResult::ok();
        }
    }
    AppLayerResult::ok()
}

export_tx_data_get!(rs_krb5_get_tx_data, KRB5Transaction);

const PARSER_NAME : &'static [u8] = b"krb5\0";

#[no_mangle]
pub unsafe extern "C" fn rs_register_krb5_parser() {
    let default_port = CString::new("88").unwrap();
    let mut parser = RustParser {
        name               : PARSER_NAME.as_ptr() as *const std::os::raw::c_char,
        default_port       : default_port.as_ptr(),
        ipproto            : core::IPPROTO_UDP,
        probe_ts           : Some(rs_krb5_probing_parser),
        probe_tc           : Some(rs_krb5_probing_parser),
        min_depth          : 0,
        max_depth          : 16,
        state_new          : rs_krb5_state_new,
        state_free         : rs_krb5_state_free,
        tx_free            : rs_krb5_state_tx_free,
        parse_ts           : rs_krb5_parse_request,
        parse_tc           : rs_krb5_parse_response,
        get_tx_count       : rs_krb5_state_get_tx_count,
        get_tx             : rs_krb5_state_get_tx,
        tx_comp_st_ts      : 1,
        tx_comp_st_tc      : 1,
        tx_get_progress    : rs_krb5_tx_get_alstate_progress,
        get_de_state       : rs_krb5_state_get_tx_detect_state,
        set_de_state       : rs_krb5_state_set_tx_detect_state,
        get_events         : Some(rs_krb5_state_get_events),
        get_eventinfo      : Some(KRB5Event::get_event_info),
        get_eventinfo_byid : Some(KRB5Event::get_event_info_by_id),
        localstorage_new   : None,
        localstorage_free  : None,
        get_files          : None,
        get_tx_iterator    : None,
        get_tx_data        : rs_krb5_get_tx_data,
        apply_tx_config    : None,
        flags              : APP_LAYER_PARSER_OPT_UNIDIR_TXS,
        truncate           : None,
    };
    // register UDP parser
    let ip_proto_str = CString::new("udp").unwrap();
    if AppLayerProtoDetectConfProtoDetectionEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
        let alproto = AppLayerRegisterProtocolDetection(&parser, 1);
        // store the allocated ID for the probe function
        ALPROTO_KRB5 = alproto;
        if AppLayerParserConfParserEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
            let _ = AppLayerRegisterParser(&parser, alproto);
        }
    } else {
        SCLogDebug!("Protocol detector and parser disabled for KRB5/UDP.");
    }
    // register TCP parser
    parser.ipproto = core::IPPROTO_TCP;
    parser.probe_ts = Some(rs_krb5_probing_parser_tcp);
    parser.probe_tc = Some(rs_krb5_probing_parser_tcp);
    parser.parse_ts = rs_krb5_parse_request_tcp;
    parser.parse_tc = rs_krb5_parse_response_tcp;
    let ip_proto_str = CString::new("tcp").unwrap();
    if AppLayerProtoDetectConfProtoDetectionEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
        let alproto = AppLayerRegisterProtocolDetection(&parser, 1);
        // store the allocated ID for the probe function
        ALPROTO_KRB5 = alproto;
        if AppLayerParserConfParserEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
            let _ = AppLayerRegisterParser(&parser, alproto);
        }
    } else {
        SCLogDebug!("Protocol detector and parser disabled for KRB5/TCP.");
    }
}
