// Sandbox 2D trigger zone script with audio chime and score tracking.
// Editable via Inspector `ScriptProperties`:
//   pickup_sound, score_value, trigger_count, total_score

fn on_start() {
    events.log("Sandbox 2D trigger zone ready");
}

fn on_trigger_enter(other) {
    props.trigger_count += 1;
    props.total_score += props.score_value;

    audio.play_with_options(props.pickup_sound, #{
        bus: "sfx",
        gain: 0.85,
        pitch: 1.1,
    });

    events.log(
        "2D Trigger zone hit by " + other.name + " (score=" + props.total_score + ")",
    );
    events.emit("trigger_collected_2d", #{
        other: other.name,
        count: props.trigger_count,
        score: props.total_score,
    });
}

fn on_trigger_exit(other) {
    events.emit("trigger_exited_2d", #{ other: other.name });
}
