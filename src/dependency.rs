use crate::manifest::Manifest;
use crate::package::Package;
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub struct Graph {
  pub dependencies: Vec<BTreeSet<usize>>,
}

impl Graph {
  pub fn new(packages: &[Package]) -> Self {
    let mut names = BTreeMap::new();
    for (idx, package) in packages.iter().enumerate() {
      for m in &package.manifests {
        names.insert((m.registry(), m.name()), idx);
      }
    }
    let dependencies = packages
      .iter()
      .enumerate()
      .map(|(idx, p)| {
        p.manifests
          .iter()
          .flat_map(|m| m.dependencies())
          .filter_map(|d| names.get(&(d.registry, d.name.as_str())).copied())
          .filter(|dep| *dep != idx)
          .collect()
      })
      .collect();
    Self { dependencies }
  }
  pub fn affected(&self, seeds: &BTreeSet<usize>) -> BTreeSet<usize> {
    let mut affected = seeds.clone();
    loop {
      let previous = affected.len();
      for (idx, deps) in self.dependencies.iter().enumerate() {
        if deps.iter().any(|d| affected.contains(d)) {
          affected.insert(idx);
        }
      }
      if previous == affected.len() {
        return affected;
      }
    }
  }

  pub fn upstream(&self, index: usize) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    let mut queue = vec![index];
    while let Some(current) = queue.pop() {
      for dependency in &self.dependencies[current] {
        if *dependency != index && found.insert(*dependency) {
          queue.push(*dependency);
        }
      }
    }
    found
  }
  pub fn order(&self, subset: &BTreeSet<usize>, packages: &[Package]) -> Result<Vec<usize>> {
    fn visit(
      idx: usize,
      graph: &Graph,
      subset: &BTreeSet<usize>,
      packages: &[Package],
      states: &mut [u8],
      out: &mut Vec<usize>,
    ) -> Result<()> {
      ensure!(
        states[idx] != 1,
        "cyclic dependency involving {}",
        packages[idx].name
      );
      if states[idx] == 2 {
        return Ok(());
      }
      states[idx] = 1;
      for dep in &graph.dependencies[idx] {
        if subset.contains(dep) {
          visit(*dep, graph, subset, packages, states, out)?;
        }
      }
      states[idx] = 2;
      out.push(idx);
      Ok(())
    }
    let mut states = vec![0; self.dependencies.len()];
    let mut order = vec![];
    for idx in subset {
      visit(*idx, self, subset, packages, &mut states, &mut order)?;
    }
    Ok(order)
  }
}
