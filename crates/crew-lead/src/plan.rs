//! Lead's plan layer — DESIGN.md §4.1 ①~③: request → [`SpecDoc`] → 5-role
//! [`TaskDag`] → sprint slices. `LeadPlanner::specify` here is a **M3
//! deterministic template**; DESIGN.md §4.3's LLM-driven, structured-JSON
//! specification replaces it in M4+ under the same signature.

use crew_proto::{
    ArtifactContract, DagError, DodCheck, ReqId, Requirement, Role, SpecDoc, TaskDag, TaskSpec,
};
use thiserror::Error;

/// Errors from the plan layer.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PlanError {
    #[error("request must not be empty")]
    EmptyRequest,
    #[error("max_per_sprint must be greater than zero")]
    InvalidSprintSize,
    #[error(transparent)]
    Dag(#[from] DagError),
}

/// Turns a one-line request into a [`SpecDoc`] and a 5-role [`TaskDag`].
pub struct LeadPlanner;

impl LeadPlanner {
    /// M3 deterministic specification: any non-empty request maps to the
    /// same fixed REQ-1..5 template (DESIGN.md §4.3 M4+ replaces this with
    /// LLM-driven specification under this same signature).
    pub fn specify(request: &str) -> Result<SpecDoc, PlanError> {
        let goal = request.trim();
        if goal.is_empty() {
            return Err(PlanError::EmptyRequest);
        }

        Ok(SpecDoc {
            goal: goal.to_string(),
            non_goals: vec!["백엔드 API".to_string(), "인증/결제".to_string()],
            constraints: vec![
                "정적 웹, 단일 페이지".to_string(),
                "외부 네트워크 자원 없이 렌더 가능".to_string(),
            ],
            requirements: vec![
                requirement("REQ-1", "히어로 섹션(제목+한줄 소개)"),
                requirement("REQ-2", "핵심 기능 소개 3개 카드"),
                requirement("REQ-3", "CTA 버튼 1개"),
                requirement("REQ-4", "반응형 레이아웃(모바일 1열)"),
                requirement("REQ-5", "푸터(저작권 표기)"),
            ],
            acceptance: vec![
                "모든 REQ id가 산출물에 커버".to_string(),
                "QA 보고서 REQ별 pass".to_string(),
            ],
        })
    }

    /// Builds the 5-role linear chain t-pm → t-design → t-publish → t-dev →
    /// t-qa (DESIGN.md §4.2 role table), each requiring coverage of every
    /// requirement in `spec` and expecting exactly the artifact contracted
    /// for its role, then self-validates via [`TaskDag::validate`].
    pub fn plan_dag(spec: &SpecDoc) -> Result<TaskDag, PlanError> {
        let req_ids: Vec<ReqId> = spec.requirements.iter().map(|r| r.id.clone()).collect();
        let dod = vec![DodCheck::ReqCover {
            ids: req_ids.clone(),
        }];

        let tasks = vec![
            role_task(
                "t-pm",
                Role::Pm,
                "PM 스펙 정리",
                &spec.goal,
                &dod,
                vec![],
                "spec.md",
                "doc",
                &req_ids,
            ),
            role_task(
                "t-design",
                Role::Designer,
                "디자인",
                &spec.goal,
                &dod,
                vec!["t-pm".to_string()],
                "design.md",
                "doc",
                &req_ids,
            ),
            role_task(
                "t-publish",
                Role::Publisher,
                "퍼블리싱",
                &spec.goal,
                &dod,
                vec!["t-design".to_string()],
                "index.html",
                "markup",
                &req_ids,
            ),
            role_task(
                "t-dev",
                Role::Developer,
                "개발",
                &spec.goal,
                &dod,
                vec!["t-publish".to_string()],
                "app.js",
                "code",
                &req_ids,
            ),
            role_task(
                "t-qa",
                Role::Qa,
                "QA",
                &spec.goal,
                &dod,
                vec!["t-dev".to_string()],
                "qa-report.md",
                "report",
                &req_ids,
            ),
        ];

        let dag = TaskDag { tasks };
        dag.validate()?;
        Ok(dag)
    }
}

fn requirement(id: &str, text: &str) -> Requirement {
    Requirement {
        id: ReqId::new(id).expect("REQ- 접두 상수"),
        text: text.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn role_task(
    id: &str,
    role: Role,
    duty: &str,
    goal: &str,
    dod: &[DodCheck],
    deps: Vec<String>,
    artifact_name: &str,
    artifact_kind: &str,
    req_ids: &[ReqId],
) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        role,
        title: format!("{goal} — {duty}"),
        brief: format!("{goal} 요청에 대해 {duty}을(를) 수행한다"),
        dod: dod.to_vec(),
        deps,
        artifacts_expected: vec![ArtifactContract {
            name: artifact_name.to_string(),
            kind: artifact_kind.to_string(),
            req_ids: req_ids.to_vec(),
        }],
    }
}

/// Slices a validated task DAG's topological order into sprint-sized chunks.
pub struct SprintSlicer;

impl SprintSlicer {
    /// Chunks `dag.validate()`'s topo order into groups of at most
    /// `max_per_sprint` task ids. Because a dependency always precedes its
    /// dependents in a topological order, every chunk boundary can only ever
    /// separate a dependency into an *earlier* chunk than its dependent —
    /// never a later one — so dependency order across sprints is preserved
    /// structurally.
    pub fn slice(dag: &TaskDag, max_per_sprint: usize) -> Result<Vec<Vec<String>>, PlanError> {
        if max_per_sprint == 0 {
            return Err(PlanError::InvalidSprintSize);
        }
        let order = dag.validate()?;
        Ok(order.chunks(max_per_sprint).map(|c| c.to_vec()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specify_returns_fixed_req_template_for_a_normal_request() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        assert_eq!(spec.goal, "간단한 랜딩 페이지");
        let ids: Vec<&str> = spec.requirements.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["REQ-1", "REQ-2", "REQ-3", "REQ-4", "REQ-5"]);
    }

    #[test]
    fn specify_rejects_whitespace_only_request() {
        assert_eq!(LeadPlanner::specify("  "), Err(PlanError::EmptyRequest));
    }

    #[test]
    fn plan_dag_builds_a_validated_linear_five_role_chain() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag(&spec).unwrap();

        let order = dag.validate().unwrap();
        assert_eq!(order, vec!["t-pm", "t-design", "t-publish", "t-dev", "t-qa"]);

        let roles: Vec<Role> = dag.tasks.iter().map(|t| t.role).collect();
        assert_eq!(
            roles,
            vec![
                Role::Pm,
                Role::Designer,
                Role::Publisher,
                Role::Developer,
                Role::Qa
            ]
        );

        let all_req_ids: Vec<ReqId> = spec.requirements.iter().map(|r| r.id.clone()).collect();
        let expected_artifacts = [
            ("spec.md", "doc"),
            ("design.md", "doc"),
            ("index.html", "markup"),
            ("app.js", "code"),
            ("qa-report.md", "report"),
        ];
        for (task, (name, kind)) in dag.tasks.iter().zip(expected_artifacts) {
            assert_eq!(task.dod, vec![DodCheck::ReqCover { ids: all_req_ids.clone() }]);
            assert_eq!(task.artifacts_expected.len(), 1);
            assert_eq!(task.artifacts_expected[0].name, name);
            assert_eq!(task.artifacts_expected[0].kind, kind);
            assert_eq!(task.artifacts_expected[0].req_ids, all_req_ids);
        }
    }

    #[test]
    fn slice_with_room_for_all_tasks_returns_one_sprint() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag(&spec).unwrap();

        let sprints = SprintSlicer::slice(&dag, 7).unwrap();
        assert_eq!(
            sprints,
            vec![vec![
                "t-pm".to_string(),
                "t-design".to_string(),
                "t-publish".to_string(),
                "t-dev".to_string(),
                "t-qa".to_string(),
            ]]
        );
    }

    #[test]
    fn slice_chunks_preserve_dependency_order_across_sprints() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag(&spec).unwrap();

        let sprints = SprintSlicer::slice(&dag, 2).unwrap();
        assert_eq!(sprints.len(), 3);
        assert_eq!(sprints.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 2, 1]);

        let sprint_of: std::collections::HashMap<&str, usize> = sprints
            .iter()
            .enumerate()
            .flat_map(|(i, s)| s.iter().map(move |id| (id.as_str(), i)))
            .collect();
        for task in &dag.tasks {
            for dep in &task.deps {
                assert!(sprint_of[dep.as_str()] <= sprint_of[task.id.as_str()]);
            }
        }
    }

    #[test]
    fn slice_rejects_zero_max_per_sprint() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag(&spec).unwrap();
        assert_eq!(
            SprintSlicer::slice(&dag, 0),
            Err(PlanError::InvalidSprintSize)
        );
    }

    #[test]
    fn slice_accepts_empty_dag_as_boundary() {
        let dag = TaskDag { tasks: vec![] };
        assert_eq!(SprintSlicer::slice(&dag, 3), Ok(vec![]));
    }

    #[test]
    fn plan_dag_task_spec_round_trips_through_serde() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag(&spec).unwrap();
        let task = &dag.tasks[0];

        let json = serde_json::to_string(task).unwrap();
        let back: TaskSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, *task);
    }
}
