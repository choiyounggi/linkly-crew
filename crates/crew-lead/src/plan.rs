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
    /// `LlmLeadPlanner::specify` failure (contracts-m5.md §C3c, plan_llm.rs)
    /// — parse/validation failure or harness error. No silent fallback to
    /// the deterministic template.
    #[error("llm specify failed: {0}")]
    LlmSpecify(String),
    /// `plan_dag_for` requires at least one role (contracts-m7.md §E1).
    #[error("roles must not be empty")]
    EmptyRoles,
    /// `plan_dag_for` rejects a role appearing more than once in `roles`
    /// (contracts-m7.md §E1).
    #[error("duplicate role in roles: {0:?}")]
    DuplicateRole(Role),
}

/// A single `cmd` DoD check the planner may attach to a Developer task
/// (contracts-m10.md §H1b). Not `DodCheck` itself — this narrower type keeps
/// the injection knob from ever admitting a check other than `Cmd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdCheck {
    pub run: String,
    pub expect: String,
}

/// Optional behavior for [`LeadPlanner::plan_dag_with`]. `Default` is
/// entirely empty, which reproduces `plan_dag_for`'s historical output
/// (contracts-m10.md §H1b/H1c).
#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Cmd checks to append after `ReqCover` in the Developer task's `dod`.
    pub dev_cmd_checks: Vec<CmdCheck>,
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
    /// t-qa (DESIGN.md §4.2 role table) by delegating to [`Self::plan_dag_for`]
    /// with every role in canonical order — output unchanged from before
    /// `plan_dag_for` existed (contracts-m7.md §E1).
    pub fn plan_dag(spec: &SpecDoc) -> Result<TaskDag, PlanError> {
        Self::plan_dag_for(spec, &ROLE_ORDER)
    }

    /// `plan_dag_for` with `opts` entirely empty (contracts-m7.md §E1) —
    /// delegates to [`Self::plan_dag_with`]. Signature and behavior
    /// unchanged from before `plan_dag_with` existed.
    pub fn plan_dag_for(spec: &SpecDoc, roles: &[Role]) -> Result<TaskDag, PlanError> {
        Self::plan_dag_with(spec, roles, &PlanOptions::default())
    }

    /// Builds a task DAG covering exactly `roles` (a non-empty, duplicate-free
    /// subset of the canonical order `[Pm, Designer, Publisher, Developer,
    /// Qa]`), chained in that canonical order regardless of `roles`' input
    /// order: each task depends on the nearest preceding role that is present
    /// (contracts-m7.md §E1). Per-role id/title/brief/dod/artifacts_expected
    /// are verbatim the same as `plan_dag`'s historical output for that role,
    /// except the Developer task's `dod`, which additionally gets
    /// `opts.dev_cmd_checks` appended after `ReqCover` (contracts-m10.md §H1d).
    /// `opts.dev_cmd_checks` is silently ignored when `roles` has no
    /// `Role::Developer` — not an error (contracts-m10.md §H1d).
    pub fn plan_dag_with(
        spec: &SpecDoc,
        roles: &[Role],
        opts: &PlanOptions,
    ) -> Result<TaskDag, PlanError> {
        if roles.is_empty() {
            return Err(PlanError::EmptyRoles);
        }
        for (i, role) in roles.iter().enumerate() {
            if roles[..i].contains(role) {
                return Err(PlanError::DuplicateRole(*role));
            }
        }

        let req_ids: Vec<ReqId> = spec.requirements.iter().map(|r| r.id.clone()).collect();
        let base_dod = vec![DodCheck::ReqCover {
            ids: req_ids.clone(),
        }];

        let mut tasks = Vec::with_capacity(roles.len());
        let mut prev_id: Option<String> = None;
        for role in ROLE_ORDER.iter().copied().filter(|r| roles.contains(r)) {
            let (id, duty, artifact_name, artifact_kind) = role_meta(role);
            let deps = prev_id.clone().into_iter().collect();
            let dod = if role == Role::Developer {
                let mut dod = base_dod.clone();
                dod.extend(opts.dev_cmd_checks.iter().map(|c| DodCheck::Cmd {
                    run: c.run.clone(),
                    expect: c.expect.clone(),
                }));
                dod
            } else {
                base_dod.clone()
            };
            tasks.push(role_task(
                id,
                role,
                duty,
                &spec.goal,
                &dod,
                deps,
                artifact_name,
                artifact_kind,
                &req_ids,
            ));
            prev_id = Some(id.to_string());
        }

        let dag = TaskDag { tasks };
        dag.validate()?;
        Ok(dag)
    }
}

/// Canonical role order (contracts-m7.md §E1) — the fixed order `plan_dag_for`
/// chains its `roles` subset in, independent of that slice's input order.
const ROLE_ORDER: [Role; 5] = [
    Role::Pm,
    Role::Designer,
    Role::Publisher,
    Role::Developer,
    Role::Qa,
];

/// Per-role task id/duty/artifact contract — the single source `plan_dag`
/// and `plan_dag_for` both build tasks from (contracts-m7.md §E1: "기존
/// plan_dag의 해당 역할 항목과 동일(verbatim 재사용)").
fn role_meta(role: Role) -> (&'static str, &'static str, &'static str, &'static str) {
    match role {
        Role::Pm => ("t-pm", "PM 스펙 정리", "spec.md", "doc"),
        Role::Designer => ("t-design", "디자인", "design.md", "doc"),
        Role::Publisher => ("t-publish", "퍼블리싱", "index.html", "markup"),
        Role::Developer => ("t-dev", "개발", "app.js", "code"),
        Role::Qa => ("t-qa", "QA", "qa-report.md", "report"),
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
    fn plan_dag_for_equals_plan_dag_when_given_all_five_roles_in_canonical_order() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let via_wrapper = LeadPlanner::plan_dag(&spec).unwrap();
        let via_plan_dag_for = LeadPlanner::plan_dag_for(&spec, &ROLE_ORDER).unwrap();
        assert_eq!(via_wrapper, via_plan_dag_for);
    }

    #[test]
    fn plan_dag_for_chains_a_subset_of_roles_in_canonical_order_regardless_of_input_order() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag_for(&spec, &[Role::Qa, Role::Designer]).unwrap();

        assert_eq!(dag.tasks.len(), 2);
        assert_eq!(dag.tasks[0].id, "t-design");
        assert_eq!(dag.tasks[0].role, Role::Designer);
        assert_eq!(dag.tasks[0].deps, Vec::<String>::new());
        assert_eq!(dag.tasks[1].id, "t-qa");
        assert_eq!(dag.tasks[1].role, Role::Qa);
        assert_eq!(dag.tasks[1].deps, vec!["t-design".to_string()]);

        assert_eq!(dag.validate().unwrap(), vec!["t-design", "t-qa"]);
    }

    #[test]
    fn plan_dag_for_builds_a_single_role_with_no_deps() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag_for(&spec, &[Role::Developer]).unwrap();

        assert_eq!(dag.tasks.len(), 1);
        assert_eq!(dag.tasks[0].id, "t-dev");
        assert_eq!(dag.tasks[0].role, Role::Developer);
        assert_eq!(dag.tasks[0].deps, Vec::<String>::new());
    }

    #[test]
    fn plan_dag_for_rejects_empty_roles() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        assert_eq!(
            LeadPlanner::plan_dag_for(&spec, &[]),
            Err(PlanError::EmptyRoles)
        );
    }

    #[test]
    fn plan_dag_for_rejects_duplicate_roles() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        assert_eq!(
            LeadPlanner::plan_dag_for(&spec, &[Role::Pm, Role::Qa, Role::Pm]),
            Err(PlanError::DuplicateRole(Role::Pm))
        );
    }

    #[test]
    fn plan_dag_with_appends_dev_cmd_checks_to_developer_dod_in_order() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let opts = PlanOptions {
            dev_cmd_checks: vec![
                CmdCheck {
                    run: "cargo test".to_string(),
                    expect: "exit 0".to_string(),
                },
                CmdCheck {
                    run: "npm run build".to_string(),
                    expect: "exit 0".to_string(),
                },
            ],
        };
        let dag = LeadPlanner::plan_dag_with(&spec, &ROLE_ORDER, &opts).unwrap();
        let all_req_ids: Vec<ReqId> = spec.requirements.iter().map(|r| r.id.clone()).collect();

        let dev_task = dag.tasks.iter().find(|t| t.role == Role::Developer).unwrap();
        assert_eq!(
            dev_task.dod,
            vec![
                DodCheck::ReqCover { ids: all_req_ids },
                DodCheck::Cmd {
                    run: "cargo test".to_string(),
                    expect: "exit 0".to_string(),
                },
                DodCheck::Cmd {
                    run: "npm run build".to_string(),
                    expect: "exit 0".to_string(),
                },
            ]
        );
    }

    #[test]
    fn plan_dag_with_leaves_non_developer_dod_untouched_by_injected_checks() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let opts = PlanOptions {
            dev_cmd_checks: vec![CmdCheck {
                run: "cargo test".to_string(),
                expect: "exit 0".to_string(),
            }],
        };
        let dag = LeadPlanner::plan_dag_with(&spec, &ROLE_ORDER, &opts).unwrap();
        let all_req_ids: Vec<ReqId> = spec.requirements.iter().map(|r| r.id.clone()).collect();

        for task in dag.tasks.iter().filter(|t| t.role != Role::Developer) {
            assert_eq!(
                task.dod,
                vec![DodCheck::ReqCover {
                    ids: all_req_ids.clone()
                }]
            );
        }
    }

    #[test]
    fn plan_dag_with_empty_dev_cmd_checks_matches_default_boundary() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let opts = PlanOptions {
            dev_cmd_checks: vec![],
        };
        let via_empty = LeadPlanner::plan_dag_with(&spec, &ROLE_ORDER, &opts).unwrap();
        let via_default =
            LeadPlanner::plan_dag_with(&spec, &ROLE_ORDER, &PlanOptions::default()).unwrap();
        assert_eq!(via_empty, via_default);
    }

    #[test]
    fn plan_dag_with_no_developer_role_silently_drops_dev_cmd_checks_without_error() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let opts = PlanOptions {
            dev_cmd_checks: vec![CmdCheck {
                run: "cargo test".to_string(),
                expect: "exit 0".to_string(),
            }],
        };
        let dag = LeadPlanner::plan_dag_with(&spec, &[Role::Pm, Role::Qa], &opts).unwrap();

        assert_eq!(dag.tasks.len(), 2);
        for task in &dag.tasks {
            assert!(
                !task.dod.iter().any(|c| matches!(c, DodCheck::Cmd { .. })),
                "no task should carry a Cmd check when Developer is absent from roles"
            );
        }
    }

    #[test]
    fn plan_dag_with_rejects_empty_roles() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let opts = PlanOptions {
            dev_cmd_checks: vec![CmdCheck {
                run: "cargo test".to_string(),
                expect: "exit 0".to_string(),
            }],
        };
        assert_eq!(
            LeadPlanner::plan_dag_with(&spec, &[], &opts),
            Err(PlanError::EmptyRoles)
        );
    }

    #[test]
    fn plan_dag_with_rejects_duplicate_roles() {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let opts = PlanOptions {
            dev_cmd_checks: vec![CmdCheck {
                run: "cargo test".to_string(),
                expect: "exit 0".to_string(),
            }],
        };
        assert_eq!(
            LeadPlanner::plan_dag_with(&spec, &[Role::Pm, Role::Pm], &opts),
            Err(PlanError::DuplicateRole(Role::Pm))
        );
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
