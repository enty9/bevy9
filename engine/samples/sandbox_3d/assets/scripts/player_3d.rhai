// Sandbox 3D player controller for HeroCube (WASD movement + Space jump + audio + respawn).
// Editable via Inspector `ScriptProperties`:
//   move_speed, jump_impulse, ground_check_dist, respawn_y, jump_sound, jumps

let spawn_pos = vec3(-0.9, 1.5, 0.0);
let jump_cooldown = 0.0;

fn on_start() {
    spawn_pos = transform.translation;
    events.log("Sandbox 3D player ready (WASD to move, Space to jump)");
}

fn on_fixed_update(dt) {
    if jump_cooldown > 0.0 {
        jump_cooldown -= dt;
    }

    let move_axis = input.axis_3d("move");
    let dir = vec3(move_axis.x, 0.0, move_axis.z);
    if dir.length_squared() > 1.0 {
        dir = dir.normalize();
    }

    let vel = physics.linear_velocity_3d();
    let target_vx = dir.x * props.move_speed;
    let target_vz = dir.z * props.move_speed;

    let accel = if dir.length_squared() > 0.001 { 8.0 } else { 5.0 };
    let blend = accel * dt;
    if blend > 1.0 {
        blend = 1.0;
    }

    // HeroCube is a 1.2m cube (mass ~1.73 kg); steer smoothly toward target horizontal velocity.
    let ix = (target_vx - vel.x) * blend * 1.73;
    let iz = (target_vz - vel.z) * blend * 1.73;
    if ix * ix + iz * iz > 0.0001 {
        physics.apply_impulse_3d(ix, 0.0, iz);
    }

    let ground_hit = physics.raycast_3d(
        transform.translation,
        vec3(0.0, -1.0, 0.0),
        props.ground_check_dist,
    );
    if input.action_just_pressed("jump") && ground_hit != () && jump_cooldown <= 0.0 {
        physics.apply_impulse_3d(0.0, props.jump_impulse, 0.0);
        jump_cooldown = 0.18;
        props.jumps += 1;
        audio.play_with_options(props.jump_sound, #{
            bus: "sfx",
            gain: 0.8,
            pitch: 1.0,
        });
        events.emit("player_3d_jumped", #{ jumps: props.jumps });
    }

    if transform.translation.y < props.respawn_y {
        transform.set_translation(spawn_pos);
        events.log("HeroCube respawned on arena");
    }
}

fn on_collision(other) {
    if other.name == "AccentSphere" {
        events.emit("sphere_bumped", #{ other: other.name });
    }
}
