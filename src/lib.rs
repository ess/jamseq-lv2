//! `jamseq-lv2`: Standalone dynamic LV2 plugin host adapter for Jamband.
//!
//! Implements and exports the binary-stable `JamSeqAdapterVTable` C-ABI contract,
//! isolating Lilv and LV2 dependencies from core `jamseq`.

pub mod instance;
pub mod scanner;

use std::ffi::{c_char, c_void, CStr};

use instance::Lv2Instance;
use jamseq_plugin_api::{
    allocate_c_plugin_list, free_c_note_name_list, free_c_parameter_list, free_c_plugin_list,
    JamSeqAdapterVTable, JamSeqAudioBuffer, JamSeqMidiEvent, JamSeqNoteNameList,
    JamSeqParameterList, JamSeqPluginList, JAMSEQ_API_VERSION,
};

static FORMAT_ID_CSTR: &[u8] = b"lv2\0";

unsafe extern "C" fn lv2_format_id() -> *const c_char {
    FORMAT_ID_CSTR.as_ptr() as *const c_char
}

unsafe extern "C" fn lv2_handles(path_or_uri: *const c_char) -> bool {
    if path_or_uri.is_null() {
        return false;
    }
    let s = unsafe { CStr::from_ptr(path_or_uri).to_string_lossy() };
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("lv2:")
        || s.ends_with(".lv2")
        || s.contains(".lv2/")
        || s.contains(".lv2\\")
}

unsafe extern "C" fn lv2_scan() -> *mut JamSeqPluginList {
    let plugins = scanner::scan_lv2_plugins();
    allocate_c_plugin_list(&plugins)
}

unsafe extern "C" fn lv2_free_plugin_list(list: *mut JamSeqPluginList) {
    unsafe { free_c_plugin_list(list) };
}

unsafe extern "C" fn lv2_create_instance(
    path_or_uri: *const c_char,
    sample_rate: f64,
    max_block_size: u32,
) -> *mut c_void {
    if path_or_uri.is_null() {
        return std::ptr::null_mut();
    }
    let uri = unsafe { CStr::from_ptr(path_or_uri).to_string_lossy().into_owned() };
    match Lv2Instance::load(&uri, sample_rate, max_block_size) {
        Ok(instance) => Box::into_raw(Box::new(instance)) as *mut c_void,
        Err(e) => {
            eprintln!("[jamseq-lv2] Error loading LV2 plugin '{}': {}", uri, e);
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn lv2_process(
    instance: *mut c_void,
    inputs: *const JamSeqAudioBuffer,
    outputs: *mut JamSeqAudioBuffer,
    midi_events: *const JamSeqMidiEvent,
    midi_count: u32,
) {
    if instance.is_null() || outputs.is_null() {
        return;
    }
    let inst = unsafe { &mut *(instance as *mut Lv2Instance) };
    let in_ref = if inputs.is_null() {
        None
    } else {
        Some(unsafe { &*inputs })
    };
    let out_ref = unsafe { &mut *outputs };

    let events_slice = if midi_count > 0 && !midi_events.is_null() {
        unsafe { std::slice::from_raw_parts(midi_events, midi_count as usize) }
    } else {
        &[]
    };

    inst.process(in_ref, out_ref, events_slice);
}

unsafe extern "C" fn lv2_get_parameters(instance: *mut c_void) -> *mut JamSeqParameterList {
    if instance.is_null() {
        return std::ptr::null_mut();
    }
    let inst = unsafe { &*(instance as *const Lv2Instance) };
    inst.get_parameters()
}

unsafe extern "C" fn lv2_free_parameters(list: *mut JamSeqParameterList) {
    unsafe { free_c_parameter_list(list) };
}

unsafe extern "C" fn lv2_set_parameter(
    instance: *mut c_void,
    param_id: *const c_char,
    value: f32,
) {
    if instance.is_null() || param_id.is_null() {
        return;
    }
    let inst = unsafe { &mut *(instance as *mut Lv2Instance) };
    let id_str = unsafe { CStr::from_ptr(param_id).to_string_lossy() };
    inst.set_parameter(&id_str, value);
}

unsafe extern "C" fn lv2_get_note_names(instance: *mut c_void) -> *mut JamSeqNoteNameList {
    if instance.is_null() {
        return std::ptr::null_mut();
    }
    let inst = unsafe { &*(instance as *const Lv2Instance) };
    inst.get_note_names()
}

unsafe extern "C" fn lv2_free_note_names(list: *mut JamSeqNoteNameList) {
    unsafe { free_c_note_name_list(list) };
}

unsafe extern "C" fn lv2_destroy_instance(instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(instance as *mut Lv2Instance);
    }
}

pub static LV2_ADAPTER_VTABLE: JamSeqAdapterVTable = JamSeqAdapterVTable {
    api_version: JAMSEQ_API_VERSION,
    format_id: lv2_format_id,
    handles: lv2_handles,
    scan: lv2_scan,
    free_plugin_list: lv2_free_plugin_list,
    create_instance: lv2_create_instance,
    process: lv2_process,
    get_parameters: lv2_get_parameters,
    free_parameters: lv2_free_parameters,
    set_parameter: lv2_set_parameter,
    get_note_names: lv2_get_note_names,
    free_note_names: lv2_free_note_names,
    destroy_instance: lv2_destroy_instance,
};

/// Exported dynamic library entrypoint symbol.
#[unsafe(no_mangle)]
pub extern "C" fn jamseq_get_host_adapter() -> *const JamSeqAdapterVTable {
    &LV2_ADAPTER_VTABLE
}
