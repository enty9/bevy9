// Sandbox 3D collectible/boost trigger volume script.
// Editable via Inspector `ScriptProperties`:
//   pickup_sound, boost_impulse, score_value, trigger_count, total_score

fn on_start() {
    events.log("Sandbox 3D trigger zone ready");
}

fn on_trigger_enter(other) {
    props.trigger_count += 1;
    props.total_score += props.score_value;

    audio.play_with_options(props.pickup_sound, #{
        bus: "sfx",
        gain: 0.85,
        pitch: 1.05,
    });

    if other.name == "HeroCube" && props.boost_impulse > 0.0 {
        physics.apply_impulse_3d_on(other.entity, 0.0, props.boost_impulse, 0.0);
    }

    events.log(
        "Trigger zone activated by " + other.name + " (score=" + props.total_score + ")",
    );
    events.emit("trigger_collected_3d", #{
        other: other.name,
        count: props.trigger_count,
        score: props.total_score,
    });
}

fn on_trigger_exit(other) {
    events.emit("trigger_exited_3d", #{ other: other.name });
}
