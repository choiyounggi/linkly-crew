use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dod::DodCheck;
use crate::spec::ArtifactContract;

/// Agent role assigned to a task — DESIGN.md §4.2 role table (기획자
/// PM/디자이너/퍼블리셔/개발자/QA). Lead is not a variant here: DESIGN.md
/// §4.3 "Lead는 코드를 만지지 않는다... 오직 계획·라우팅·수락 판정·머지" —
/// Lead assigns tasks, it never receives one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Pm,
    Designer,
    Publisher,
    Developer,
    Qa,
}

/// One task in the DAG — DESIGN.md §8 `tasks` schema
/// (`role, title, brief, dod_json, deps_json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub role: Role,
    pub title: String,
    pub brief: String,
    pub dod: Vec<DodCheck>,
    pub deps: Vec<String>,
    pub artifacts_expected: Vec<ArtifactContract>,
}

/// DAG validation failures — DESIGN.md §9 "DAG에 사이클 생성 (Lead가 잘못
/// 계획) → 생성 시점에 위상정렬 검증".
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DagError {
    #[error("duplicate task id: {0:?}")]
    DuplicateTaskId(String),
    #[error("task {task:?} depends on unknown task {dep:?}")]
    UnknownDep { task: String, dep: String },
    #[error("cycle detected among task ids: {0:?}")]
    CycleDetected(Vec<String>),
}

/// Lead's task DAG — DESIGN.md §4.1② "Lead: 태스크 DAG 생성 — 역할·의존성·
/// 산출물 계약·DoD".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDag {
    pub tasks: Vec<TaskSpec>,
}

impl TaskDag {
    /// Validates task ids are unique and every `deps` entry resolves to a
    /// task in this DAG, then topologically sorts (Kahn's algorithm),
    /// returning the sorted task ids. An incomplete sort means a cycle.
    pub fn validate(&self) -> Result<Vec<String>, DagError> {
        let mut seen = HashSet::new();
        for task in &self.tasks {
            if !seen.insert(task.id.as_str()) {
                return Err(DagError::DuplicateTaskId(task.id.clone()));
            }
        }

        let ids: HashSet<&str> = self.tasks.iter().map(|t| t.id.as_str()).collect();
        for task in &self.tasks {
            for dep in &task.deps {
                if !ids.contains(dep.as_str()) {
                    return Err(DagError::UnknownDep {
                        task: task.id.clone(),
                        dep: dep.clone(),
                    });
                }
            }
        }

        let mut in_degree: HashMap<&str, usize> =
            self.tasks.iter().map(|t| (t.id.as_str(), 0)).collect();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
        for task in &self.tasks {
            for dep in &task.deps {
                *in_degree.get_mut(task.id.as_str()).unwrap() += 1;
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(task.id.as_str());
            }
        }

        let mut queue: VecDeque<&str> = self
            .tasks
            .iter()
            .map(|t| t.id.as_str())
            .filter(|id| in_degree[id] == 0)
            .collect();

        let mut order: Vec<String> = Vec::with_capacity(self.tasks.len());
        while let Some(id) = queue.pop_front() {
            order.push(id.to_string());
            if let Some(next_ids) = dependents.get(id) {
                for &next in next_ids {
                    let degree = in_degree.get_mut(next).unwrap();
                    *degree -= 1;
                    if *degree == 0 {
                        queue.push_back(next);
                    }
                }
            }
        }

        if order.len() != self.tasks.len() {
            let sorted: HashSet<&str> = order.iter().map(String::as_str).collect();
            let remaining: Vec<String> = self
                .tasks
                .iter()
                .map(|t| t.id.clone())
                .filter(|id| !sorted.contains(id.as_str()))
                .collect();
            return Err(DagError::CycleDetected(remaining));
        }

        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, deps: &[&str]) -> TaskSpec {
        TaskSpec {
            id: id.to_string(),
            role: Role::Developer,
            title: format!("task {id}"),
            brief: "brief".to_string(),
            dod: vec![],
            deps: deps.iter().map(|d| d.to_string()).collect(),
            artifacts_expected: vec![],
        }
    }

    #[test]
    fn role_serializes_to_snake_case() {
        let json = serde_json::to_string(&Role::Pm).unwrap();
        assert_eq!(json, "\"pm\"");
        let back: Role = serde_json::from_str("\"qa\"").unwrap();
        assert_eq!(back, Role::Qa);
    }

    #[test]
    fn validate_returns_topological_order_for_a_normal_chain() {
        let dag = TaskDag {
            tasks: vec![task("A", &[]), task("B", &["A"]), task("C", &["B"])],
        };
        assert_eq!(
            dag.validate(),
            Ok(vec!["A".to_string(), "B".to_string(), "C".to_string()])
        );
    }

    #[test]
    fn validate_rejects_a_cycle() {
        let dag = TaskDag {
            tasks: vec![task("A", &["B"]), task("B", &["A"])],
        };
        match dag.validate() {
            Err(DagError::CycleDetected(mut ids)) => {
                ids.sort();
                assert_eq!(ids, vec!["A".to_string(), "B".to_string()]);
            }
            other => panic!("expected CycleDetected, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_duplicate_task_id() {
        let dag = TaskDag {
            tasks: vec![task("A", &[]), task("A", &[])],
        };
        assert_eq!(
            dag.validate(),
            Err(DagError::DuplicateTaskId("A".to_string()))
        );
    }

    #[test]
    fn validate_rejects_unknown_dependency() {
        let dag = TaskDag {
            tasks: vec![task("A", &["ghost"])],
        };
        assert_eq!(
            dag.validate(),
            Err(DagError::UnknownDep {
                task: "A".to_string(),
                dep: "ghost".to_string(),
            })
        );
    }

    #[test]
    fn validate_accepts_empty_dag_as_boundary() {
        let dag = TaskDag { tasks: vec![] };
        assert_eq!(dag.validate(), Ok(vec![]));
    }

    #[test]
    fn task_spec_round_trips_through_serde() {
        let t = task("A", &["B"]);
        let json = serde_json::to_string(&t).unwrap();
        let back: TaskSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}
