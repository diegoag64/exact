//! What the reload tests share: a source with no data, a plan of one view,
//! a boot and the running plan's digest.

use exact_apple::abi::{Bridge, Hooks};

pub struct NoData;

impl exact_runner::DataSource for NoData {
    fn query(
        &mut self,
        name: &str,
        _: &[exact_plan::Value],
    ) -> Result<exact_plan::Value, exact_runner::DataError> {
        Err(exact_runner::DataError::UnknownSource(name.into()))
    }
}

pub fn plan(view: &str) -> Vec<u8> {
    source(&format!("component A\n  view\n    {view}\n"))
}

pub fn source(text: &str) -> Vec<u8> {
    contract::compile(text).unwrap().encode()
}

/// A plan that can start a Live Activity, which only the activities family
/// serves; it reads nothing at boot, so its candidate is not held for data.
pub const ACTIVITIES: &str = "shape ID\n  id: string\nshape S\n  n: number\ncomponent A\n  mutation started as shape ID\n  action start\n    send started = startActivity(\"k\", S(n=1))\n  view\n    text \"a\"\n";

pub fn boot(bridge: &mut Bridge<NoData>, plan: &[u8]) {
    let out = bridge.boot(plan, NoData, Hooks::none(), 390.0, 844.0);
    let reply = String::from_utf8_lossy(bridge.output_bytes(out as usize)).into_owned();
    assert!(!reply.contains("\"error\":\""), "{reply}");
}

pub fn digest(bridge: &mut Bridge<NoData>) -> String {
    let out = bridge.plan_digest();
    String::from_utf8_lossy(bridge.output_bytes(out as usize)).into_owned()
}
