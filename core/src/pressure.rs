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

/// Internamente etiquetado para que `never_ran` chegue a quem le como um valor de `outcome`, e
/// nao como a ausencia de campos. Um relatorio vazio que se le como aprovacao e pior do que nao
/// existir, e a rota e onde essa distincao mais facilmente se achata.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Report {
    /// A equipa existe e nunca correu nada. Não é o mesmo que correr e não ter problemas.
    NeverRan {
        team: String,
    },
    Measured(Measured),
}

#[derive(Debug, serde::Serialize)]
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
    pub rollups: Vec<AgentRollup>,
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
            let (
                round,
                agent_id,
                agent_name,
                run_id,
                context_peak,
                compacted,
                tools_used,
                num_turns,
            ) = row;
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
        rollups: {
            let mut rollups = roll_up(&items);
            judge(&mut rollups);
            rollups
        },
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

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentRollup {
    pub round: i64,
    pub agent_id: String,
    pub agent_name: String,
    pub items: usize,
    pub items_with_steps: usize,
    pub compacted_items: usize,
    pub peak_p50: Option<i64>,
    pub peak_p90: Option<i64>,
    pub steps_median: Option<i64>,
    pub fit: Fit,
    /// Preenchido por `judge`, nunca por `roll_up`.
    pub verdicts: Vec<Verdict>,
}

/// Uma linha por `(round, agent_id)`, na ordem dos rounds.
///
/// Aritmética e mais nada — não julga. Dois dos quatro veredictos não são propriedade de um rollup
/// isolado: um compara o agente com os outros do mesmo round, outro compara-o consigo próprio
/// noutros rounds. Quem julga recebe a fatia inteira, e chega a seguir.
fn roll_up(items: &[Item]) -> Vec<AgentRollup> {
    let mut order: Vec<(i64, String)> = Vec::new();
    for item in items {
        let key = (item.round, item.agent_id.clone());
        if !order.contains(&key) {
            order.push(key);
        }
    }
    order
        .into_iter()
        .map(|(round, agent_id)| {
            let group: Vec<&Item> = items
                .iter()
                .filter(|item| item.round == round && item.agent_id == agent_id)
                .collect();
            let mut peaks: Vec<i64> = group.iter().filter_map(|item| item.context_peak).collect();
            peaks.sort_unstable();
            let mut steps: Vec<i64> = group
                .iter()
                .filter_map(|item| steps_of(item.tools_used.as_deref(), item.num_turns))
                .collect();
            steps.sort_unstable();
            // Só os itens que têm as DUAS coisas entram no ajuste: um ponto com um eixo em falta não
            // é meio ponto, é nenhum.
            let points: Vec<(f64, f64)> = group
                .iter()
                .filter_map(|item| {
                    match (
                        steps_of(item.tools_used.as_deref(), item.num_turns),
                        item.context_peak,
                    ) {
                        (Some(steps), Some(peak)) => Some((steps as f64, peak as f64)),
                        _ => None,
                    }
                })
                .collect();
            AgentRollup {
                round,
                agent_name: group
                    .first()
                    .map(|item| item.agent_name.clone())
                    .unwrap_or_default(),
                agent_id,
                items: group.len(),
                items_with_steps: steps.len(),
                compacted_items: group.iter().filter(|item| item.compacted).count(),
                peak_p50: percentile(&peaks, 0.5),
                peak_p90: percentile(&peaks, 0.9),
                steps_median: percentile(&steps, 0.5),
                fit: fit(&points),
                verdicts: Vec::new(),
            }
        })
        .collect()
}

/// Calibracao das leituras. Primeiros palpites, deliberadamente conservadores.
///
/// Nenhum destes numeros foi medido: `team_runs` estava vazia quando isto se escreveu, logo nao
/// havia contra o que calibrar. Estao aqui juntos e com nome para serem revistos depois dos
/// primeiros jobs a serio, e e ISSO que se espera que aconteca -- nao que fiquem.
mod calibracao {
    /// Quantas vezes acima da mediana dos pares do round conta como carregar o round sozinho.
    pub const DESEQUILIBRIO: f64 = 2.0;
    /// Abaixo disto, "mais itens que os pares" e ruido e nao sinal.
    pub const ITENS_MINIMOS: usize = 3;
    /// Subida do primeiro ao ultimo round, em fraccao, para o handoff contar como pesado.
    pub const SUBIDA_ENTRE_ROUNDS: f64 = 0.25;
    /// Que fatia da ocupacao tipica ja la esta antes do agente fazer nada.
    pub const ARRANQUE_DOMINANTE: f64 = 0.5;
    /// Acima disto o arranque amortiza-se e deixa de ser o problema.
    pub const PASSOS_QUE_AMORTIZAM: i64 = 10;
}

/// O que a evidencia sugere fazer a forma da equipa. Nunca e feito automaticamente.
///
/// Nao sao exclusivos: um agente pode disparar `SplitSpeciality` e `TrimPrompt` ao mesmo tempo, e
/// isso e informacao -- enche cedo E trabalha pouco, logo o problema esta quase todo no briefing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Os itens deste agente compactaram: a especialidade cobre superficie a mais.
    SplitSpeciality,
    /// Carrega o round quase sozinho: falta gente nesta layer.
    MoreAgents,
    /// O contexto sobe de round para round: o handoff entre layers traz de mais.
    MoreLayers,
    /// Nasce caro e trabalha pouco. Repartir aqui PIORA -- cada metade volta a pagar o arranque.
    TrimPrompt,
}

/// Preenche `verdicts` em cada rollup da fatia.
///
/// Recebe a fatia inteira e nao um rollup porque metade das leituras sao comparacoes:
/// `MoreAgents` mede um agente contra os outros do mesmo round, `MoreLayers` mede-o contra si
/// proprio noutros rounds. Um julgador de um so rollup nao teria como as ver.
///
/// **Nenhuma das quatro usa um limiar absoluto em tokens.** A janela do modelo nao e conhecida por
/// run -- `runner.rs::window_env` so a passa quando a pergunta a traz, e uma run de agente nao traz
/// -- portanto todas comparam o agente consigo proprio ou com os pares.
fn judge(rollups: &mut [AgentRollup]) {
    // Uma copia dos tres campos que as comparacoes leem, tirada antes de se escrever em qualquer
    // rollup: um julgamento tem de ver a fatia como ela chegou, nao meio-escrita.
    let seen: Vec<(i64, String, usize, Option<i64>)> = rollups
        .iter()
        .map(|rollup| {
            (
                rollup.round,
                rollup.agent_id.clone(),
                rollup.items,
                rollup.peak_p50,
            )
        })
        .collect();

    for rollup in rollups.iter_mut() {
        let mut verdicts = Vec::new();

        // Verdade-terreno da propria CLI: nao coube. Uma vez basta.
        if rollup.compacted_items >= 1 {
            verdicts.push(Verdict::SplitSpeciality);
        }

        let peers: Vec<i64> = seen
            .iter()
            .filter(|(round, agent, ..)| *round == rollup.round && *agent != rollup.agent_id)
            .map(|(_, _, items, _)| *items as i64)
            .collect();
        if !peers.is_empty() && rollup.items >= calibracao::ITENS_MINIMOS {
            let mut sorted = peers;
            sorted.sort_unstable();
            // O piso de 1 impede que uma layer onde os pares nao fizeram nada divida por zero e
            // declare desequilibrio infinito.
            let baseline = percentile(&sorted, 0.5).unwrap_or(1).max(1) as f64;
            if rollup.items as f64 >= calibracao::DESEQUILIBRIO * baseline {
                verdicts.push(Verdict::MoreAgents);
            }
        }

        let mut mine: Vec<(i64, Option<i64>)> = seen
            .iter()
            .filter(|(_, agent, ..)| *agent == rollup.agent_id)
            .map(|(round, _, _, p50)| (*round, *p50))
            .collect();
        mine.sort_by_key(|(round, _)| *round);
        // Prende-se ao ULTIMO round em que o agente aparece, que e onde a subida ja e visivel
        // inteira.
        if mine.len() >= 2
            && mine.last().map(|(round, _)| *round) == Some(rollup.round)
            && let Some(peaks) = mine
                .iter()
                .map(|(_, p50)| *p50)
                .collect::<Option<Vec<i64>>>()
            && peaks.windows(2).all(|pair| pair[1] > pair[0])
            && peaks[0] > 0
            && (peaks[peaks.len() - 1] - peaks[0]) as f64 / peaks[0] as f64
                >= calibracao::SUBIDA_ENTRE_ROUNDS
        {
            verdicts.push(Verdict::MoreLayers);
        }

        if let Fit::Line { intercept, .. } = &rollup.fit
            && let Some(p50) = rollup.peak_p50
            && let Some(steps) = rollup.steps_median
            && *intercept >= calibracao::ARRANQUE_DOMINANTE * p50 as f64
            && steps <= calibracao::PASSOS_QUE_AMORTIZAM
        {
            verdicts.push(Verdict::TrimPrompt);
        }

        rollup.verdicts = verdicts;
    }
}

/// Percentil por interpolação linear sobre os valores presentes, ordenados.
///
/// Escrito à mão e não trazido de uma dependência: são sete linhas e o repositório não tem nenhuma
/// crate de estatística. Com dois valores o p50 é a média dos dois, e não o menor — as duas
/// respostas são defensáveis e esta é a que está aqui.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        // Nenhum valor não é o valor zero.
        return None;
    }
    let rank = p * (sorted.len() - 1) as f64;
    let low = rank.floor() as usize;
    let high = rank.ceil() as usize;
    if low == high {
        return Some(sorted[low]);
    }
    let frac = rank - low as f64;
    Some((sorted[low] as f64 + (sorted[high] - sorted[low]) as f64 * frac).round() as i64)
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
                    let tools = serde_json::to_string(&vec![
                        serde_json::json!({"name": "Bash"});
                        run.tools
                    ])
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

    /// Um `Item` com o que os testes de rollup precisam de dizer, e nada mais.
    fn item(round: i64, agent: &str, peak: Option<i64>, compacted: bool, tools: usize) -> Item {
        Item {
            round,
            agent_id: agent.to_string(),
            agent_name: agent.to_string(),
            run_id: Some(1),
            context_peak: peak,
            compacted,
            // Uma lista com `tools` chamadas — o conteúdo não importa, o comprimento sim.
            tools_used: Some(
                serde_json::to_string(&vec![serde_json::json!({"name": "Bash"}); tools]).unwrap(),
            ),
            num_turns: None,
        }
    }

    /// Um rollup no estado neutro: nada compactou, ocupacao tipica, passos que amortizam, sem
    /// ajuste. Cada teste mexe so no campo que lhe interessa, e o resto fica explicitamente
    /// inofensivo.
    fn r(round: i64, agent: &str, items: usize) -> AgentRollup {
        AgentRollup {
            round,
            agent_id: agent.to_string(),
            agent_name: agent.to_string(),
            items,
            items_with_steps: items,
            compacted_items: 0,
            peak_p50: Some(100_000),
            peak_p90: Some(100_000),
            steps_median: Some(20),
            fit: Fit::Insufficient,
            verdicts: Vec::new(),
        }
    }

    /// Julga a fatia e devolve os veredictos de um agente num round.
    fn verdicts_of(rollups: &mut [AgentRollup], agent: &str, round: i64) -> Vec<Verdict> {
        judge(rollups);
        rollups
            .iter()
            .find(|x| x.agent_name == agent && x.round == round)
            .expect("o rollup pedido existe")
            .verdicts
            .clone()
    }

    #[test]
    fn an_agent_whose_items_compact_is_told_to_split() {
        // Uma compactacao basta. Nao e um limiar: e a CLI a dizer que nao coube.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].compacted_items = 1;
        assert!(verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::SplitSpeciality));
    }

    #[test]
    fn a_high_peak_that_never_compacted_is_slack_and_not_pressure() {
        // 180k sem compactacao e folga. E a distincao inteira que o veredicto guarda, e a razao de
        // o sinal ser `compacted` e nao uma contagem de tokens.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].peak_p90 = Some(180_000);
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::SplitSpeciality));
    }

    #[test]
    fn an_agent_carrying_the_round_alone_wants_company() {
        // 4 itens contra pares com 1: 4 >= 2 x max(1, mediana 1).
        let mut rs = vec![r(1, "Nucleo", 4), r(1, "Concha", 1), r(1, "Portao", 1)];
        assert!(verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn an_even_round_wants_nothing() {
        let mut rs = vec![r(1, "Nucleo", 3), r(1, "Concha", 3)];
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn two_items_against_one_is_noise_and_not_imbalance() {
        // 2 >= 2 x 1 e verdade, mas 2 < ITENS_MINIMOS. O piso existe exactamente para este caso.
        let mut rs = vec![r(1, "Nucleo", 2), r(1, "Concha", 1)];
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn an_agent_alone_in_its_round_is_not_called_imbalanced() {
        // Sem pares nao ha desequilibrio possivel -- so uma layer de um. Guarda contra o
        // `max(1, ...)` transformar "nao ha mediana" em "a mediana e 1".
        let mut rs = vec![r(1, "Nucleo", 5)];
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn context_climbing_across_rounds_asks_for_a_layer() {
        // 100k -> 150k: +50%, acima de SUBIDA_ENTRE_ROUNDS. O veredicto prende-se ao ultimo round.
        let mut rs = vec![r(1, "Nucleo", 2), r(2, "Nucleo", 2)];
        rs[0].peak_p50 = Some(100_000);
        rs[1].peak_p50 = Some(150_000);
        assert!(verdicts_of(&mut rs, "Nucleo", 2).contains(&Verdict::MoreLayers));
    }

    #[test]
    fn context_that_holds_steady_does_not() {
        let mut rs = vec![r(1, "Nucleo", 2), r(2, "Nucleo", 2)];
        rs[0].peak_p50 = Some(100_000);
        // +5%, ruido.
        rs[1].peak_p50 = Some(105_000);
        assert!(!verdicts_of(&mut rs, "Nucleo", 2).contains(&Verdict::MoreLayers));
    }

    #[test]
    fn a_high_start_with_little_work_is_a_prompt_problem() {
        // Arranque de 60k numa ocupacao tipica de 100k, e mediana de 4 passos: nasce caro e faz
        // pouco.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].fit = Fit::Line {
            intercept: 60_000.0,
            slope: 900.0,
            r2: 0.8,
        };
        rs[0].steps_median = Some(4);
        assert!(verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::TrimPrompt));
    }

    #[test]
    fn a_high_start_that_does_a_lot_of_work_is_not() {
        // O mesmo arranque, 40 passos: amortiza-se. Repartir aqui pagava-o outra vez, e e o erro
        // que este veredicto existe para impedir.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].fit = Fit::Line {
            intercept: 60_000.0,
            slope: 900.0,
            r2: 0.8,
        };
        rs[0].steps_median = Some(40);
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::TrimPrompt));
    }

    #[test]
    fn a_rollup_keeps_rounds_apart() {
        // O mesmo agente em dois rounds dá duas linhas, nunca uma soma. Somar apagaria a subida
        // entre rounds, que é precisamente o terceiro veredicto.
        let rollups = roll_up(&[
            item(1, "Nucleo", Some(100_000), false, 10),
            item(2, "Nucleo", Some(150_000), false, 10),
        ]);
        assert_eq!(rollups.len(), 2);
        assert_eq!(rollups[0].round, 1);
        assert_eq!(rollups[1].round, 2);
    }

    #[test]
    fn percentiles_come_from_the_items_that_have_a_peak() {
        let rollups = roll_up(&[
            item(1, "Nucleo", Some(100_000), false, 5),
            // Sem pico: conta para `items`, não para o p50.
            item(1, "Nucleo", None, false, 5),
            item(1, "Nucleo", Some(200_000), false, 5),
        ]);
        let r = &rollups[0];
        assert_eq!(
            r.items, 3,
            "o item sem pico continua a ser um item da layer"
        );
        assert_eq!(r.peak_p50, Some(150_000), "mediana de [100k, 200k]");
    }

    #[test]
    fn an_agent_with_no_peaks_at_all_has_no_percentiles() {
        // Nenhum pico não é pico zero. Zero seria uma afirmação, e falsa.
        let rollups = roll_up(&[item(1, "Nucleo", None, false, 5)]);
        assert_eq!(rollups[0].peak_p50, None);
        assert_eq!(rollups[0].peak_p90, None);
    }

    #[test]
    fn compacted_items_are_counted_and_not_just_flagged() {
        // O primeiro veredicto conta-os; saber que «algum» compactou não chega para o relatório
        // dizer quantos dos quantos.
        let rollups = roll_up(&[
            item(1, "Nucleo", Some(100_000), true, 5),
            item(1, "Nucleo", Some(100_000), false, 5),
        ]);
        assert_eq!(rollups[0].compacted_items, 1);
        assert_eq!(rollups[0].items, 2);
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
        let Fit::Line {
            intercept,
            slope,
            r2,
        } = fit(&[(10.0, 64_000.0), (20.0, 78_000.0), (30.0, 92_000.0)])
        else {
            panic!("devia ajustar")
        };
        assert!((intercept - 50_000.0).abs() < 1.0);
        assert!((slope - 1_400.0).abs() < 0.1);
        assert!(r2 > 0.99);
    }
}
