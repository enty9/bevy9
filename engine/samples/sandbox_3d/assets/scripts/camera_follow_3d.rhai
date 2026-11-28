// Sandbox 3D smooth third-person follow camera for MainCamera3D.
// Editable via Inspector `ScriptProperties`:
//   target_name, follow_offset, smoothness, look_height

fn on_update(dt) {
    let target = world.find_by_name(props.target_name);
    if target == () {
        return;
    }

    let target_pos = world.get_component_field(target, "Transform", "translation");
    if target_pos == () {
        return;
    }

    let desired = target_pos + props.follow_offset;
    let alpha = props.smoothness * dt;
    if alpha > 1.0 {
        alpha = 1.0;
    }
    if alpha < 0.0 {
        alpha = 0.0;
    }

    let current = transform.translation;
    transform.set_translation(current + (desired - current) * alpha);
    transform.look_at(target_pos + vec3(0.0, props.look_height, 0.0));
}
