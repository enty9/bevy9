// Sandbox 2D platformer character controller for PlayerSprite.
// Uses CharacterController2D (`physics.move_character_2d` / `is_grounded_2d`),
// SpriteDesc horizontal flip via Reflect bridge, audio feedback, and collision kick.
// Editable via Inspector `ScriptProperties`:
//   move_speed, jump_speed, bump_impulse, respawn_y, jump_sound, jumps, facing_right

let spawn_pos = vec3(-90.0, 20.0, 1.0);
let jump_vel = 0.0;
let left_ground = false;
let jump_ticks = 0;

fn on_start() {
    spawn_pos = transform.translation;
    events.log("Sandbox 2D player ready (A/D to move, Space/W to jump)");
}

fn on_fixed_update(dt) {
    let move_axis = input.axis_2d("move");
    let vx = move_axis.x * props.move_speed;

    if move_axis.x < -0.05 && props.facing_right {
        props.facing_right = false;
        entity.set_component_field("SpriteDesc", "flip_x", true);
    } else if move_axis.x > 0.05 && !props.facing_right {
        props.facing_right = true;
        entity.set_component_field("SpriteDesc", "flip_x", false);
    }

    let grounded = physics.is_grounded_2d();
    if !grounded {
        left_ground = true;
    }
    if grounded && left_ground {
        jump_vel = 0.0;
        left_ground = false;
        jump_ticks = 0;
    }
    if jump_vel > 0.0 {
        jump_ticks += 1;
        if grounded && jump_ticks > 2 {
            jump_vel = 0.0;
            jump_ticks = 0;
        }
    }

    if (input.action_just_pressed("jump") || move_axis.y > 0.5) && grounded && jump_vel <= 0.0 {
        jump_vel = props.jump_speed;
        left_ground = false;
        jump_ticks = 0;
        props.jumps += 1;
        audio.play_with_options(props.jump_sound, #{
            bus: "sfx",
            gain: 0.8,
            pitch: 1.0,
        });
        events.emit("player_2d_jumped", #{ jumps: props.jumps });
    }

    physics.move_character_2d(vx, jump_vel);

    if transform.translation.y < props.respawn_y {
        transform.set_translation(spawn_pos);
        jump_vel = 0.0;
        left_ground = false;
        jump_ticks = 0;
        events.log("PlayerSprite respawned on 2D platform");
    }
}

fn on_collision(other) {
    if other.name == "AccentCircle" {
        let dir = if props.facing_right { 1.0 } else { -1.0 };
        physics.apply_impulse_2d_on(
            other.entity,
            dir * props.bump_impulse,
            props.bump_impulse * 0.5,
        );
        events.emit("circle_bumped_2d", #{ other: other.name });
    }
}
