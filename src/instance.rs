use anyhow::Result;
use jamseq_plugin_api::{
    allocate_c_note_name_list, allocate_c_parameter_list, JamSeqAudioBuffer, JamSeqMidiEvent,
    JamSeqNoteName, JamSeqNoteNameList, JamSeqParamType, JamSeqParameterList,
};
use lilv::World;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

const GM_DRUM_MAP: &[(u8, &str)] = &[
    (35, "Acoustic Bass Drum"),
    (36, "Bass Drum 1"),
    (37, "Side Stick"),
    (38, "Acoustic Snare"),
    (39, "Hand Clap"),
    (40, "Electric Snare"),
    (41, "Low Floor Tom"),
    (42, "Closed Hi-Hat"),
    (43, "High Floor Tom"),
    (44, "Pedal Hi-Hat"),
    (45, "Low Tom"),
    (46, "Open Hi-Hat"),
    (47, "Low-Mid Tom"),
    (48, "Hi-Mid Tom"),
    (49, "Crash Cymbal 1"),
    (50, "High Tom"),
    (51, "Ride Cymbal 1"),
    (52, "Chinese Cymbal"),
    (53, "Ride Bell"),
    (54, "Tambourine"),
    (55, "Splash Cymbal"),
    (56, "Cowbell"),
    (57, "Crash Cymbal 2"),
];

#[repr(C)]
struct Lv2UridMap {
    handle: *mut c_void,
    map: unsafe extern "C" fn(handle: *mut c_void, uri: *const c_char) -> u32,
}

struct UridState {
    uris: Vec<String>,
    map: HashMap<String, u32>,
}

unsafe extern "C" fn urid_map_fn(handle: *mut c_void, uri_ptr: *const c_char) -> u32 {
    let state_arc = unsafe { &*(handle as *const Mutex<UridState>) };
    let uri_str = unsafe { CStr::from_ptr(uri_ptr) }.to_string_lossy().to_string();
    let mut state = state_arc.lock().unwrap();
    if let Some(&id) = state.map.get(&uri_str) {
        return id;
    }
    let id = (state.uris.len() as u32) + 1;
    state.uris.push(uri_str.clone());
    state.map.insert(uri_str, id);
    id
}

#[repr(C, align(8))]
struct AtomSequenceBuffer {
    atom_size: u32,
    atom_type: u32,
    unit: u32,
    pad: u32,
    data: [u8; 2048],
}

#[derive(Clone, Debug)]
pub struct Lv2ControlPort {
    pub index: usize,
    pub name: String,
    pub group: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    pub param_type: JamSeqParamType,
}

#[repr(C)]
struct Lv2WorkerSchedule {
    handle: *mut c_void,
    schedule_work: unsafe extern "C" fn(handle: *mut c_void, size: u32, data: *const c_void) -> i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Lv2WorkerInterface {
    work: unsafe extern "C" fn(
        instance: *mut c_void,
        respond: unsafe extern "C" fn(handle: *mut c_void, size: u32, data: *const c_void) -> i32,
        handle: *mut c_void,
        size: u32,
        data: *const c_void,
    ) -> i32,
    work_response: unsafe extern "C" fn(instance: *mut c_void, size: u32, body: *const c_void) -> i32,
    end_run: Option<unsafe extern "C" fn(instance: *mut c_void) -> i32>,
}

struct WorkerState {
    instance_ptr: *mut c_void,
    iface: Option<Lv2WorkerInterface>,
}

unsafe extern "C" fn worker_respond_fn(handle: *mut c_void, size: u32, data: *const c_void) -> i32 {
    let state = unsafe { &*(handle as *const Mutex<WorkerState>) };
    let guard = state.lock().unwrap();
    if let Some(iface) = guard.iface {
        if !guard.instance_ptr.is_null() {
            unsafe {
                let _ = (iface.work_response)(guard.instance_ptr, size, data);
                if let Some(end_run) = iface.end_run {
                    let _ = end_run(guard.instance_ptr);
                }
            }
        }
    }
    0
}

unsafe extern "C" fn worker_schedule_fn(handle: *mut c_void, size: u32, data: *const c_void) -> i32 {
    let state = unsafe { &*(handle as *const Mutex<WorkerState>) };
    let (inst, iface) = {
        let guard = state.lock().unwrap();
        (guard.instance_ptr, guard.iface)
    };
    if let Some(iface) = iface {
        if !inst.is_null() {
            unsafe {
                let _ = (iface.work)(inst, worker_respond_fn, handle, size, data);
            }
        }
    }
    0
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Lv2MidnamInterface {
    pub midnam: unsafe extern "C" fn(instance: *mut c_void) -> *mut c_char,
    pub model: unsafe extern "C" fn(instance: *mut c_void) -> *mut c_char,
    pub free: Option<unsafe extern "C" fn(text: *mut c_char)>,
}

pub struct Lv2Instance {
    pub uri: String,
    pub plugin_name: String,
    instance: Arc<Mutex<lilv::instance::ActiveInstance>>,
    midi_port: Option<usize>,
    notify_port: Option<usize>,
    audio_ins: Vec<usize>,
    out_l_port: usize,
    out_r_port: usize,
    control_ports: Arc<Mutex<Vec<Lv2ControlPort>>>,
    port_index_map: Arc<Mutex<HashMap<String, usize>>>,
    note_names: Arc<Mutex<Vec<JamSeqNoteName>>>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    silent_input: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    atom_buf: Box<AtomSequenceBuffer>,
    notify_buf: Box<AtomSequenceBuffer>,
    seq_urid: u32,
    midi_urid: u32,
}

impl Lv2Instance {
    pub fn load(plugin_uri: &str, sample_rate: f64, max_block_size: u32) -> Result<Self> {
        let world = Box::leak(Box::new(World::with_load_all()));
        let uri_node = world.new_uri(plugin_uri);
        let plugin = world
            .plugins()
            .into_iter()
            .find(|p| p.uri() == uri_node)
            .ok_or_else(|| anyhow::anyhow!("LV2 plugin URI not found: {}", plugin_uri))?;

        let plugin_name = plugin.name().as_str().unwrap_or("LV2 Plugin").to_string();

        let urid_state = Box::leak(Box::new(Mutex::new(UridState {
            uris: Vec::new(),
            map: HashMap::new(),
        })));
        let urid_map_struct = Box::leak(Box::new(Lv2UridMap {
            handle: (urid_state as *const Mutex<UridState>) as *mut c_void,
            map: urid_map_fn,
        }));

        let worker_state = Box::leak(Box::new(Mutex::new(WorkerState {
            instance_ptr: std::ptr::null_mut(),
            iface: None,
        })));
        let worker_sched_struct = Box::leak(Box::new(Lv2WorkerSchedule {
            handle: (worker_state as *const Mutex<WorkerState>) as *mut c_void,
            schedule_work: worker_schedule_fn,
        }));

        let feat_urid_map = lv2_raw::LV2Feature {
            uri: CString::new("http://lv2plug.in/ns/ext/urid#map")?.into_raw(),
            data: (urid_map_struct as *const Lv2UridMap) as *mut c_void,
        };
        let feat_worker = lv2_raw::LV2Feature {
            uri: CString::new("http://lv2plug.in/ns/ext/worker#schedule")?.into_raw(),
            data: (worker_sched_struct as *const Lv2WorkerSchedule) as *mut c_void,
        };
        let feat_buf_sz = lv2_raw::LV2Feature {
            uri: CString::new("http://lv2plug.in/ns/ext/buf-size#boundedBlockLength")?.into_raw(),
            data: std::ptr::null_mut(),
        };

        #[repr(C)]
        struct Lv2Option {
            context: i32,
            subject: u32,
            key: u32,
            size: u32,
            type_: u32,
            value: *const c_void,
        }

        let max_block_val = Box::leak(Box::new(max_block_size.max(4096)));
        let min_block_val = Box::leak(Box::new(32u32));
        let int_type_urid = {
            let c = CString::new("http://lv2plug.in/ns/ext/atom#Int")?;
            unsafe { urid_map_fn(urid_map_struct.handle, c.as_ptr()) }
        };
        let max_block_urid = {
            let c = CString::new("http://lv2plug.in/ns/ext/buf-size#maxBlockLength")?;
            unsafe { urid_map_fn(urid_map_struct.handle, c.as_ptr()) }
        };
        let min_block_urid = {
            let c = CString::new("http://lv2plug.in/ns/ext/buf-size#minBlockLength")?;
            unsafe { urid_map_fn(urid_map_struct.handle, c.as_ptr()) }
        };

        let options_array = Box::leak(Box::new([
            Lv2Option {
                context: 0,
                subject: 0,
                key: max_block_urid,
                size: 4,
                type_: int_type_urid,
                value: (max_block_val as *const u32) as *const c_void,
            },
            Lv2Option {
                context: 0,
                subject: 0,
                key: min_block_urid,
                size: 4,
                type_: int_type_urid,
                value: (min_block_val as *const u32) as *const c_void,
            },
            Lv2Option {
                context: 0,
                subject: 0,
                key: 0,
                size: 0,
                type_: 0,
                value: std::ptr::null(),
            },
        ]));

        let feat_options = lv2_raw::LV2Feature {
            uri: CString::new("http://lv2plug.in/ns/ext/options#options")?.into_raw(),
            data: (options_array.as_ptr()) as *mut c_void,
        };

        let features = [feat_urid_map, feat_worker, feat_buf_sz, feat_options];

        let instance = unsafe {
            plugin
                .instantiate(sample_rate, &features)
                .ok_or_else(|| anyhow::anyhow!("Failed to instantiate LV2 plugin: {}", plugin_name))?
        };

        // Wire instance pointer and worker interface for background sample/asset loading
        if let Some(iface_ptr) = unsafe {
            instance.extension_data::<Lv2WorkerInterface>("http://lv2plug.in/ns/ext/worker#interface")
        } {
            let iface = unsafe { *iface_ptr.as_ptr() };
            let raw_handle = instance.handle();
            let mut guard = worker_state.lock().unwrap();
            guard.instance_ptr = raw_handle;
            guard.iface = Some(iface);
        }

        // Introspect Note Names via MIDNAM interface
        let mut note_names_vec = Vec::new();
        if let Some(midnam_iface_ptr) = unsafe {
            instance.extension_data::<Lv2MidnamInterface>("http://ardour.org/lv2/midnam#interface")
        } {
            let iface = unsafe { *midnam_iface_ptr.as_ptr() };
            let raw_handle = instance.handle();
            let xml_ptr = unsafe { (iface.midnam)(raw_handle) };
            if !xml_ptr.is_null() {
                let xml_str = unsafe { CStr::from_ptr(xml_ptr) }.to_string_lossy();
                let parsed_notes = parse_midnam_xml(&xml_str);
                for (pitch, name) in parsed_notes {
                    note_names_vec.push(JamSeqNoteName::new(pitch, &name));
                }
                if let Some(free_fn) = iface.free {
                    unsafe { free_fn(xml_ptr) };
                }
            }
        }

        // Fallback to General MIDI drum table if note names are empty and plugin is a drum/percussion kit
        if note_names_vec.is_empty() {
            let lower_name = plugin_name.to_lowercase();
            let lower_uri = plugin_uri.to_lowercase();
            if lower_name.contains("drum")
                || lower_name.contains("kit")
                || lower_name.contains("percussion")
                || lower_uri.contains("drum")
                || lower_uri.contains("kit")
            {
                for &(pitch, name) in GM_DRUM_MAP {
                    note_names_vec.push(JamSeqNoteName::new(pitch, name));
                }
            }
        }

        // Port discovery
        let num_ports = plugin.ports_count();
        let mut midi_port = None;
        let mut notify_port = None;
        let mut audio_ins = Vec::new();
        let mut audio_outs = Vec::new();
        let mut control_ports = Vec::new();
        let mut port_index_map = HashMap::new();

        let input_class = world.new_uri("http://lv2plug.in/ns/lv2core#InputPort");
        let output_class = world.new_uri("http://lv2plug.in/ns/lv2core#OutputPort");
        let control_class = world.new_uri("http://lv2plug.in/ns/lv2core#ControlPort");
        let audio_class = world.new_uri("http://lv2plug.in/ns/lv2core#AudioPort");
        let atom_class = world.new_uri("http://lv2plug.in/ns/ext/atom#AtomPort");
        let toggled_class = world.new_uri("http://lv2plug.in/ns/lv2core#toggled");
        let integer_class = world.new_uri("http://lv2plug.in/ns/lv2core#integer");
        let enum_class = world.new_uri("http://lv2plug.in/ns/lv2core#enumeration");

        for i in 0..num_ports {
            if let Some(port) = plugin.port_by_index(i) {
                let sym_node = port.symbol();
                let symbol = sym_node
                    .as_ref()
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();

                let is_input = port.is_a(&input_class);
                let is_output = port.is_a(&output_class);
                let is_control = port.is_a(&control_class);
                let is_audio = port.is_a(&audio_class);
                let is_atom = port.is_a(&atom_class);

                if is_input && is_audio {
                    audio_ins.push(i);
                } else if is_input && (is_atom || symbol == "control" || symbol.contains("midi")) {
                    if midi_port.is_none() {
                        midi_port = Some(i);
                    }
                } else if is_output && (is_atom || symbol == "notify") {
                    notify_port = Some(i);
                } else if is_output && is_audio {
                    audio_outs.push(i);
                } else if is_input && is_control {
                    let name = port
                        .name()
                        .and_then(|n| n.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| {
                            if !symbol.is_empty() {
                                symbol.clone()
                            } else {
                                format!("Param {}", i)
                            }
                        });
                    let range = port.range();
                    let def_val = range.default.as_ref().and_then(|n| n.as_float()).unwrap_or(0.0);
                    let min_val = range.minimum.as_ref().and_then(|n| n.as_float()).unwrap_or(0.0);
                    let max_val = range.maximum.as_ref().and_then(|n| n.as_float()).unwrap_or(1.0);

                    let is_toggled = port.has_property(&toggled_class);
                    let is_integer = port.has_property(&integer_class);
                    let is_enumeration = port.has_property(&enum_class);

                    let sp_coll = port.scale_points();
                    let sp_count = sp_coll.count();

                    let param_type = if is_toggled
                        || (min_val == 0.0 && max_val == 1.0 && (is_integer || sp_count == 2))
                    {
                        JamSeqParamType::Bool
                    } else if (is_enumeration || sp_count > 0) && sp_count <= 64 {
                        JamSeqParamType::Enum
                    } else if is_integer {
                        JamSeqParamType::Int
                    } else {
                        JamSeqParamType::Float
                    };

                    let (group, display_name) = if let Some(slash_idx) = name.rfind('/') {
                        let g = name[..slash_idx].trim().to_string();
                        let n = name[slash_idx + 1..].trim().to_string();
                        (
                            if g.is_empty() {
                                "General".to_string()
                            } else {
                                g.replace('/', " > ")
                            },
                            if n.is_empty() { name.clone() } else { n },
                        )
                    } else if let Some(colon_idx) = name.find(':') {
                        let g = name[..colon_idx].trim().to_string();
                        let n = name[colon_idx + 1..].trim().to_string();
                        (
                            if g.is_empty() {
                                "General".to_string()
                            } else {
                                g
                            },
                            if n.is_empty() { name.clone() } else { n },
                        )
                    } else {
                        ("General".to_string(), name.clone())
                    };

                    let group = if group == "General" {
                        let n = display_name.to_lowercase();
                        if n.contains("cutoff")
                            || n.contains("reso")
                            || n.contains("drive")
                            || n.contains("filter")
                        {
                            "Filter".to_string()
                        } else if n.contains("attack")
                            || n.contains("decay")
                            || n.contains("sustain")
                            || n.contains("release")
                            || n.contains("env")
                        {
                            "Amp Envelope".to_string()
                        } else if n.contains("glide")
                            || n.contains("voice")
                            || n.contains("portamento")
                            || n.contains("tuning")
                        {
                            "Voice".to_string()
                        } else if n.contains("gain")
                            || n.contains("volume")
                            || n.contains("master")
                            || n.contains("pan")
                        {
                            "Output".to_string()
                        } else {
                            "General".to_string()
                        }
                    } else {
                        group
                    };

                    let idx = control_ports.len();
                    port_index_map.insert(display_name.clone(), idx);
                    port_index_map.insert(display_name.to_lowercase(), idx);
                    if display_name != name {
                        port_index_map.insert(name.clone(), idx);
                        port_index_map.insert(name.to_lowercase(), idx);
                    }
                    if !symbol.is_empty() {
                        port_index_map.insert(symbol.clone(), idx);
                    }

                    control_ports.push(Lv2ControlPort {
                        index: i,
                        name: display_name,
                        group,
                        min: min_val,
                        max: max_val,
                        default: def_val,
                        value: def_val,
                        param_type,
                    });
                }
            }
        }

        let out_l_port = if audio_outs.len() >= 2 {
            audio_outs[0]
        } else {
            2.min(num_ports.saturating_sub(1))
        };
        let out_r_port = if audio_outs.len() >= 2 {
            audio_outs[1]
        } else {
            3.min(num_ports.saturating_sub(1))
        };

        let active_instance = unsafe { instance.activate() };
        let active_arc = Arc::new(Mutex::new(active_instance));

        let seq_urid = unsafe {
            let c = CString::new("http://lv2plug.in/ns/ext/atom#Sequence")?;
            urid_map_fn(urid_map_struct.handle, c.as_ptr())
        };
        let midi_urid = unsafe {
            let c = CString::new("http://lv2plug.in/ns/ext/midi#MidiEvent")?;
            urid_map_fn(urid_map_struct.handle, c.as_ptr())
        };

        let mut atom_buf = Box::new(AtomSequenceBuffer {
            atom_size: 0,
            atom_type: seq_urid,
            unit: 0,
            pad: 0,
            data: [0u8; 2048],
        });
        let mut notify_buf = Box::new(AtomSequenceBuffer {
            atom_size: 0,
            atom_type: seq_urid,
            unit: 0,
            pad: 0,
            data: [0u8; 2048],
        });

        // Pre-connect atom ports
        {
            let mut act = active_arc.lock().unwrap();
            unsafe {
                if let Some(p) = midi_port {
                    act.instance_mut()
                        .connect_port_mut(p, &mut *atom_buf as *mut AtomSequenceBuffer);
                }
                if let Some(p) = notify_port {
                    act.instance_mut()
                        .connect_port_mut(p, &mut *notify_buf as *mut AtomSequenceBuffer);
                }
            }
        }

        Ok(Self {
            uri: plugin_uri.to_string(),
            plugin_name,
            instance: active_arc,
            midi_port,
            notify_port,
            audio_ins,
            out_l_port,
            out_r_port,
            control_ports: Arc::new(Mutex::new(control_ports)),
            port_index_map: Arc::new(Mutex::new(port_index_map)),
            note_names: Arc::new(Mutex::new(note_names_vec)),
            in_l: vec![0.0f32; 2048],
            in_r: vec![0.0f32; 2048],
            silent_input: vec![0.0f32; 2048],
            out_l: vec![0.0f32; 2048],
            out_r: vec![0.0f32; 2048],
            atom_buf,
            notify_buf,
            seq_urid,
            midi_urid,
        })
    }

    pub fn process(
        &mut self,
        inputs: Option<&JamSeqAudioBuffer>,
        outputs: &mut JamSeqAudioBuffer,
        midi_events: &[JamSeqMidiEvent],
    ) {
        let num_samples = outputs.num_samples as usize;
        if self.out_l.len() < num_samples {
            self.out_l.resize(num_samples, 0.0);
            self.out_r.resize(num_samples, 0.0);
        }
        if self.silent_input.len() < num_samples {
            self.silent_input.resize(num_samples, 0.0);
        }
        if self.in_l.len() < num_samples {
            self.in_l.resize(num_samples, 0.0);
            self.in_r.resize(num_samples, 0.0);
        }

        self.silent_input[..num_samples].fill(0.0);
        self.out_l[..num_samples].fill(0.0);
        self.out_r[..num_samples].fill(0.0);

        // Copy input audio if provided
        let has_inputs = if let Some(in_buf) = inputs {
            if in_buf.num_samples > 0 && in_buf.num_channels > 0 && !in_buf.channel_data.is_null() {
                unsafe {
                    let in_l_ptr = *in_buf.channel_data.add(0);
                    let in_l_slice = std::slice::from_raw_parts(in_l_ptr, num_samples);
                    self.in_l[..num_samples].copy_from_slice(in_l_slice);

                    if in_buf.num_channels > 1 {
                        let in_r_ptr = *in_buf.channel_data.add(1);
                        let in_r_slice = std::slice::from_raw_parts(in_r_ptr, num_samples);
                        self.in_r[..num_samples].copy_from_slice(in_r_slice);
                    } else {
                        self.in_r[..num_samples].copy_from_slice(in_l_slice);
                    }
                }
                true
            } else {
                false
            }
        } else {
            false
        };

        // Construct Atom Sequence from MIDI events
        let mut offset = 0usize;
        for ev in midi_events {
            let status = ev.status;
            let data1 = ev.data1;
            let data2 = ev.data2;

            if offset + 19 <= self.atom_buf.data.len() {
                let time_frames = ev.tick_offset as i64;
                self.atom_buf.data[offset..offset + 8].copy_from_slice(&time_frames.to_ne_bytes());
                offset += 8;

                // Event size = 3 bytes
                self.atom_buf.data[offset..offset + 4].copy_from_slice(&3u32.to_ne_bytes());
                offset += 4;

                // Event type = midi_urid
                self.atom_buf.data[offset..offset + 4]
                    .copy_from_slice(&self.midi_urid.to_ne_bytes());
                offset += 4;

                // 3 MIDI payload bytes
                self.atom_buf.data[offset..offset + 3]
                    .copy_from_slice(&[status, data1, data2]);
                offset += 3;

                // Pad to 8-byte alignment
                let pad = (8 - (offset % 8)) % 8;
                for i in 0..pad {
                    self.atom_buf.data[offset + i] = 0;
                }
                offset += pad;
            }
        }

        self.atom_buf.atom_size = (8 + offset) as u32;
        self.atom_buf.atom_type = self.seq_urid;
        self.atom_buf.unit = 0;
        self.atom_buf.pad = 0;

        self.notify_buf.atom_size = 2040;
        self.notify_buf.atom_type = self.seq_urid;
        self.notify_buf.unit = 0;
        self.notify_buf.pad = 0;

        if let Ok(mut inst) = self.instance.lock() {
            unsafe {
                if let Some(p) = self.midi_port {
                    inst.instance_mut()
                        .connect_port_mut(p, &mut *self.atom_buf as *mut AtomSequenceBuffer);
                }
                if let Some(p) = self.notify_port {
                    inst.instance_mut()
                        .connect_port_mut(p, &mut *self.notify_buf as *mut AtomSequenceBuffer);
                }

                // Connect audio inputs
                if has_inputs {
                    for (idx, &in_port) in self.audio_ins.iter().enumerate() {
                        let ptr = match idx {
                            0 => self.in_l.as_mut_ptr(),
                            1 => self.in_r.as_mut_ptr(),
                            _ => self.silent_input.as_mut_ptr(),
                        };
                        inst.instance_mut().connect_port_mut(in_port, ptr);
                    }
                } else {
                    for &in_port in &self.audio_ins {
                        inst.instance_mut()
                            .connect_port_mut(in_port, self.silent_input.as_mut_ptr());
                    }
                }

                // Connect audio outputs
                inst.instance_mut()
                    .connect_port_mut(self.out_l_port, self.out_l.as_mut_ptr());
                inst.instance_mut()
                    .connect_port_mut(self.out_r_port, self.out_r.as_mut_ptr());

                // Connect control ports to live values
                if let Ok(mut ports) = self.control_ports.lock() {
                    for port in ports.iter_mut() {
                        inst.instance_mut()
                            .connect_port_mut(port.index, &mut port.value as *mut f32);
                    }
                }

                inst.run(num_samples);
            }
        }

        // Copy rendered audio to outputs
        unsafe {
            if outputs.num_channels > 0 && !outputs.channel_data.is_null() {
                let out_l_ptr = *outputs.channel_data.add(0);
                let out_l_slice = std::slice::from_raw_parts_mut(out_l_ptr, num_samples);
                out_l_slice.copy_from_slice(&self.out_l[..num_samples]);

                if outputs.num_channels > 1 {
                    let out_r_ptr = *outputs.channel_data.add(1);
                    let out_r_slice = std::slice::from_raw_parts_mut(out_r_ptr, num_samples);
                    out_r_slice.copy_from_slice(&self.out_r[..num_samples]);
                }
            }
        }
    }

    pub fn get_parameters(&self) -> *mut JamSeqParameterList {
        let ports = self.control_ports.lock().unwrap();
        let items: Vec<(String, String, String, f32, f32, f32, f32, JamSeqParamType)> = ports
            .iter()
            .map(|p| {
                (
                    p.index.to_string(),
                    p.name.clone(),
                    p.group.clone(),
                    p.min,
                    p.max,
                    p.default,
                    p.value,
                    p.param_type.clone(),
                )
            })
            .collect();

        allocate_c_parameter_list(&items)
    }

    pub fn set_parameter(&self, param_id: &str, value: f32) {
        let mut ports = self.control_ports.lock().unwrap();
        let map = self.port_index_map.lock().unwrap();

        let target_idx = if let Ok(idx) = param_id.parse::<usize>() {
            ports.iter().position(|p| p.index == idx)
        } else {
            map.get(param_id)
                .or_else(|| map.get(&param_id.to_lowercase()))
                .copied()
        };

        if let Some(i) = target_idx {
            if let Some(port) = ports.get_mut(i) {
                port.value = value.clamp(port.min, port.max);
            }
        }
    }

    pub fn get_note_names(&self) -> *mut JamSeqNoteNameList {
        let names = self.note_names.lock().unwrap();
        allocate_c_note_name_list(&names)
    }
}

/// Parses MIDNAM XML payload extracting `<Note Number="..." Name="..." />` elements.
pub fn parse_midnam_xml(xml: &str) -> Vec<(u8, String)> {
    let mut notes = Vec::new();
    let lower = xml.to_lowercase();
    let mut cursor = 0;

    while let Some(note_start) = lower[cursor..].find("<note ") {
        let abs_start = cursor + note_start;
        let tag_content_start = abs_start + 6;
        let tag_content_end = match xml[tag_content_start..].find('>') {
            Some(idx) => tag_content_start + idx,
            None => break,
        };

        let tag_text = &xml[tag_content_start..tag_content_end];
        cursor = tag_content_end + 1;

        let number_val = extract_attr_val(tag_text, "number")
            .and_then(|s| s.parse::<u8>().ok());
        let name_val = extract_attr_val(tag_text, "name");

        if let (Some(pitch), Some(name)) = (number_val, name_val) {
            if pitch <= 127 && !name.trim().is_empty() {
                notes.push((pitch, name.trim().to_string()));
            }
        }
    }

    notes.sort_by_key(|&(pitch, _)| pitch);
    notes.dedup_by_key(|&mut (pitch, _)| pitch);
    notes
}

fn extract_attr_val(tag: &str, attr: &str) -> Option<String> {
    let lower_tag = tag.to_lowercase();
    let lower_attr = attr.to_lowercase();
    let pos = lower_tag.find(&lower_attr)?;
    let after_attr = &tag[pos + lower_attr.len()..];
    let eq_idx = after_attr.find('=')?;
    let after_eq = after_attr[eq_idx + 1..].trim_start();
    let quote_char = after_eq.chars().next()?;
    if quote_char == '"' || quote_char == '\'' {
        let val_start = 1;
        let end_quote = after_eq[val_start..].find(quote_char)?;
        Some(after_eq[val_start..val_start + end_quote].to_string())
    } else {
        let end_idx = after_eq
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .unwrap_or(after_eq.len());
        Some(after_eq[..end_idx].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_midnam_xml_basic() {
        let xml = r#"
        <MIDINameDocument>
          <MasterDeviceNames>
            <NoteList Name="Notes">
              <Note Number="36" Name="Kick Drum" />
              <Note Number="38" Name="Snare Center" />
              <Note Number="42" Name="Closed Hat" />
            </NoteList>
          </MasterDeviceNames>
        </MIDINameDocument>
        "#;
        let notes = parse_midnam_xml(xml);
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0], (36, "Kick Drum".to_string()));
        assert_eq!(notes[1], (38, "Snare Center".to_string()));
        assert_eq!(notes[2], (42, "Closed Hat".to_string()));
    }

    #[test]
    fn test_parse_midnam_xml_reversed_attrs() {
        let xml = r#"<NoteList><Note Name="Bass Drum" Number="35"/><Note Name="Snare" Number="38"/></NoteList>"#;
        let notes = parse_midnam_xml(xml);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0], (35, "Bass Drum".to_string()));
        assert_eq!(notes[1], (38, "Snare".to_string()));
    }
}
