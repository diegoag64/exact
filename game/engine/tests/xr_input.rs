//! A headset's controllers as a canvas's input (`docs/quest.md`,
//! "Controllers"): the whole state each frame, the latest standing until the
//! next, the buttons' edges reaching one tick each, the state a world
//! resource that saves and restores, and a ray taken into the world by the
//! stage.
use exact_game::*;

/// What each tick read: the right trigger's pressed and released edges and
/// whether it was held, and where the right hand aimed.
#[derive(Default, Resource)]
struct Seen {
    pressed: u32,
    released: u32,
    held: Vec<bool>,
    aim: Vec3,
}
struct Hands;
impl Game for Hands {
    const ID: &'static str = "input-xr";
    type Args = ();
    fn actions() -> Actions {
        Actions::new()
    }
    fn setup(w: &mut World, _: &()) {
        w.insert_resource(Seen::default());
    }
    fn tick(w: &mut World, input: &Input, _: &()) {
        let right = *input.xr().hand(XrHand::Right);
        let mut seen = w.resource_mut::<Seen>();
        seen.pressed += u32::from(input.xr_pressed(XrHand::Right, XrButton::Trigger));
        seen.released += u32::from(input.xr_released(XrHand::Right, XrButton::Trigger));
        seen.held.push(right.down(XrButton::Trigger));
        seen.aim = right.aim.forward();
    }
}

fn hands(trigger: bool, x: f32) -> XrInput {
    let mut input = XrInput::default();
    let right = input.hand_mut(XrHand::Right);
    right.active = true;
    right.tracked = true;
    right.aim.position = Vec3::new(x, 1.2, -0.2);
    right.aim.rotation = Quat::from_rotation_x(-0.5);
    right.grip = right.aim;
    right.trigger = f32::from(u8::from(trigger));
    right.buttons = if trigger { XrButton::Trigger.bit() } else { 0 };
    input
}
fn event(at_ms: f64, input: XrInput) -> InputEvent {
    InputEvent::Xr {
        input,
        at_ms,
        player: 0,
    }
}
fn started() -> Sim<Hands> {
    let mut sim = Sim::<Hands>::new(()).unwrap();
    sim.advance(0., Clock::Seekable);
    sim
}

#[test]
fn a_press_and_a_release_each_reach_one_tick() {
    let mut sim = started();
    assert!(sim.world().try_resource::<XrInput>().is_none(), "no headset, no resource");
    // Frames at 72 Hz under a 60 Hz world: held for five frames.
    let frame = 1000.0 / 72.0;
    for n in 0..12u32 {
        let at = f64::from(n) * frame;
        sim.input(event(at, hands((3..8).contains(&n), 0.01 * n as f32)));
        sim.advance(at + frame, Clock::Seekable);
    }
    let seen = sim.world().resource::<Seen>();
    assert_eq!((seen.pressed, seen.released), (1, 1));
    let held = seen.held.iter().filter(|h| **h).count();
    assert!((3..=5).contains(&held), "{held}");
    // The latest stands, in the world too.
    assert!((seen.aim - Quat::from_rotation_x(-0.5) * Vec3::NEG_Z).length() < 1e-6);
    let stored = *sim.world().resource::<XrInput>();
    assert!((stored.hand(XrHand::Right).aim.position.x - 0.11).abs() < 1e-6);
    assert!(!stored.hand(XrHand::Left).active);
}

#[test]
fn a_frame_that_only_moved_replaces_the_one_before_it() {
    let mut sim = started();
    // Three frames inside one tick: a move, a press, a move while held.
    sim.input(event(1.0, hands(false, 0.1)));
    sim.input(event(2.0, hands(false, 0.2)));
    sim.input(event(3.0, hands(true, 0.3)));
    sim.input(event(4.0, hands(true, 0.4)));
    sim.advance(1000.0 / 60.0 + 5.0, Clock::Seekable);
    let seen = sim.world().resource::<Seen>();
    assert_eq!((seen.pressed, seen.released), (1, 0));
    let x = sim.world().resource::<XrInput>().hand(XrHand::Right).aim.position.x;
    assert!((x - 0.4).abs() < 1e-6);
}

#[test]
fn the_hands_save_and_restore_and_refuse_what_is_not_finite() {
    let mut sim = started();
    sim.xr(hands(true, 0.25));
    sim.run(50.0);
    let saved = sim.save().unwrap();
    let mut other = started();
    other.restore(&saved).unwrap();
    assert_eq!(other.world().hash(), sim.world().hash());
    // The restored world holds the trigger: releasing it is an edge.
    other.xr(hands(false, 0.25));
    other.run(50.0);
    assert_eq!(other.world().resource::<Seen>().released, 1);
    let mut bad = hands(false, 0.0);
    bad.hand_mut(XrHand::Left).aim.position.y = f32::NAN;
    let before = sim.world().resource::<XrInput>().hand(XrHand::Right).aim.position;
    sim.input(event(0.0, bad));
    sim.run(50.0);
    assert_eq!(sim.world().resource::<XrInput>().hand(XrHand::Right).aim.position, before);
    let mut odd = hands(false, 0.0);
    odd.hand_mut(XrHand::Right).buttons = 1 << 9;
    assert!(sim.validate_input(&event(0.0, odd)).is_err());
}

#[test]
fn the_stage_takes_an_aim_into_the_world() {
    // A board 24 units across 1.2 m, on a table in front of the player.
    let stage = XrStage::tabletop(Vec3::new(2.0, 0.0, 1.0), 24.0, 1.2, 0.85, 0.9);
    let hand = XrPose {
        position: Vec3::new(0.2, 1.25, -0.5),
        rotation: Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
    };
    let (origin, direction) = hand.ray();
    assert!((direction - Vec3::NEG_Y).length() < 1e-6);
    let (o, d) = stage.ray_to_world(origin, direction);
    let hit = o + d * (o.y / -d.y);
    // 0.2 m right of and 0.4 m nearer than the table's centre, at 20 units a metre.
    assert!((hit - Vec3::new(2.0 + 4.0, 0.0, 1.0 + 8.0)).length() < 1e-3, "{hit}");
}
