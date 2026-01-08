use crate::api::Formula;
use crate::error::{Result, WaxError};
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::{debug, instrument};

#[derive(Debug, Clone)]
pub struct DependencyGraph {
    nodes: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, name: String, deps: Vec<String>) {
        self.nodes.insert(name, deps);
    }

    #[instrument(skip(self))]
    pub fn topological_sort(&self) -> Result<Vec<String>> {
        debug!("Performing topological sort on dependency graph");

        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj_list: HashMap<String, Vec<String>> = HashMap::new();

        for (node, deps) in &self.nodes {
            in_degree.entry(node.clone()).or_insert(0);

            for dep in deps {
                in_degree.entry(dep.clone()).or_insert(0);
                adj_list.entry(dep.clone()).or_default().push(node.clone());
            }
        }

        for (node, deps) in &self.nodes {
            let count = deps.len();
            *in_degree.entry(node.clone()).or_insert(0) = count;
        }

        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, &count)| count == 0)
            .map(|(node, _)| node.clone())
            .collect();

        let mut result = Vec::new();

        while let Some(node) = queue.pop_front() {
            result.push(node.clone());

            if let Some(neighbors) = adj_list.get(&node) {
                for neighbor in neighbors {
                    if let Some(count) = in_degree.get_mut(neighbor) {
                        *count -= 1;
                        if *count == 0 {
                            queue.push_back(neighbor.clone());
                        }
                    }
                }
            }
        }

        if result.len() != in_degree.len() {
            return Err(WaxError::DependencyCycle(
                "Circular dependency detected".to_string(),
            ));
        }

        debug!("Topological sort result: {:?}", result);
        Ok(result)
    }
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[instrument(skip(formulae))]
pub fn resolve_dependencies(
    formula: &Formula,
    formulae: &[Formula],
    installed: &HashSet<String>,
) -> Result<Vec<String>> {
    debug!("Resolving dependencies for {}", formula.name);

    let mut graph = DependencyGraph::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    queue.push_back(formula.name.clone());

    while let Some(name) = queue.pop_front() {
        if visited.contains(&name) || installed.contains(&name) {
            continue;
        }
        visited.insert(name.clone());

        let f = formulae
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| WaxError::FormulaNotFound(name.clone()))?;

        let deps = f.dependencies.clone().unwrap_or_default();

        graph.add_node(name.clone(), deps.clone());

        for dep in deps {
            if !installed.contains(&dep) {
                queue.push_back(dep);
            }
        }
    }

    let sorted = graph.topological_sort()?;

    let to_install: Vec<String> = sorted
        .into_iter()
        .filter(|name| !installed.contains(name))
        .collect();

    debug!("Packages to install: {:?}", to_install);
    Ok(to_install)
}
