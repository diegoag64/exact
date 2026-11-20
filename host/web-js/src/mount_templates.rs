//! DOM mount functions preserve the lexical bindings of their region or parent node.
use super::Em;
use crate::code::{self, Scope};
use std::fmt::Write as _;

impl Em<'_> {
    pub(super) fn mount_node(
        &mut self,
        node: u32,
        parent: &str,
        scope: &Scope,
    ) -> Result<(), String> {
        if !self
            .repeated_nodes
            .get(node as usize)
            .copied()
            .unwrap_or(false)
        {
            return self.node(node, parent, scope);
        }
        let saved = std::mem::take(&mut self.out);
        let outer = self.next_local_element.replace(0);
        let scope = Scope {
            capture: true,
            ..scope.clone()
        };
        self.node(node, "p", &scope)?;
        self.next_local_element = outer;
        let body = std::mem::replace(&mut self.out, saved);
        let mount = self.mount_templates.mount("p", body, Some(parent), None);
        self.out.push_str(&mount);
        Ok(())
    }

    pub(super) fn site_number(&self, n: u32) -> String {
        if self.next_local_element.is_some() {
            format!("\0s{n}\0")
        } else {
            n.to_string()
        }
    }

    pub(super) fn site_evaluation(
        &self,
        scope: &Scope,
        n: u32,
        what: String,
        instance: bool,
    ) -> Scope {
        if self.next_local_element.is_none() {
            return code::evaluation(scope, format!("site {n} {what}"), instance);
        }
        let suffix = serde_json::to_string(&format!(" {what}")).unwrap();
        let label = format!(r#""site "+({})+{suffix}"#, self.site_number(n));
        Scope {
            evaluation: Some((label, instance)),
            ..scope.clone()
        }
    }

    /// Descendant arms may close over bindings introduced here, including a
    /// keyed row's getters or the DOM element that owns a navigation status.
    pub(super) fn template_scope(
        &mut self,
        new_scope: bool,
        render: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        if !new_scope {
            return render(self);
        }
        let outer = std::mem::take(&mut self.mount_templates);
        self.mount_templates.depth = outer.depth + 1;
        let saved = std::mem::take(&mut self.out);
        render(self)?;
        let built = std::mem::replace(&mut self.out, saved);
        let templates = std::mem::replace(&mut self.mount_templates, outer);
        let (declarations, view) = templates.render(built, self.next_local_element.is_some(), "$v");
        if declarations.is_empty() {
            self.out.push_str(&view);
        } else {
            write!(self.out, "{{{declarations}{view}}}").unwrap();
        }
        Ok(())
    }
}
