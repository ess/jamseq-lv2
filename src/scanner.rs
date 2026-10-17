use jamseq_plugin_api::JamSeqPluginType;
use lilv::World;

/// Scans system and user LV2 plugin directories via Lilv and classifies each plugin.
pub fn scan_lv2_plugins() -> Vec<(String, String, String, JamSeqPluginType)> {
    let mut results = Vec::new();
    let world = World::with_load_all();

    let input_class = world.new_uri("http://lv2plug.in/ns/lv2core#InputPort");
    let output_class = world.new_uri("http://lv2plug.in/ns/lv2core#OutputPort");
    let audio_class = world.new_uri("http://lv2plug.in/ns/lv2core#AudioPort");
    let atom_class = world.new_uri("http://lv2plug.in/ns/ext/atom#AtomPort");

    for plugin in world.plugins().into_iter() {
        let uri = plugin.uri().as_str().unwrap_or("").to_string();
        if uri.is_empty() {
            continue;
        }
        let name = plugin.name().as_str().unwrap_or("Unnamed LV2").to_string();

        let class_uri = plugin
            .class()
            .uri()
            .and_then(|u| u.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let parent_uri = plugin
            .class()
            .parent_uri()
            .and_then(|u| u.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let label = plugin.class().label().as_str().unwrap_or_default().to_string();

        let is_inst = class_uri.contains("InstrumentPlugin")
            || parent_uri.contains("InstrumentPlugin")
            || label.contains("Instrument");
        let is_analyzer = class_uri.contains("AnalyserPlugin")
            || parent_uri.contains("AnalyserPlugin")
            || label.contains("Analyser")
            || label.contains("Analyzer");

        let plugin_type = if is_analyzer {
            JamSeqPluginType::Analyzer
        } else if is_inst {
            JamSeqPluginType::Instrument
        } else {
            // Fallback: check port topology
            let num_ports = plugin.ports_count();
            let mut has_audio_in = false;
            let mut has_atom_in = false;
            let mut has_audio_out = false;

            for i in 0..num_ports {
                if let Some(port) = plugin.port_by_index(i) {
                    let is_in = port.is_a(&input_class);
                    let is_out = port.is_a(&output_class);
                    let is_audio = port.is_a(&audio_class);
                    let is_atom = port.is_a(&atom_class);

                    if is_in && is_audio {
                        has_audio_in = true;
                    }
                    if is_in && is_atom {
                        has_atom_in = true;
                    }
                    if is_out && is_audio {
                        has_audio_out = true;
                    }
                }
            }

            if has_atom_in && !has_audio_in && has_audio_out {
                JamSeqPluginType::Instrument
            } else {
                JamSeqPluginType::AudioEffect
            }
        };

        results.push((name, uri, "LV2".to_string(), plugin_type));
    }

    results
}
