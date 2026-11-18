//! LLP 1047.001 D5 on a reload: an archive whose linked set is hello's (no
//! family) refuses a plan that uses a family before anything changes, and the
//! running plan stays. This proves admission, not what the archive omits: the
//! test binary carries every family. Its own process, since an archive's linked set is the
//! process's (`exact_apple::link::set`); `reload_admitted.rs` is the
//! all-linked case.

use exact_apple::abi::{Bridge, Hooks};
use exact_apple::link::{self, Uses};

#[path = "reload/common.rs"]
mod common;
use common::{boot, digest, plan, source, NoData, ACTIVITIES};

#[test]
fn hellos_binary_refuses_a_reload_into_a_family_and_keeps_its_plan() {
    link::set(Uses::NONE);
    let mut bridge = Bridge::<NoData>::new();
    boot(&mut bridge, &plan("text \"Hello world\""));
    let before = digest(&mut bridge);
    for (next, names) in [
        (plan("text \"a\" filter=\"blur(2px)\""), "effects"),
        (
            plan("svg viewBox=\"0 0 10 10\"\n      circle cx=5 cy=5 r=4"),
            "effects, svg",
        ),
        // An SVG `data:` image, which only its bytes say is one.
        (
            plan("image \"data:image/svg+xml,%3Csvg%2F%3E\""),
            "effects, svg, images",
        ),
        (plan("view hatch=\"avatar\""), "native_views"),
        (plan("input type=\"checkbox\""), "controls"),
        (
            plan("row role=\"tablist\"\n      button \"a\" role=\"tab\""),
            "controls",
        ),
        (
            plan("column sharedElement=\"hero\"\n      text \"a\""),
            "transitions, flights",
        ),
        (
            plan("column position=\"sticky\" top=0\n      text \"a\""),
            "sticky",
        ),
        (source(ACTIVITIES), "activities"),
        (
            plan("column background-image=\"linear-gradient(red, blue)\""),
            "gradients",
        ),
    ] {
        let len = bridge.input_write(&next);
        let out = bridge.prepare_plan(len, NoData, Hooks::none(), 390.0, 844.0);
        let reply = String::from_utf8_lossy(bridge.output_bytes(out as usize)).into_owned();
        assert!(
            reply.contains(&format!("Unlinked(\\\"{names}\\\")")),
            "{names}: {reply}"
        );
        let out = bridge.commit_plan();
        let reply = String::from_utf8_lossy(bridge.output_bytes(out as usize)).into_owned();
        assert!(
            reply.contains("error"),
            "a refused candidate is not committed: {reply}"
        );
        assert_eq!(digest(&mut bridge), before, "{names}");
    }
}
