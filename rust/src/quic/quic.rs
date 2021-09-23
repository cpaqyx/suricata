/* Copyright (C) 2021 Open Information Security Foundation
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

use super::{
    cyu::Cyu,
    parser::{QuicData, QuicHeader},
};
use crate::applayer::{self, *};
use crate::core::{self, AppProto, Flow, ALPROTO_FAILED, ALPROTO_UNKNOWN, IPPROTO_UDP};
use std::ffi::CString;

static mut ALPROTO_QUIC: AppProto = ALPROTO_UNKNOWN;

const DEFAULT_DCID_LEN: usize = 16;

#[derive(Debug)]
pub struct QuicTransaction {
    tx_id: u64,
    pub header: QuicHeader,
    pub cyu: Vec<Cyu>,

    de_state: Option<*mut core::DetectEngineState>,
    events: *mut core::AppLayerDecoderEvents,
    tx_data: AppLayerTxData,
}

impl QuicTransaction {
    fn new(header: QuicHeader, data: QuicData) -> Self {
        let cyu = Cyu::generate(&header, &data.frames);
        QuicTransaction {
            tx_id: 0,
            header,
            cyu,
            de_state: None,
            events: std::ptr::null_mut(),
            tx_data: AppLayerTxData::new(),
        }
    }

    fn free(&mut self) {
        if !self.events.is_null() {
            core::sc_app_layer_decoder_events_free_events(&mut self.events);
        }
        if let Some(state) = self.de_state {
            core::sc_detect_engine_state_free(state);
        }
    }
}

impl Drop for QuicTransaction {
    fn drop(&mut self) {
        self.free();
    }
}

pub struct QuicState {
    max_tx_id: u64,
    transactions: Vec<QuicTransaction>,
}

impl Default for QuicState {
    fn default() -> Self {
        Self {
            max_tx_id: 0,
            transactions: Vec::new(),
        }
    }
}

impl QuicState {
    fn new() -> Self {
        Self::default()
    }

    // Free a transaction by ID.
    fn free_tx(&mut self, tx_id: u64) {
        let tx = self
            .transactions
            .iter()
            .position(|tx| tx.tx_id == tx_id + 1);
        if let Some(idx) = tx {
            let _ = self.transactions.remove(idx);
        }
    }

    fn get_tx(&mut self, tx_id: u64) -> Option<&QuicTransaction> {
        self.transactions.iter().find(|&tx| tx.tx_id == tx_id + 1)
    }

    fn new_tx(&mut self, header: QuicHeader, data: QuicData) -> QuicTransaction {
        let mut tx = QuicTransaction::new(header, data);
        self.max_tx_id += 1;
        tx.tx_id = self.max_tx_id;
        return tx;
    }

    fn tx_iterator(
        &mut self, min_tx_id: u64, state: &mut u64,
    ) -> Option<(&QuicTransaction, u64, bool)> {
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

    fn parse(&mut self, input: &[u8]) -> bool {
        match QuicHeader::from_bytes(input, DEFAULT_DCID_LEN) {
            Ok((rest, header)) => match QuicData::from_bytes(rest) {
                Ok(data) => {
                    let transaction = self.new_tx(header, data);
                    self.transactions.push(transaction);

                    return true;
                }
                Err(_e) => {
                    return false;
                }
            },
            Err(_e) => {
                return false;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn rs_quic_state_new(
    _orig_state: *mut std::os::raw::c_void, _orig_proto: AppProto,
) -> *mut std::os::raw::c_void {
    let state = QuicState::new();
    let boxed = Box::new(state);
    return Box::into_raw(boxed) as *mut _;
}

#[no_mangle]
pub extern "C" fn rs_quic_state_free(state: *mut std::os::raw::c_void) {
    // Just unbox...
    std::mem::drop(unsafe { Box::from_raw(state as *mut QuicState) });
}

#[no_mangle]
pub unsafe extern "C" fn rs_quic_state_tx_free(state: *mut std::os::raw::c_void, tx_id: u64) {
    let state = cast_pointer!(state, QuicState);
    state.free_tx(tx_id);
}

#[no_mangle]
pub unsafe extern "C" fn rs_quic_probing_parser(
    _flow: *const Flow, _direction: u8, input: *const u8, input_len: u32, _rdir: *mut u8,
) -> AppProto {
    let slice = build_slice!(input, input_len as usize);

    if QuicHeader::from_bytes(slice, DEFAULT_DCID_LEN).is_ok() {
        return ALPROTO_QUIC;
    } else {
        return ALPROTO_FAILED;
    }
}

#[no_mangle]
pub unsafe extern "C" fn rs_quic_parse(
    _flow: *const Flow, state: *mut std::os::raw::c_void, _pstate: *mut std::os::raw::c_void,
    stream_slice: StreamSlice,
    _data: *const std::os::raw::c_void,
) -> AppLayerResult {
    let state = cast_pointer!(state, QuicState);
    let buf = stream_slice.as_slice();

    if state.parse(buf) {
        return AppLayerResult::ok();
    }
    return AppLayerResult::err();
}

#[no_mangle]
pub unsafe extern "C" fn rs_quic_state_get_tx(
    state: *mut std::os::raw::c_void, tx_id: u64,
) -> *mut std::os::raw::c_void {
    let state = cast_pointer!(state, QuicState);
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
pub unsafe extern "C" fn rs_quic_state_get_tx_count(state: *mut std::os::raw::c_void) -> u64 {
    let state = cast_pointer!(state, QuicState);
    return state.max_tx_id;
}

#[no_mangle]
pub extern "C" fn rs_quic_state_progress_completion_status(_direction: u8) -> std::os::raw::c_int {
    // This parser uses 1 to signal transaction completion status.
    return 1;
}

#[no_mangle]
pub unsafe extern "C" fn rs_quic_tx_get_alstate_progress(
    tx: *mut std::os::raw::c_void, _direction: u8,
) -> std::os::raw::c_int {
    let _tx = cast_pointer!(tx, QuicTransaction);
    return 1;
}

#[no_mangle]
pub unsafe extern "C" fn rs_quic_state_get_events(
    tx: *mut std::os::raw::c_void,
) -> *mut core::AppLayerDecoderEvents {
    let tx = cast_pointer!(tx, QuicTransaction);
    return tx.events;
}

#[no_mangle]
pub extern "C" fn rs_quic_state_get_event_info(
    _event_name: *const std::os::raw::c_char, _event_id: *mut std::os::raw::c_int,
    _event_type: *mut core::AppLayerEventType,
) -> std::os::raw::c_int {
    return -1;
}

#[no_mangle]
pub extern "C" fn rs_quic_state_get_event_info_by_id(
    _event_id: std::os::raw::c_int, _event_name: *mut *const std::os::raw::c_char,
    _event_type: *mut core::AppLayerEventType,
) -> i8 {
    return -1;
}

#[no_mangle]
pub unsafe extern "C" fn rs_quic_state_get_tx_iterator(
    _ipproto: u8, _alproto: AppProto, state: *mut std::os::raw::c_void, min_tx_id: u64,
    _max_tx_id: u64, istate: &mut u64,
) -> applayer::AppLayerGetTxIterTuple {
    let state = cast_pointer!(state, QuicState);
    match state.tx_iterator(min_tx_id, istate) {
        Some((tx, out_tx_id, has_next)) => {
            let c_tx = tx as *const _ as *mut _;
            let ires = applayer::AppLayerGetTxIterTuple::with_values(c_tx, out_tx_id, has_next);
            return ires;
        }
        None => {
            return applayer::AppLayerGetTxIterTuple::not_found();
        }
    }
}

export_tx_data_get!(rs_quic_get_tx_data, QuicTransaction);

// Parser name as a C style string.
const PARSER_NAME: &[u8] = b"quic\0";

#[no_mangle]
pub unsafe extern "C" fn rs_quic_register_parser() {
    let default_port = CString::new("[443,80]").unwrap();
    let parser = RustParser {
        name: PARSER_NAME.as_ptr() as *const std::os::raw::c_char,
        default_port: default_port.as_ptr(),
        ipproto: IPPROTO_UDP,
        probe_ts: Some(rs_quic_probing_parser),
        probe_tc: Some(rs_quic_probing_parser),
        min_depth: 0,
        max_depth: 16,
        state_new: rs_quic_state_new,
        state_free: rs_quic_state_free,
        tx_free: rs_quic_state_tx_free,
        parse_ts: rs_quic_parse,
        parse_tc: rs_quic_parse,
        get_tx_count: rs_quic_state_get_tx_count,
        get_tx: rs_quic_state_get_tx,
        tx_comp_st_ts: 1,
        tx_comp_st_tc: 1,
        tx_get_progress: rs_quic_tx_get_alstate_progress,
        get_eventinfo: Some(rs_quic_state_get_event_info),
        get_eventinfo_byid: Some(rs_quic_state_get_event_info_by_id),
        localstorage_new: None,
        localstorage_free: None,
        get_files: None,
        get_tx_iterator: Some(rs_quic_state_get_tx_iterator),
        get_tx_data: rs_quic_get_tx_data,
        apply_tx_config: None,
        flags: 0,
        truncate: None,
        get_frame_id_by_name: None,
        get_frame_name_by_id: None,

    };

    let ip_proto_str = CString::new("udp").unwrap();

    if AppLayerProtoDetectConfProtoDetectionEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
        let alproto = AppLayerRegisterProtocolDetection(&parser, 1);
        ALPROTO_QUIC = alproto;
        if AppLayerParserConfParserEnabled(ip_proto_str.as_ptr(), parser.name) != 0 {
            let _ = AppLayerRegisterParser(&parser, alproto);
        }
        SCLogDebug!("Rust quic parser registered.");
    } else {
        SCLogDebug!("Protocol detector and parser disabled for quic.");
    }
}