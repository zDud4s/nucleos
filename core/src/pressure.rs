//! Quanta janela custou a cada agente, por round, e o que isso diz da forma da equipa.
//!
//! Leitura pura: este módulo não escreve nada, em lado nenhum. Vive à parte de `runs.rs` porque
//! aquele é sobre ESCREVER o ciclo de vida de uma run e este é sobre LER o que ficou escrito — e
//! porque `runs.rs` já passa das cinco mil linhas.
//!
//! O que NÃO faz, de propósito: não é um sinal vivo (a decisão que informa toma-se entre jobs, não
//! durante um), não reparte agentes, não altera equipas. Dá a evidência; a decisão é do dono.

use sqlx::SqlitePool;

/// O que se está a medir.
pub enum Scope {
    /// O job mais recente desta equipa, por nome.
    Team(String),
    /// Um job em concreto, por `team_runs.id`.
    Job(String),
}

#[derive(Debug)]
pub enum Report {
    /// A equipa existe e nunca correu nada. Não é o mesmo que correr e não ter problemas.
    NeverRan { team: String },
    Measured(Measured),
}

#[derive(Debug)]
pub struct Measured {
    pub team: String,
    pub job: String,
    pub rounds: i64,
    pub items_measured: usize,
    /// Itens planeados que nunca chegaram a ter run.
    pub items_without_run: usize,
    /// Itens com run mas sem `tools_used` nem `num_turns` — entram na ocupação, ficam fora do
    /// ajuste.
    pub items_without_steps: usize,
}

/// Uma linha por item planeado, com o que a sua run gastou — ou nada, se nunca correu.
#[derive(Debug)]
pub struct Item {
    pub round: i64,
    pub agent_id: String,
    pub agent_name: String,
    /// `None` quando o item foi planeado e nunca chegou a ter run.
    pub run_id: Option<i64>,
    pub context_peak: Option<i64>,
    pub compacted: bool,
    pub tools_used: Option<String>,
    pub num_turns: Option<i64>,
}

/// Uma linha da colheita, tal como o SQLite a devolve. Nomeada porque clippy conta os braços do
/// tuplo, e oito é mais do que ele tolera anónimo.
type HarvestedRow = (
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    i64,
    Option<String>,
    Option<i64>,
);

/// `LEFT JOIN` e não `JOIN`: um item pode não ter chegado a correr, e isso é um estado real. Um
/// `JOIN` deitava-o fora em silêncio e o relatório dizia que a layer teve menos trabalho do que
/// teve.
const COLHEITA: &str = "\
SELECT ti.round, ti.agent_id, a.name AS agent_name, r.id AS run_id,
       r.context_peak, COALESCE(r.compacted, 0) AS compacted, r.tools_used, r.num_turns
FROM team_items ti
JOIN agents a ON a.id = ti.agent_id
LEFT JOIN runs r ON r.id = ti.run_id
WHERE ti.team_run_id = ?
ORDER BY ti.round, ti.ordinal";

async fn harvest(pool: &SqlitePool, job: &str) -> sqlx::Result<Vec<Item>> {
    let rows: Vec<HarvestedRow> = sqlx::query_as(COLHEITA).bind(job).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let (round, agent_id, agent_name, run_id, context_peak, compacted, tools_used, num_turns) =
                row;
            Item {
                round,
                agent_id,
                agent_name,
                run_id,
                context_peak,
                compacted: compacted != 0,
                tools_used,
                num_turns,
            }
        })
        .collect())
}

/// A única porta do módulo. Resolve o âmbito, colhe, e monta o relatório.
///
/// `Scope::Team` resolve para o `team_run` mais recente dessa equipa. Uma equipa que existe e não
/// tem nenhum dá `NeverRan`; um nome que não existe dá erro, porque são coisas diferentes e quem
/// escreveu mal o nome tem direito a saber.
pub async fn measure(pool: &SqlitePool, scope: Scope) -> sqlx::Result<Report> {
    let (team, job) = match scope {
        Scope::Team(name) => {
            let team_id: String = sqlx::query_scalar("SELECT id FROM teams WHERE name = ?")
                .bind(&name)
                .fetch_one(pool)
                .await?;
            let latest: Option<String> = sqlx::query_scalar(
                "SELECT id FROM team_runs WHERE team_id = ?
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            )
            .bind(&team_id)
            .fetch_optional(pool)
            .await?;
            match latest {
                Some(job) => (name, job),
                None => return Ok(Report::NeverRan { team: name }),
            }
        }
        Scope::Job(job) => {
            let team: String = sqlx::query_scalar(
                "SELECT t.name FROM team_runs tr JOIN teams t ON t.id = tr.team_id WHERE tr.id = ?",
            )
            .bind(&job)
            .fetch_one(pool)
            .await?;
            (team, job)
        }
    };

    let items = harvest(pool, &job).await?;
    let mut rounds: Vec<i64> = items.iter().map(|item| item.round).collect();
    rounds.sort_unstable();
    rounds.dedup();

    Ok(Report::Measured(Measured {
        team,
        job,
        rounds: rounds.len() as i64,
        items_measured: items.iter().filter(|item| item.run_id.is_some()).count(),
        items_without_run: items.iter().filter(|item| item.run_id.is_none()).count(),
        items_without_steps: items
            .iter()
            .filter(|item| {
                item.run_id.is_some()
                    && steps_of(item.tools_used.as_deref(), item.num_turns).is_none()
            })
            .count(),
    }))
}

/// Quantos passos deu este item. `tools_used` primeiro, `num_turns` em recurso.
///
/// A ordem não é arbitrária: um turno com doze ferramentas e um turno com uma contam igual em
/// `num_turns`. Recorrer a ele é perder resolução, e `Measured::items_without_steps` diz quantas
/// vezes foi preciso.
///
/// Uma lista malformada conta como ausente e nunca como zero: lixo não é a afirmação «não usou
/// ferramentas», e tratá-lo como tal punha um ponto falso no ajuste.
fn steps_of(tools_used: Option<&str>, num_turns: Option<i64>) -> Option<i64> {
    if let Some(raw) = tools_used
        && let Ok(serde_json::Value::Array(calls)) = serde_json::from_str::<serde_json::Value>(raw)
    {
        return Some(calls.len() as i64);
    }
    num_turns
}

#[derive(Debug, Clone)]
pub enum Fit {
    /// Menos de três pontos, ou todos no mesmo x. Não há recta que se possa afirmar.
    ///
    /// Devolver zeros seria afirmar «arranque 0, declive 0» — uma leitura, e falsa.
    Insufficient,
    Line {
        intercept: f64,
        slope: f64,
        r2: f64,
    },
}

/// Mínimos quadrados a uma variável, com R² ao lado.
///
/// O R² sai junto e não à parte porque, com uma variável e tarefas heterogéneas, isto é
/// diagnóstico populacional e não previsor de um item. Um arranque sem o R² ao lado lê-se com uma
/// confiança que não tem.
fn fit(points: &[(f64, f64)]) -> Fit {
    if points.len() < 3 {
        return Fit::Insufficient;
    }
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|(x, _)| (x - mx).powi(2)).sum();
    if sxx == 0.0 {
        return Fit::Insufficient;
    }
    let sxy: f64 = points.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
    let slope = sxy / sxx;
    let intercept = my - slope * mx;
    let sst: f64 = points.iter().map(|(_, y)| (y - my).powi(2)).sum();
    let sse: f64 = points
        .iter()
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum();
    Fit::Line {
        intercept,
        slope,
        // Sem variação em y não há nada por explicar, e a recta explica-o todo.
        r2: if sst == 0.0 { 1.0 } else { 1.0 - sse / sst },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// Um item a semear: em que round, de que agente, e com que run — ou nenhuma.
    struct Seed {
        round: i64,
        agent: &'static str,
        run: Option<RunSeed>,
    }

    /// O que a `runs` desse item vai ter. `Default` é uma run saudável e sem nada a assinalar.
    #[derive(Default)]
    struct RunSeed {
        context_peak: Option<i64>,
        compacted: bool,
        tools: usize,
    }

    /// Insere um agente com o nome dado, se ainda não existir.
    ///
    /// A especialidade é de fachada: nada neste módulo a lê, e um teste que a inventasse estaria a
    /// sugerir que sim.
    async fn seed_agent(pool: &SqlitePool, name: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO agents (id, name, speciality, prompt, engine,
                                           tool_policy, created_at, updated_at)
             VALUES (?, ?, 'a speciality', 'a prompt', 'claude', 'unrestricted',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(name)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Insere equipa, director e membros. Devolve o `teams.id`.
    async fn seed_team(pool: &SqlitePool, name: &str, members: &[&str]) -> String {
        seed_agent(pool, "director").await;
        for member in members {
            seed_agent(pool, member).await;
        }
        let team_id = name.to_lowercase();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES (?, ?, 'a mission', 'director', 3, 3,
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(&team_id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
        for member in members {
            sqlx::query("INSERT INTO team_members (team_id, agent_id) VALUES (?, ?)")
                .bind(&team_id)
                .bind(member)
                .execute(pool)
                .await
                .unwrap();
        }
        team_id
    }

    /// Insere um `team_run` sobre a equipa `NucleOS` e os itens pedidos, criando as `runs` onde há
    /// `RunSeed`. Devolve o `team_runs.id`.
    async fn seed_job(pool: &SqlitePool, items: &[Seed]) -> String {
        let mut members: Vec<&str> = items.iter().map(|seed| seed.agent).collect();
        members.sort_unstable();
        members.dedup();
        let team_id = seed_team(pool, "NucleOS", &members).await;

        let job = "job-1".to_string();
        sqlx::query(
            "INSERT INTO team_runs (id, team_id, request, workspace, token, state,
                                    created_at, updated_at)
             VALUES (?, ?, 'a request', 'a workspace', 'a token', 'running',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(&job)
        .bind(&team_id)
        .execute(pool)
        .await
        .unwrap();

        for (ordinal, seed) in items.iter().enumerate() {
            let run_id: Option<i64> = match &seed.run {
                Some(run) => {
                    let tools =
                        serde_json::to_string(&vec![serde_json::json!({"name": "Bash"}); run.tools])
                            .unwrap();
                    let id: i64 = sqlx::query_scalar(
                        "INSERT INTO runs (prompt, status, mode, context_peak, compacted,
                                           tools_used, team_run_id, created_at)
                         VALUES ('an item', 'completed', 'worktree', ?, ?, ?, ?,
                                 '2026-08-26T00:00:00Z')
                         RETURNING id",
                    )
                    .bind(run.context_peak)
                    .bind(run.compacted)
                    .bind(&tools)
                    .bind(&job)
                    .fetch_one(pool)
                    .await
                    .unwrap();
                    Some(id)
                }
                None => None,
            };
            sqlx::query(
                "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description,
                                         state, run_id)
                 VALUES (?, ?, ?, ?, 'do the thing', 'done', ?)",
            )
            .bind(&job)
            .bind(ordinal as i64 + 1)
            .bind(seed.round)
            .bind(seed.agent)
            .bind(run_id)
            .execute(pool)
            .await
            .unwrap();
        }
        job
    }

    #[tokio::test]
    async fn a_team_that_never_ran_says_so_instead_of_showing_zeros() {
        let pool = test_pool().await;
        seed_team(&pool, "NucleOS", &["Nucleo", "Concha"]).await;

        // Um relatório verde e vazio lê-se como aprovação. Não é o que aconteceu: não aconteceu
        // nada.
        assert!(matches!(
            measure(&pool, Scope::Team("NucleOS".into())).await.unwrap(),
            Report::NeverRan { .. }
        ));
    }

    #[tokio::test]
    async fn an_item_that_never_ran_is_counted_apart_and_not_dropped() {
        let pool = test_pool().await;
        let job = seed_job(
            &pool,
            &[
                Seed {
                    round: 1,
                    agent: "Nucleo",
                    run: Some(RunSeed::default()),
                },
                // Planeado, nunca correu.
                Seed {
                    round: 1,
                    agent: "Concha",
                    run: None,
                },
            ],
        )
        .await;

        let Report::Measured(m) = measure(&pool, Scope::Job(job)).await.unwrap() else {
            panic!("um job semeado mede-se")
        };
        assert_eq!(m.items_measured, 1);
        assert_eq!(
            m.items_without_run, 1,
            "não corridos contam-se; deitá-los fora seria dizer que a layer teve menos trabalho do que teve"
        );
    }

    #[test]
    fn steps_prefer_tools_and_fall_back_to_turns() {
        assert_eq!(
            steps_of(Some(r#"[{"name":"Bash"},{"name":"Read"}]"#), Some(9)),
            Some(2)
        );
        assert_eq!(steps_of(None, Some(9)), Some(9));
        assert_eq!(steps_of(None, None), None);
    }

    #[test]
    fn an_empty_tool_list_is_zero_steps_and_not_a_fallback() {
        // `[]` é uma afirmação: «não usou ferramentas». Não é ausência.
        assert_eq!(steps_of(Some("[]"), Some(7)), Some(0));
    }

    #[test]
    fn a_malformed_tool_list_is_absent_and_never_zero() {
        // Lixo não é uma afirmação. Recorre-se aos turnos.
        assert_eq!(steps_of(Some("{isto nao e uma lista"), Some(4)), Some(4));
        assert_eq!(steps_of(Some("{isto nao e uma lista"), None), None);
    }

    #[test]
    fn the_fit_needs_spread_and_says_so_when_it_has_none() {
        // Três itens todos com 10 passos: não há declive que se possa afirmar.
        assert!(matches!(
            fit(&[(10.0, 100_000.0), (10.0, 110_000.0), (10.0, 105_000.0)]),
            Fit::Insufficient
        ));
        // Dois pontos são uma recta trivial, não uma medição.
        assert!(matches!(
            fit(&[(10.0, 100_000.0), (20.0, 120_000.0)]),
            Fit::Insufficient
        ));
    }

    #[test]
    fn the_fit_recovers_a_line_it_was_given() {
        // arranque 50k, declive 1 400/passo, exacto.
        let Fit::Line { intercept, slope, r2 } =
            fit(&[(10.0, 64_000.0), (20.0, 78_000.0), (30.0, 92_000.0)])
        else {
            panic!("devia ajustar")
        };
        assert!((intercept - 50_000.0).abs() < 1.0);
        assert!((slope - 1_400.0).abs() < 0.1);
        assert!(r2 > 0.99);
    }
}
