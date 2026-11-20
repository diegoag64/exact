//! The plan's child sites, in the order the runner mounts them.
use exact_plan::{BindingKind, EventKind, Plan, RegionKind};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub enum Site {
    Node(u32),
    Region(u32),
}

/// The plan's child sites, grouped as the runner groups them.
pub struct Sites {
    pub root: u32,
    by_node: Vec<Vec<Site>>,
    by_arm: Vec<Vec<Site>>,
}

impl Sites {
    pub fn new(plan: &Plan) -> Result<Sites, String> {
        let mut by_node: Vec<Vec<(u32, u8, u32, Site)>> = vec![Vec::new(); plan.nodes.len()];
        let mut by_arm: Vec<Vec<(u32, u8, u32, Site)>> = vec![Vec::new(); plan.arms.len()];
        let mut root = None;
        for (i, n) in plan.nodes.iter().enumerate() {
            let e = (n.order, 0, i as u32, Site::Node(i as u32));
            // A site's group is (parent, arm): an arm's roots have no parent;
            // a deeper site names its parent node and its enclosing arm.
            match (n.parent, n.arm) {
                (Some(p), _) => by_node[p.0 as usize].push(e),
                (None, Some(a)) => by_arm[a.0 as usize].push(e),
                (None, None) => {
                    if root.replace(i as u32).is_some() {
                        return Err("more than one root".into());
                    }
                }
            }
        }
        for (i, r) in plan.regions.iter().enumerate() {
            let e = (r.order, 1, i as u32, Site::Region(i as u32));
            match (r.parent, r.arm) {
                (Some(p), _) => by_node[p.0 as usize].push(e),
                (None, Some(a)) => by_arm[a.0 as usize].push(e),
                (None, None) => return Err("a region at the root".into()),
            }
        }
        let sort = |v: Vec<Vec<(u32, u8, u32, Site)>>| {
            v.into_iter()
                .map(|mut l| {
                    l.sort_by_key(|e| (e.0, e.1, e.2));
                    l.into_iter().map(|e| e.3).collect()
                })
                .collect()
        };
        Ok(Sites {
            root: root.ok_or("no root")?,
            by_node: sort(by_node),
            by_arm: sort(by_arm),
        })
    }
    pub fn of_node(&self, n: u32) -> &[Site] {
        &self.by_node[n as usize]
    }
    pub fn of_arm(&self, a: u32) -> &[Site] {
        &self.by_arm[a as usize]
    }
}

/// Count references to a subtree across distinct parent shapes. Two copies of
/// one component need a mount for its root. A child used only there is
/// already shared by that mount. The emitter checks the generated bodies
/// before sharing them, including bindings and closed-over variables.
pub(super) fn repeated_nodes(plan: &Plan, sites: &Sites) -> Vec<bool> {
    #[derive(Hash, Eq, PartialEq)]
    enum Kind {
        Node(u8, Vec<(BindingKind, u16)>, Vec<EventKind>),
        Region(RegionKind, Vec<usize>),
    }
    #[derive(Default)]
    struct Shapes {
        index: HashMap<(Kind, Vec<usize>), usize>,
        uses: Vec<usize>,
        nodes: Vec<(u32, usize)>,
    }
    impl Shapes {
        fn visit(&mut self, plan: &Plan, sites: &Sites, site: Site) -> usize {
            let mut children = Vec::new();
            let kind = match site {
                Site::Node(i) => {
                    let n = plan.node(exact_plan::NodesId(i));
                    for &child in sites.of_node(i) {
                        children.push(self.visit(plan, sites, child));
                    }
                    Kind::Node(
                        n.node_type,
                        n.bindings
                            .iter()
                            .map(|b| {
                                let b = plan.binding(b);
                                (b.kind, b.id)
                            })
                            .collect(),
                        n.handlers.iter().map(|h| plan.handler(h).event).collect(),
                    )
                }
                Site::Region(i) => {
                    let r = plan.region(exact_plan::RegionsId(i));
                    let mut ends = Vec::new();
                    for arm in r.arms.iter() {
                        for &child in sites.of_arm(arm.0) {
                            children.push(self.visit(plan, sites, child));
                        }
                        ends.push(children.len());
                    }
                    Kind::Region(r.kind, ends)
                }
            };
            let n = self.index.len();
            let id = *self
                .index
                .entry((kind, children))
                .or_insert_with_key(|(_, children)| {
                    for &child in children {
                        self.uses[child] += 1;
                    }
                    self.uses.push(0);
                    n
                });
            if let Site::Node(i) = site {
                self.nodes.push((i, id));
            }
            id
        }
    }
    let mut shapes = Shapes::default();
    shapes.visit(plan, sites, Site::Node(sites.root));
    let mut repeated = vec![false; plan.nodes.len()];
    for (node, shape) in shapes.nodes {
        repeated[node as usize] = shapes.uses[shape] > 1;
    }
    repeated
}

/// Mark each arm a resource or task owner crosses when it mounts. A site
/// with a parent mounts under that node, even in a hand-built plan.
pub(super) fn instance_paths(plan: &Plan) -> Vec<bool> {
    let mut paths = vec![false; plan.arms.len()];
    let mut nodes = std::collections::HashSet::new();
    let owners = plan
        .resources
        .iter()
        .filter_map(|r| r.owner)
        .chain(plan.timers.iter().filter_map(|t| t.owner));
    for owner in owners {
        let mut arm = Some(owner);
        while let Some(id) = arm {
            if std::mem::replace(&mut paths[id.0 as usize], true) {
                break;
            }
            let region = plan.region(plan.arm(id).region);
            let mut parent = region.parent;
            arm = region.arm;
            while let Some(id) = parent {
                if !nodes.insert(id) {
                    arm = None;
                    break;
                }
                let node = plan.node(id);
                (parent, arm) = (node.parent, node.arm);
            }
        }
    }
    paths
}
