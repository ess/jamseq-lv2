use jamseq_lv2::LV2_ADAPTER_VTABLE;
use jamseq_plugin_api::{
    AudioPluginInstance, JamSeqGetHostAdapterFn, JamSeqMidiEvent, JamSeqPluginType,
    PluginHostAdapter, StackAudioOutputView, JAMSEQ_API_VERSION, JAMSEQ_ENTRYPOINT_NAME,
};

#[test]
fn test_lv2_vtable_metadata_and_format() {
    assert_eq!(LV2_ADAPTER_VTABLE.api_version, JAMSEQ_API_VERSION);

    let adapter = PluginHostAdapter::new(&LV2_ADAPTER_VTABLE).expect("Valid API version");
    assert_eq!(adapter.format_id(), "lv2");

    assert!(adapter.handles("http://gareus.org/oss/lv2/avldrums#BlackPearl"));
    assert!(adapter.handles("https://example.com/lv2/synth"));
    assert!(adapter.handles("lv2:filter"));
    assert!(adapter.handles("/usr/lib/lv2/avldrums.lv2"));
    assert!(adapter.handles("avldrums.lv2"));
    assert!(!adapter.handles("plugin.clap"));
    assert!(!adapter.handles("plugin.vst3"));
}

#[test]
fn test_lv2_scanning_and_classification() {
    let adapter = PluginHostAdapter::new(&LV2_ADAPTER_VTABLE).expect("Valid API version");
    let plugins = adapter.scan();

    println!("Scanned {} LV2 plugins on system", plugins.len());
    for p in plugins.iter().take(20) {
        println!("  • Discovered: {} ({:?}) -> {}", p.name, p.plugin_type, p.uri_or_path);
    }

    if let Some(avl) = plugins.iter().find(|p| p.name.contains("Black Pearl") || p.uri_or_path.contains("avldrums")) {
        assert_eq!(avl.plugin_type, JamSeqPluginType::Instrument);
    }

    if let Some(surge_fx) = plugins.iter().find(|p| p.name.contains("Surge XT Effects") || p.uri_or_path.contains("surge-xt-effects")) {
        assert_eq!(surge_fx.plugin_type, JamSeqPluginType::AudioEffect);
    }
}

#[test]
fn test_lv2_instance_playback_params_and_lifecycle() {
    let adapter = PluginHostAdapter::new(&LV2_ADAPTER_VTABLE).expect("Valid API version");

    let candidate_uris = [
        "http://gareus.org/oss/lv2/avldrums#BlackPearl",
        "http://gareus.org/oss/lv2/avldrums#RedZeppelin",
        "http://zamaudio.com/lv2/zamcomp",
        "http://zamaudio.com/lv2/zamdelay",
    ];

    let mut found_uri = None;
    for &uri in &candidate_uris {
        if adapter.scan().iter().any(|p| p.uri_or_path == uri) {
            found_uri = Some(uri);
            break;
        }
    }

    if let Some(uri) = found_uri {
        println!("Testing LV2 instance with plugin: {}", uri);
        let mut instance = adapter
            .create_instance(uri, 48000.0, 512)
            .expect("Failed to create LV2 instance");

        // Introspect parameters
        let params = instance.get_parameters();
        println!("Introspected {} parameters", params.len());
        if !params.is_empty() {
            let first_param = &params[0];
            let test_val = (first_param.min + first_param.max) / 2.0;
            instance.set_parameter(&first_param.id, test_val);
            println!("Set parameter '{}' to {}", first_param.name, test_val);
        }

        // Introspect note names
        let note_names = instance.get_note_names();
        println!("Introspected {} note names", note_names.len());
        if uri.contains("avldrums") {
            assert!(
                !note_names.is_empty(),
                "AVL Drums should introspect note names via MIDNAM or GM fallback"
            );
            println!("First note name: pitch {} -> {}", note_names[0].pitch, note_names[0].name);
        }

        // Process a block of audio and MIDI
        let mut out_l = vec![0.0f32; 256];
        let mut out_r = vec![0.0f32; 256];
        let mut out_chans: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
        let mut out_view = StackAudioOutputView::<2>::new(&mut out_chans);

        let midi_events = vec![
            JamSeqMidiEvent::note_on(0, 0, 36, 100), // Kick drum
            JamSeqMidiEvent::note_off(128, 0, 36),
        ];

        instance.process(None, out_view.as_raw_mut(), &midi_events);
        println!("Process block complete without panic or crash");

        // Drop instance cleanly
        drop(instance);
        println!("Instance dropped cleanly");
    } else {
        println!("No candidate LV2 test plugins discovered; skipping instance lifecycle test");
    }
}

#[test]
fn test_lv2_effect_parameters_and_audio_processing() {
    let adapter = PluginHostAdapter::new(&LV2_ADAPTER_VTABLE).expect("Valid API version");

    let candidate_fx = [
        "http://distrho.sf.net/plugins/MVerb",
        "http://distrho.sf.net/plugins/3BandEQ",
        "http://zamaudio.com/lv2/zamcomp",
        "http://zamaudio.com/lv2/zamdelay",
    ];

    let found_fx = candidate_fx
        .iter()
        .find(|&&uri| adapter.scan().iter().any(|p| p.uri_or_path == uri));

    if let Some(&uri) = found_fx {
        println!("Testing LV2 audio effect: {}", uri);
        let mut instance = adapter
            .create_instance(uri, 48000.0, 512)
            .expect("Failed to create LV2 effect instance");

        let params = instance.get_parameters();
        println!("Effect introspected {} parameters", params.len());
        assert!(!params.is_empty(), "Audio effect should expose control parameters");

        let first = &params[0];
        let mid_val = (first.min + first.max) / 2.0;
        instance.set_parameter(&first.id, mid_val);
        println!("Set effect parameter '{}' to {}", first.name, mid_val);

        // Process with audio input
        let in_l = vec![0.5f32; 256];
        let in_r = vec![0.5f32; 256];
        let in_chans: [&[f32]; 2] = [&in_l, &in_r];
        let mut in_view = jamseq_plugin_api::StackAudioInputView::<2>::new(&in_chans);

        let mut out_l = vec![0.0f32; 256];
        let mut out_r = vec![0.0f32; 256];
        let mut out_chans: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
        let mut out_view = StackAudioOutputView::<2>::new(&mut out_chans);

        instance.process(Some(in_view.as_raw()), out_view.as_raw_mut(), &[]);
        println!("Effect process block completed cleanly");
    } else {
        println!("No candidate LV2 audio effect plugins found; skipping effect test");
    }
}

#[test]
fn test_dynamic_cdylib_loading_via_libloading() {
    let dylib_path = if cfg!(target_os = "linux") {
        "../target/debug/libjamseq_lv2.so"
    } else if cfg!(target_os = "macos") {
        "../target/debug/libjamseq_lv2.dylib"
    } else {
        "../target/debug/jamseq_lv2.dll"
    };

    if !std::path::Path::new(dylib_path).exists() {
        eprintln!(
            "Dynamic library '{}' not yet compiled; skipping dynamic load test",
            dylib_path
        );
        return;
    }

    unsafe {
        let lib = libloading::Library::new(dylib_path)
            .expect("Failed to load libjamseq_lv2 dynamic shared library");

        let get_adapter: libloading::Symbol<JamSeqGetHostAdapterFn> = lib
            .get(JAMSEQ_ENTRYPOINT_NAME.as_bytes())
            .expect("Exported symbol jamseq_get_host_adapter not found");

        let vtable_ptr = get_adapter();
        assert!(!vtable_ptr.is_null(), "Adapter vtable pointer must not be null");

        let adapter = PluginHostAdapter::new(&*vtable_ptr).expect("Valid API version");
        assert_eq!(adapter.format_id(), "lv2");
        assert!(adapter.handles("http://gareus.org/oss/lv2/avldrums#BlackPearl"));
    }
}
