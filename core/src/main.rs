mod agent;
mod assistant;
mod assistants;
mod attention;
mod auth;
mod autopilot;
mod autostart;
mod backup;
mod brief;
mod browser;
mod browser_client;
mod browser_policy;
mod browser_wheel;
mod budget;
mod calendar;
mod capabilities;
mod chat_groups;
mod chat_notices;
mod chats;
mod classifier;
mod collision;
mod command_reader;
mod commands;
mod concurrency;
mod config;
mod consolidate;
mod contacts;
mod council;
mod daemon_client;
mod detect;
mod email;
mod exclusion;
mod feed;
mod files;
mod gate;
mod git_exec;
mod github;
mod handoff;
mod health;
mod hooks;
mod http;
mod inspect;
mod job;
mod join;
mod judge;
mod knowledge;
mod land;
mod local_agent;
mod logging;
mod machine_config;
mod mailsend;
mod map_anchor;
mod map_intent;
mod map_items;
mod map_join;
mod map_orphan;
mod map_recency;
mod map_seam;
mod map_stamp;
mod map_store;
mod map_triage;
mod mcp_tools;
mod mentions;
mod model_catalog;
mod notes;
mod notify;
mod notify_policy;
mod onboarding;
mod openai_compatible;
mod ownership;
mod pii_shadow;
mod presets;
mod pressure;
mod priority;
mod process_tree;
mod project_commands;
mod project_exit;
mod project_map;
mod project_policy;
mod project_readings;
mod project_state;
mod prompt_budget;
mod proposals;
mod quota;
mod quota_client;
mod recurrence;
mod redact;
mod relay;
mod repo_trigger;
mod resolver;
mod route_advice;
mod route_report;
mod router_client;
mod run_stop;
mod runner;
mod runs;
mod scheduler;
mod search;
mod seat_advice;
mod secrets;
mod seed;
mod sessions;
mod shadow;
mod sidecar;
mod speak;
mod speed;
mod state;
mod storage;
mod team;
mod team_notes;
mod team_trigger;
#[cfg(test)]
mod testdb;
mod token_efficiency;
mod transcribe;
mod triage;
mod trust;
mod vcs;
mod voice;
mod wave;
mod web;
mod web_client;
mod webhook;
mod wip;
mod workflow_graph;
mod workflow_materialize;
mod workflow_package;
mod workflows;
mod worktree;

use auth::Token;
use state::AppState;
use std::sync::Arc;

const TOKEN_KEY: &str = "daemon-token";
const TELEGRAM_TOKEN_KEY: &str = "telegram-token";
/// The mailbox password (spec §3.4). An app password, in Credential Manager rather than in
/// `~/.nucleos/email.yaml`, so the one secret the pillar needs never sits in a file next to the config.
const EMAIL_PASSWORD_KEY: &str = "email-imap-password";
/// The web search provider's API key, in Credential Manager like every other secret — no key on
/// disk, and in particular not in `~/.nucleos/web.yaml`, which is plain text anybody may open.
const WEB_SEARCH_KEY: &str = "web-search-api-key";
/// OpenRouter's own API key, in Credential Manager for the same reason every secret above is: it
/// never sits in `~/.nucleos/nucleos-models.yaml`, which only ever names the model
/// (`hosted_assistant_model`) and is plain text anybody may open.
/// `openai_compatible::OpenAiCompatibleChat::new` refuses outright when this comes back `None` —
/// see its own doc comment for why that refusal happens before any request leaves the machine
/// rather than after a 401 comes back.
const OPENROUTER_KEY: &str = "openrouter-api-key";

/// Reads a secret from stdin rather than from `argv`.
///
/// A Windows command line is readable by any process running as the same user
/// (`Get-CimInstance Win32_Process | select CommandLine`) and is recorded verbatim in PSReadLine's
/// plaintext history file. Passing an IMAP app password or a bot token as an argument therefore put
/// it on disk in cleartext at the exact moment the operator was securely storing it, which is the
/// one thing this path exists to avoid.
fn read_secret_from_stdin(prompt: &str) -> Option<String> {
    use std::io::BufRead;

    eprintln!("{prompt}");
    let mut value = String::new();
    if std::io::stdin().lock().read_line(&mut value).is_err() {
        return None;
    }
    // The line terminator only. A secret may legitimately end in a space, and silently eating one
    // would store a credential that differs from what was pasted — a failure that surfaces much
    // later as an authentication error nobody connects back to this prompt.
    let value = value
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// The workflow library, with the built-in autopilot in it.
///
/// **Seeding failing does not stop the daemon**, and that is the whole of the error handling here.
/// A library that could not be written is a canvas with one fewer workflow on it; refusing to start
/// over it would take away email, chats, runs and the autopilot itself because a picture could not
/// be drawn. It is said out loud and then let go — the same shape [`crate::backup`] uses for a
/// restore that could not be applied.
fn seeded_library() -> Option<std::path::PathBuf> {
    let root = workflows::library_root()?;
    match seed::seed(&root) {
        // The ordinary case, on every start after the first, and it says nothing.
        Ok(seed::Seeded::Unchanged) => {}
        Ok(seed::Seeded::Written) => {
            tracing::info!(
                name = seed::NAME,
                version = seed::VERSION,
                "seeded the built-in workflow"
            )
        }
        // The one case a person needs told: their disk changed under a name that means one thing.
        Ok(seed::Seeded::Restored) => tracing::warn!(
            name = seed::NAME,
            version = seed::VERSION,
            "the built-in workflow had been changed on disk and was restored; publish a new version \
             to keep your own"
        ),
        Err(error) => {
            tracing::warn!(%error, "could not seed the built-in workflow; the library is short one")
        }
    }
    Some(root)
}

/// Copies this machine's settings from the old `.ai/` beside the working directory into `root`,
/// and puts a line in the feed for each file it copied.
///
/// The copying is [`machine_config::migrate_legacy`]'s, and so is everything about when it does and
/// does not happen; this only supplies the two directories and tells somebody. The feed line is
/// said because a person looking for why their settings moved will look there, and a log line is
/// read by nobody on a desktop. A lost feed line is logged and let go: the copy already happened.
async fn migrate_machine_settings(pool: &sqlx::SqlitePool, root: &std::path::Path) {
    let Ok(started_in) = std::env::current_dir() else {
        // No working directory means no `.ai/` beside it either, so there is nothing to copy.
        return;
    };
    for file in machine_config::migrate_legacy(root, &started_in) {
        let summary = format!(
            "{} copied from .ai/{file} in the directory the daemon was started from; the old file \
             was left where it was and is no longer read",
            machine_config::display_path(file)
        );
        if let Err(error) = feed::append(pool, None, "config_migrated", &summary, None, None).await
        {
            tracing::warn!(%error, file, "a settings file was copied but the feed line was lost");
        }
    }
}

/// Copies each rostered project's state files from its old `<project root>/.ai/` into
/// `~/.nucleos/projects/<project_id>/`, and puts a line in that project's feed for each one.
///
/// The copying is [`project_state::migrate_legacy`]'s, and so is every rule about when it does and
/// does not happen; this only reads the roster and tells somebody, for the reason
/// [`migrate_machine_settings`] gives. The roster is every project with a root on record — a
/// project in `off` has none, and its old files are copied the next time it is given one and the
/// daemon starts.
///
/// An unreadable roster copies nothing and says so: those projects behave as projects with no rules
/// file until the next start, which is the state an absent file has always meant.
async fn migrate_project_state(pool: &sqlx::SqlitePool, root: &std::path::Path) {
    let roster: Vec<(String, String)> = match sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    {
        Ok(roster) => roster,
        Err(error) => {
            tracing::warn!(%error, "could not read the project roster, so no project's .ai/ state was copied");
            return;
        }
    };
    let roster: Vec<(String, std::path::PathBuf)> = roster
        .into_iter()
        .map(|(id, project_root)| (id, std::path::PathBuf::from(project_root)))
        .collect();
    let root = root.to_path_buf();
    let copied =
        match tokio::task::spawn_blocking(move || project_state::migrate_legacy(&root, &roster))
            .await
        {
            Ok(copied) => copied,
            Err(error) => {
                tracing::warn!(%error, "copying project state from .ai/ did not finish");
                return;
            }
        };
    for (project_id, file) in copied {
        let summary = format!(
            "{} copied from .ai/{file} in this project's folder; the old file was left where it \
             was and is no longer read",
            project_state::display_path(&project_id, file)
        );
        if let Err(error) = feed::append(
            pool,
            Some(&project_id),
            "config_migrated",
            &summary,
            None,
            None,
        )
        .await
        {
            tracing::warn!(%error, %project_id, file, "a project state file was copied but the feed line was lost");
        }
    }
}

/// Marks every rostered project that passed the old activation check as onboarded, once, and puts
/// a line in that project's feed for each one.
///
/// So that nobody loses autopilot on update: activation used to require `.ai/workflow/workflow.md`
/// and now requires the onboarding marker, so a project that had the first and not the second is
/// given a `migrated: true` marker. The rules are [`onboarding::migrate_legacy`]'s; this reads the
/// same roster [`migrate_project_state`] reads — every project with a root on record — and tells
/// somebody, for the reason [`migrate_machine_settings`] gives.
async fn migrate_onboarding(pool: &sqlx::SqlitePool, root: &std::path::Path) {
    let roster: Vec<(String, String)> = match sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    {
        Ok(roster) => roster,
        Err(error) => {
            tracing::warn!(%error, "could not read the project roster, so no project was marked onboarded");
            return;
        }
    };
    let roster: Vec<(String, std::path::PathBuf)> = roster
        .into_iter()
        .map(|(id, project_root)| (id, std::path::PathBuf::from(project_root)))
        .collect();
    let root = root.to_path_buf();
    let now = chrono::Utc::now().to_rfc3339();
    let marked =
        match tokio::task::spawn_blocking(move || onboarding::migrate_legacy(&root, &roster, &now))
            .await
        {
            Ok(marked) => marked,
            Err(error) => {
                tracing::warn!(%error, "marking projects onboarded did not finish");
                return;
            }
        };
    for project_id in marked {
        let summary = format!(
            "marked onboarded in {}, because this project had .ai/workflow/workflow.md, which is \
             what onboarded meant before; that file is no longer read",
            project_state::display_path(&project_id, onboarding::MARKER_FILE)
        );
        if let Err(error) = feed::append(
            pool,
            Some(&project_id),
            onboarding::FEED_KIND,
            &summary,
            None,
            None,
        )
        .await
        {
            tracing::warn!(%error, %project_id, "a project was marked onboarded but the feed line was lost");
        }
    }
}

/// Um numero, ou um travessao quando nao ha nenhum.
///
/// Nada por medir escreve-se `—` e nunca `0`: zero e uma afirmacao, e sobre uma janela que ninguem
/// mediu e falsa. E a mesma razao por que a coluna nasceu NULL.
fn or_dash(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(number) => format!("{number}"),
        _ => "—".to_string(),
    }
}

/// Desenha o relatorio que a rota devolveu.
///
/// Devolve texto em vez de o imprimir para que o desenho tenha teste: e a unica parte deste
/// caminho que pode estar errada sem que nada estoire, e a verificacao ponta-a-ponta exigia por o
/// daemon a correr contra a base de dados de quem manda.
///
/// Le o JSON como `Value` em vez de o desserializar para os tipos de `pressure`: o formato e um
/// contrato de leitura e nao um tipo partilhado, e um cliente fino que exigisse os tipos passaria a
/// quebrar de cada vez que se acrescentasse um campo.
fn render_pressure(report: &serde_json::Value) -> String {
    use std::fmt::Write as _;

    let team = report["team"].as_str().unwrap_or("?");
    if report["outcome"] == "never_ran" {
        // A saida correcta enquanto `team_runs` estiver vazia, e a que nao se pode confundir com
        // uma tabela de zeros.
        return format!("equipa {team} · nunca correu");
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "equipa {team} · job {} · {} round(s)",
        report["job"].as_str().unwrap_or("?"),
        or_dash(&report["rounds"]),
    );
    let _ = writeln!(
        out,
        "{} itens medidos · {} sem run · {} sem passos",
        or_dash(&report["items_measured"]),
        or_dash(&report["items_without_run"]),
        or_dash(&report["items_without_steps"]),
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<5} {:<14} {:>5} {:>5} {:>9} {:>9} {:>7} {:>9} {:>8} {:>5}  veredicto",
        "round",
        "agente",
        "itens",
        "comp",
        "pico p50",
        "pico p90",
        "passos",
        "arranque",
        "declive",
        "R2",
    );
    for rollup in report["rollups"].as_array().into_iter().flatten() {
        let fit = &rollup["fit"];
        let (arranque, declive, r2) = if fit["kind"] == "line" {
            (
                format!("{:.0}", fit["intercept"].as_f64().unwrap_or_default()),
                format!("{:.0}", fit["slope"].as_f64().unwrap_or_default()),
                format!("{:.2}", fit["r2"].as_f64().unwrap_or_default()),
            )
        } else {
            // Menos de tres pontos, ou todos com os mesmos passos. Um arranque inventado aqui seria
            // lido com uma confianca que nao tem.
            ("—".to_string(), "—".to_string(), "—".to_string())
        };
        let verdicts: Vec<&str> = rollup["verdicts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|verdict| verdict.as_str())
            .collect();
        let _ = writeln!(
            out,
            "{:<5} {:<14} {:>5} {:>5} {:>9} {:>9} {:>7} {:>9} {:>8} {:>5}  {}",
            or_dash(&rollup["round"]),
            rollup["agent_name"].as_str().unwrap_or("?"),
            or_dash(&rollup["items"]),
            or_dash(&rollup["compacted_items"]),
            or_dash(&rollup["peak_p50"]),
            or_dash(&rollup["peak_p90"]),
            or_dash(&rollup["steps_median"]),
            arranque,
            declive,
            r2,
            if verdicts.is_empty() {
                "—".to_string()
            } else {
                verdicts.join(", ")
            },
        );
    }
    out
}

/// Which spellings of `--land` exist — one predicate, called by both the guard in `main` and the
/// parser below, because **the guard and the parser must agree on this and once did not.** The
/// guard matched `--land` exactly, so `--land=release` fell past it, past every other flag block,
/// and into daemon startup: the person asked to land and got a server, with no message and no
/// exit code. Two copies of the answer are what allowed that; one they both call cannot drift.
fn is_land_flag(arg: &str) -> bool {
    arg == "--land" || arg.starts_with("--land=")
}

/// The branch a `--land` names, when it names one: `--land <branch>` or `--land=<branch>`.
///
/// A function rather than four lines inside the block, because it is the only part of `--land`
/// that can be wrong in a way the compiler cannot see, and `main.rs` has no other way to test a
/// command-line shape.
///
/// **Both forms are honoured rather than one of them refused.** `--land=release` has exactly one
/// reading, and turning away a form that cannot be misunderstood is worse than answering it — the
/// same two lines either way.
///
/// `nucleos-core --land` has to keep meaning exactly what it has always meant, so an absent
/// argument is `None` rather than an error. A session that wrote `--land --something` meant the
/// flag: an argument that itself looks like one is not a branch name, and treating it as one would
/// send a typo to the daemon as a landing target. Trimmed before that decision because the daemon
/// trims before its own — ` --verbose` reaches here as one argument when a shell kept the space,
/// and the two ends must not disagree about whether it is a branch.
///
/// **The two empty forms are answered differently, on purpose.** `--land=` carries no value at
/// all, so there is nothing to send and it is `None`. `--land ""` carries one, and it goes as it
/// was typed: `land::resolve_target` reads an empty target as the project's integration branch, so
/// the ends already agree, and a refusal invented here would be a second opinion nobody asked for.
/// Both land in the same place.
///
/// **The first spelling wins when a command line carries more than one**, whichever form it is:
/// that is the one the person typed as the command, and a second is a mistake rather than an
/// override.
fn land_target_from(args: &[String]) -> Option<String> {
    let at = args.iter().position(|arg| is_land_flag(arg))?;
    if let Some(value) = args[at].strip_prefix("--land=") {
        return Some(value.to_owned()).filter(|value| !value.trim().is_empty());
    }
    args.get(at + 1)
        .filter(|value| !value.trim().starts_with('-'))
        .cloned()
}

/// The checkout `--workflow-sync` names: the first word after the flag that is neither a flag nor
/// `--project`'s value, or the working directory. Absolute either way, because the daemon
/// resolving it runs somewhere else.
fn workflow_sync_target(args: &[String]) -> Result<String, String> {
    let at = args
        .iter()
        .position(|arg| arg == "--workflow-sync")
        .ok_or("--workflow-sync was not given")?;
    let mut rest = args[at + 1..].iter();
    let mut named = None;
    while let Some(word) = rest.next() {
        if word == "--project" {
            rest.next();
        } else if !word.starts_with("--") {
            named = Some(std::path::PathBuf::from(word));
            break;
        }
    }
    let path = match named {
        Some(path) => path,
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    std::path::absolute(&path)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// The project `--workflow-sync --project <id>` names, for a repository more than one rostered
/// project points at. `None` when the flag is absent; a flag with no value is refused rather than
/// read as absent, because absent means "let the daemon decide" and that is not what was typed.
fn workflow_sync_project(args: &[String]) -> Result<Option<String>, String> {
    let Some(at) = args.iter().position(|arg| arg == "--project") else {
        return Ok(None);
    };
    args.get(at + 1)
        .filter(|value| !value.starts_with("--") && !value.trim().is_empty())
        .cloned()
        .map(Some)
        .ok_or_else(|| "--project needs a project id".to_string())
}

/// The daemon's `ambiguous_project` refusal, written as the choice it is: one candidate per line,
/// with the flag that picks it. `None` for any other body, which is printed as it came.
fn ambiguous_sync_message(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    if body["refusal"] != "ambiguous_project" {
        return None;
    }
    let mut said =
        String::from("more than one project uses this repository; pick one with --project <id>:\n");
    for candidate in body["candidates"].as_array()? {
        let id = candidate["project_id"].as_str().unwrap_or("?");
        let root = candidate["project_root"].as_str().unwrap_or("?");
        said.push_str(&format!("  {id}  ({root})\n"));
    }
    Some(said)
}

/// `--workflow-package <name> <version> --from <project> --manifest <bundle.yaml>`, all required.
/// Refused as a usage line rather than guessed: packaging writes a version that is never
/// rewritten, so a default that picked the wrong folder would be permanent.
fn workflow_package_args(
    args: &[String],
) -> Result<(String, String, std::path::PathBuf, std::path::PathBuf), String> {
    const USAGE: &str = "usage: nucleos-core --workflow-package <name> <version> --from <project root> --manifest <bundle.yaml>";
    let at = args
        .iter()
        .position(|arg| arg == "--workflow-package")
        .ok_or(USAGE)?;
    let positional = |offset: usize| {
        args.get(at + offset)
            .filter(|value| !value.starts_with("--"))
            .cloned()
            .ok_or(USAGE)
    };
    let value_of = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .filter(|value| !value.starts_with("--"))
            .map(std::path::PathBuf::from)
            .ok_or(USAGE)
    };
    Ok((
        positional(1)?,
        positional(2)?,
        value_of("--from")?,
        value_of("--manifest")?,
    ))
}

#[tokio::main]
async fn main() {
    http::remember_home();
    if std::env::args().any(|a| a == "--print-token") {
        match secrets::load_secret(TOKEN_KEY) {
            Ok(Some(t)) => println!("{t}"),
            Ok(None) => {
                eprintln!("no daemon token stored yet — start the daemon once to generate one");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("failed to read token from the system credential store: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `nucleos-core --land`, run from inside a worktree: "I am finished, take this branch."
    //
    // A subcommand rather than a documented `curl`, for the reason `--print-token` is one: the
    // token lives in Credential Manager, and the alternative is teaching every session how to
    // fetch the master key in order to ask a question about itself. Here the binary reads it, and
    // the session runs one word.
    //
    // It asks; it does not wait. The queue decides when, and the ticket is how to follow it —
    // printing the id and returning is the honest shape for a request whose whole point is that
    // somebody else schedules it.
    if std::env::args().any(|a| is_land_flag(&a)) {
        let token = match secrets::load_secret(TOKEN_KEY) {
            Ok(Some(token)) => token,
            _ => {
                eprintln!("no daemon token stored yet — start the daemon once to generate one");
                std::process::exit(1);
            }
        };
        let cwd = std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let args: Vec<String> = std::env::args().collect();
        let target = land_target_from(&args);
        // Omitted rather than sent as an explicit `null` when there is none, so a bare `--land`
        // puts on the wire the same body every caller put there before the field existed.
        let body = match &target {
            Some(target) => serde_json::json!({ "cwd": cwd, "target": target }),
            None => serde_json::json!({ "cwd": cwd }),
        }
        .to_string();
        let response = reqwest::Client::new()
            .post(format!("{}/vcs/land", daemon_client::daemon_url()))
            .bearer_auth(token)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if status.is_success() {
                    println!("{text}");
                    // Said at the moment of asking, because that is the last moment the asker is
                    // listening. A conflict arrives later and reads like a failure to fix; whoever
                    // read this already knows it is not theirs.
                    eprintln!(
                        "asked. the queue decides when — one operation per repository, in order.\n\
                         watch it with GET /vcs/requests/<id>/wait.\n\
                         if it conflicts, nothing is published and no copy is left conflicted: \
                         that is the queue's to report, not yours to resolve from here."
                    );
                } else {
                    eprintln!("the queue refused: {text}");
                    std::process::exit(1);
                }
            }
            Err(error) => {
                eprintln!("the daemon is not reachable: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `nucleos-core --pressao --equipa NucleOS`: quanta janela custou a cada agente da equipa.
    //
    // Cliente fino pelo molde do `--land` acima, e pela mesma razao: a pool esta aberta no daemon,
    // e abrir o SQLite em paralelo a partir da CLI seria contra o desenho da casa. Aqui a CLI le o
    // token e faz um GET.
    //
    // Nao ha pagina na Concha de proposito. A decisao que isto informa -- mais uma layer, mais um
    // agente, repartir uma especialidade, ou so cortar o prompt -- toma-se entre jobs, e nao ha
    // nada que valha a pena olhar enquanto um corre.
    if std::env::args().any(|a| a == "--pressao") {
        let args: Vec<String> = std::env::args().collect();
        let value_of = |flag: &str| -> Option<String> {
            args.iter()
                .position(|arg| arg == flag)
                .and_then(|at| args.get(at + 1))
                .cloned()
        };
        let job = value_of("--job");
        let equipa = value_of("--equipa");
        // Sem adivinhar por omissao: uma resposta sobre a equipa errada e pior do que nenhuma.
        // Codificado pelo mesmo escapador do `daemon_client`, e nao por um segundo: o argumento do
        // dono chega de uma linha de comandos e um nome com `&` seria outro pedido.
        let query = match (job, equipa) {
            (Some(job), _) => format!("job={}", daemon_client::urlencoding_encode(&job)),
            (None, Some(equipa)) => {
                format!("team={}", daemon_client::urlencoding_encode(&equipa))
            }
            (None, None) => {
                eprintln!("diga de quem: --equipa <nome> ou --job <team_run_id>");
                std::process::exit(1);
            }
        };
        let token = match secrets::load_secret(TOKEN_KEY) {
            Ok(Some(token)) => token,
            _ => {
                eprintln!("no daemon token stored yet — start the daemon once to generate one");
                std::process::exit(1);
            }
        };
        let response = reqwest::Client::new()
            .get(format!("http://127.0.0.1:8791/team-pressure?{query}"))
            .bearer_auth(token)
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::NOT_FOUND {
                    eprintln!("nao ha nenhuma equipa nem job com esse nome");
                    std::process::exit(1);
                }
                if !status.is_success() {
                    eprintln!("o daemon recusou: {text}");
                    std::process::exit(1);
                }
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(report) => println!("{}", render_pressure(&report)),
                    Err(_) => println!("{text}"),
                }
            }
            Err(error) => {
                eprintln!("the daemon is not reachable: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|a| a == "--set-telegram-token") {
        match read_secret_from_stdin("paste the bot token, then press Enter:") {
            Some(value) => match secrets::store_secret(TELEGRAM_TOKEN_KEY, &value) {
                Ok(()) => println!("telegram bot token stored in the system credential store"),
                Err(e) => {
                    eprintln!("failed to store telegram token: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("no bot token was read from stdin");
                std::process::exit(1);
            }
        }
        return;
    }

    // The door the `github-token` comes in by. `secrets.rs` has always exposed store/load/delete and
    // nothing put this key there, so the pillar could be configured, enabled and credential-less with
    // no way to fix it that did not involve writing a second program.
    //
    // Stdin and never an argument, exactly like its two neighbours: a token on a command line is in
    // the shell's history and in every process listing on the machine for as long as this runs.
    if std::env::args().any(|a| a == "--set-github-token") {
        match read_secret_from_stdin(
            "paste the GitHub token (a fine-grained PAT or a classic one), then press Enter:",
        ) {
            Some(value) => match secrets::store_secret(github::TOKEN_KEY, &value) {
                Ok(()) => {
                    println!("github token stored in the system credential store");
                    // Said here because this is the last moment the person is listening, and the
                    // alternative is discovering it from a health row that says permission-denied.
                    eprintln!(
                        "a `gh auth login` on this machine is NOT a substitute and never was: that                          login writes into the interactive session's keyring, and the daemon runs                          as a scheduled task."
                    );
                }
                Err(e) => {
                    eprintln!("failed to store github token: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("no github token was read from stdin");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|a| a == "--set-email-password") {
        match read_secret_from_stdin("paste the app password, then press Enter:") {
            Some(value) => match secrets::store_secret(EMAIL_PASSWORD_KEY, &value) {
                Ok(()) => println!("email password stored in the system credential store"),
                Err(e) => {
                    eprintln!("failed to store email password: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("no app password was read from stdin");
                std::process::exit(1);
            }
        }
        return;
    }

    // The other half of the hosted route. `hosted_assistant_model` names the model in
    // `~/.nucleos/nucleos-models.yaml` and this stores the key, and until both exist the route refuses
    // (`assistants::Refusal::HostedModelNamedButNoKey`) rather than answering. There was no way at
    // all to store this before: the resolver at the bottom of `main` has read `OPENROUTER_KEY`
    // since the hosted route shipped, and nothing on this machine ever wrote it — so the whole
    // route was unreachable by anyone who had not planted a credential by hand.
    //
    // Through stdin like the three above, for the reason `read_secret_from_stdin`'s own doc gives:
    // an argument would put the key in the shell's plaintext history at the exact moment the
    // operator was securely storing it.
    if std::env::args().any(|a| a == "--set-openrouter-key") {
        match read_secret_from_stdin("paste the OpenRouter API key, then press Enter:") {
            Some(value) => match secrets::store_secret(OPENROUTER_KEY, &value) {
                Ok(()) => {
                    println!("openrouter key stored in the system credential store");
                    // Said here because this is the last moment the person is listening, and the
                    // alternative is a chat that refuses with no visible reason: the key alone
                    // gets a conversation nowhere, and the daemon reads both ONCE, at startup.
                    eprintln!(
                        "name a model in `hosted_assistant_model` (~/.nucleos/nucleos-models.yaml) too, \
                         then restart the daemon — both are read at startup and neither half \
                         answers a chat on its own"
                    );
                }
                Err(e) => {
                    eprintln!("failed to store openrouter key: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("no openrouter key was read from stdin");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|a| a == "--mcp-tools") {
        // `--box job-node --job <id>` narrows what this process serves. Refused rather than
        // ignored when the box is not one this server knows: a launcher that misspells it would
        // otherwise get the broad tool set, with nothing saying so.
        let args: Vec<String> = std::env::args().collect();
        let served = match mcp_tools::box_from_args(&args) {
            Ok(served) => served,
            Err(e) => {
                eprintln!("mcp-tools failed: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = mcp_tools::run_stdio(served).await {
            eprintln!("mcp-tools failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // `nucleos-core --workflow-sync [<checkout>] [--project <id>]`: put the project's pinned
    // workflows into a worktree made by hand. A thin client, for `--land`'s reason: which project
    // the checkout belongs to is answered from the roster, and the roster is the running daemon's
    // database. The daemon also holds the kill switch this write must respect. `--project` only
    // chooses among the projects the repository already matches, when there is more than one.
    if std::env::args().any(|a| a == "--workflow-sync") {
        let args: Vec<String> = std::env::args().collect();
        let (path, project) = match workflow_sync_target(&args)
            .and_then(|path| Ok((path, workflow_sync_project(&args)?)))
        {
            Ok(parsed) => parsed,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        };
        let mut body = serde_json::json!({ "path": path });
        if let Some(project) = project {
            body["project_id"] = serde_json::Value::String(project);
        }
        let token = match secrets::load_secret(TOKEN_KEY) {
            Ok(Some(token)) => token,
            _ => {
                eprintln!("no daemon token stored yet — start the daemon once to generate one");
                std::process::exit(1);
            }
        };
        let response = reqwest::Client::new()
            .post(format!("{}/workflows/sync", daemon_client::daemon_url()))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if let Some(choice) = ambiguous_sync_message(&text) {
                    eprint!("{choice}");
                    std::process::exit(1);
                }
                if !status.is_success() {
                    eprintln!("the daemon refused: {text}");
                    std::process::exit(1);
                }
                println!("{text}");
            }
            Err(error) => {
                eprintln!("the daemon is not reachable: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `nucleos-core --workflow-package <name> <version> --from <project> --manifest <bundle.yaml>`:
    // copy a workflow out of a project into the library as a new version, and commit it there.
    //
    // No daemon: this reads a folder and writes `~/.nucleos/workflows/`, and neither is the
    // database. A library a running daemon lists mid-copy sees no manifest yet, which it already
    // treats as somebody's scratch folder (`workflows::read_bundle`), until the manifest is written
    // last.
    if std::env::args().any(|a| a == "--workflow-package") {
        let args: Vec<String> = std::env::args().collect();
        let (name, version, from, manifest) = match workflow_package_args(&args) {
            Ok(parsed) => parsed,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        };
        let Some(library) = workflows::library_root() else {
            eprintln!("this machine has no home directory, so it has no workflow library");
            std::process::exit(1);
        };
        match workflow_package::package(&library, &name, &version, &from, &manifest).await {
            Ok(packaged) => {
                println!(
                    "{name}@{version}: {} files into {} ({})",
                    packaged.files,
                    packaged.dir.display(),
                    packaged.hash
                );
                match packaged.commit {
                    Ok(commit) => println!("recorded in the library's history as {commit}"),
                    Err(why) => {
                        eprintln!("packaged, but not recorded in the library's history: {why}")
                    }
                }
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let dirs = directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
        .expect("could not resolve local app data directory");

    // Whether this process is the machine's daemon or a second one somebody is testing with, and
    // where its database and logs go. Resolved HERE, before the first thing that acts on either,
    // because everything below reads the answer and nothing below may reach for the environment
    // again — two readers of one variable is how a daemon ends up binding one port and telling its
    // own tools to call back on another.
    //
    // The reason this exists at all: the port and the data directory were literals, so exercising
    // a change end to end meant running the new build against the live database, on the live port,
    // after stopping whatever was already serving. That is the one experiment nobody can undo.
    let port_override = std::env::var(daemon_client::PORT_VAR).ok();
    let data_dir_override = std::env::var(daemon_client::DATA_DIR_VAR).ok();
    let is_primary =
        daemon_client::is_primary(port_override.as_deref(), data_dir_override.as_deref());
    let data_dir = match data_dir_override.as_deref().filter(|dir| !dir.is_empty()) {
        Some(dir) => std::path::PathBuf::from(dir),
        None => dirs.data_local_dir().to_path_buf(),
    };
    if !is_primary {
        eprintln!(
            "nucleos-core: secondary instance — data in {}, no autostart, no sidecars",
            data_dir.display()
        );
    }

    let log_dir = data_dir.join("logs");
    let _log_guard = logging::init(&log_dir);

    // Only the machine's daemon claims the logon entry. `ensure_registered` overwrites it (on
    // Windows with `schtasks /F`), so a secondary doing this would point the autostart at whatever
    // build is under test — typically one inside a worktree that is about to be deleted, leaving a
    // task that runs nothing.
    if is_primary {
        match std::env::current_exe() {
            Ok(exe_path) => {
                if let Err(e) = autostart::ensure_registered(&exe_path) {
                    tracing::warn!("failed to register the daemon's autostart entry: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("failed to resolve current exe path for autostart registration: {e}")
            }
        }
    }

    let db_path = data_dir.join("nucleos.db");
    match backup::apply_pending_restore(&db_path).await {
        Ok(Some(applied)) => tracing::warn!(
            "applied pending database restore from {}; safety backup at {}",
            applied.restored_from,
            applied.safety_backup.display()
        ),
        Ok(None) => tracing::info!("no pending database restore to apply"),
        Err(e) => tracing::error!(
            "failed to apply pending database restore; continuing startup and retrying next start: {e}"
        ),
    }
    let pool = storage::open(&db_path)
        .await
        .expect("failed to open local database");
    tracing::info!("nucleos-core database ready at {}", db_path.display());

    // Beside the run sweep and for the same reason: a row saying `running` against a process that
    // has not existed since the last restart would refuse every future click with "already
    // running". Best-effort — a project command left unsettled is a button that will not press, not
    // a daemon that must not start.
    match project_commands::reconcile_orphaned_commands(&pool).await {
        Ok(0) => {}
        Ok(settled) => tracing::info!(
            settled,
            "settled project commands left running by a restart"
        ),
        Err(error) => tracing::warn!(%error, "could not settle project commands left running"),
    }

    let interrupted = runs::reconcile_orphaned_runs(&pool)
        .await
        .expect("failed to reconcile orphaned runs on startup");
    if interrupted > 0 {
        tracing::warn!(
            "reconciled {interrupted} run(s) left 'running' by a previous crash -> 'interrupted'"
        );
    }

    let stranded = runs::reconcile_stranded_approvals(&pool)
        .await
        .expect("failed to reconcile stranded approval pauses on startup");
    if stranded > 0 {
        tracing::warn!(
            "reconciled {stranded} run(s) left 'awaiting_approval' with no pending proposal -> 'interrupted'"
        );
    }

    // Spec A D14: heals rows a release or a job cancel left before `expire_for_run` existed, and
    // does not `expect` — unlike the two reconciliations above, a pending approval holds no slot,
    // so the daemon must not refuse to start over one.
    match proposals::expire_orphaned_approvals(&pool).await {
        Ok(0) => {}
        Ok(expired) => tracing::warn!(
            "expired {expired} approval(s) whose run had already stopped (spec A D14)"
        ),
        Err(error) => tracing::warn!(%error, "could not expire orphaned approvals at startup"),
    }

    // After the run reconciliations, and for a reason worth stating: they mark every run left
    // `running` as `interrupted`, so by now no job has a live node under it — which means "has no
    // live run" is true of every job, including the ones that died in the gap between two nodes.
    // Those are exactly the recoverable ones, so the pass below discriminates on HEAD instead.
    match job::reconcile_orphaned_jobs(&pool).await {
        Ok(retired) if retired > 0 => {
            tracing::warn!(
                "retired {retired} job(s) whose repository moved while the daemon was down -> 'interrupted'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "orphaned-job reconciliation failed"),
    }

    // After both owner reconciliations above, and that order is the whole correctness of this pass:
    // it frees a slot by asking whether its owner is still live, and before those two every dead
    // owner still reads live. Run earlier it would free nothing at all.
    match concurrency::reconcile_orphaned_slots(&pool).await {
        Ok(freed) if freed > 0 => {
            tracing::warn!("freed {freed} concurrency slot(s) left held by a previous crash");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "orphaned concurrency slot sweep failed"),
    }

    // Neither fatal like the run reconciliations above nor mere hygiene like the worktree sweep
    // below: louder than the sweep, quieter than the panics.
    //
    // A `vcs_requests` row left `running` holds its repository's only slot — the partial unique
    // index sees to that — so failing to clear it means no git operation for that project until
    // somebody notices. That is a jam, not untidiness, hence `error!`. But it is one pillar's queue:
    // refusing to boot mail, voice, calendar and runs over it would trade a stuck repository for a
    // stuck machine.
    match vcs::reconcile_interrupted(&pool).await {
        Ok(released) if released > 0 => {
            tracing::warn!(
                "reconciled {released} vcs request(s) left 'running' by a previous crash -> 'interrupted'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::error!(
            %error,
            "vcs request reconciliation failed — a repository may stay queue-locked, and nothing retries before the next startup"
        ),
    }

    // After the run reconciliations, and that order is the whole of this pass's correctness: they
    // mark every run left `running` as `interrupted`, so by now no council has a live seat and
    // "still running" needs no further test to mean "abandoned". A council left that way is stuck
    // at a phase that will never advance, with nothing to advance it.
    match council::reconcile(&pool).await {
        Ok(reconciled) if reconciled > 0 => {
            tracing::warn!(
                "reconciled {reconciled} council(s) left deliberating by a previous crash -> 'error'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "council reconciliation failed"),
    }

    // After the run reconciliations above, so nothing from a previous life still counts as live.
    match worktree::reconcile_orphaned_worktrees(
        &pool,
        worktree::ORPHAN_MIN_AGE,
        worktree::GC_BACKOFF,
    )
    .await
    {
        Ok(collected) if collected > 0 => {
            tracing::warn!("collected {collected} orphaned worktree director(ies) on startup");
        }
        Ok(_) => {}
        // Hygiene, not a prerequisite: a daemon that cannot tidy up must still start.
        Err(error) => tracing::warn!(%error, "orphaned-worktree reconciliation failed"),
    }

    let token_value = match secrets::daemon_token(
        secrets::load_secret(TOKEN_KEY),
        // Only a FIRST start mints: a token already in the store is returned as it is, prefixed or
        // not, so the shell, the telegram sidecar and every hook holding it keep working.
        || auth::mint_secret("ctl"),
        |fresh| secrets::store_secret(TOKEN_KEY, fresh),
    ) {
        Ok(token) => token,
        Err(sentence) => {
            // Said on stderr as well as in the log: whoever launched a daemon that refuses to
            // start is the one person who can fix the store, and may not be reading the log.
            tracing::error!("{sentence}");
            eprintln!("{sentence}");
            std::process::exit(1);
        }
    };
    tracing::info!("nucleos-core token loaded from the system credential store");

    // This machine's settings, all under one root — see `machine_config`'s header for why it is
    // `~/.nucleos/` and no longer the directory the daemon happened to be launched from. `None` (no
    // home directory) reads every file as absent, which is what an absent file has always meant:
    // each pillar starts on its defaults.
    let machine_config_root = machine_config::root();
    match &machine_config_root {
        Some(root) => {
            migrate_machine_settings(&pool, root).await;
            migrate_project_state(&pool, root).await;
            migrate_onboarding(&pool, root).await;
        }
        None => tracing::warn!(
            "no home directory, so {} cannot be read; every pillar starts on its defaults",
            machine_config::ROOT_DISPLAY
        ),
    }
    let machine_file = |file: &str| machine_config_root.as_ref().map(|root| root.join(file));

    let models_config = machine_file(config::MODELS_CONFIG_FILE)
        .map(|path| config::load_models_config(&path))
        .unwrap_or_else(|| Ok(config::ModelsConfig::default()))
        .unwrap_or_else(|e| {
            tracing::warn!(
                "failed to parse {} ({e}), using defaults",
                config::MODELS_CONFIG_DISPLAY_PATH
            );
            config::ModelsConfig::default()
        });
    let (triage_runner, local_triage_disabled): (
        Option<Arc<dyn runner::CommandRunner>>,
        Option<String>,
    ) = if let Some(model) = models_config.local_triage_model.clone() {
        let local_runner =
            runner::OllamaRunner::new(runner::OLLAMA_BASE_URL.to_string(), model.clone());
        // The requirement's own field, not `triage::LOCAL_NUM_CTX` directly: this is the number
        // that gets enforced, and it must come FROM the declaration the role states below, not
        // sit beside it as a second copy that could drift out of step. `CAPABILITY_REQUIREMENT`
        // is itself defined from `LOCAL_NUM_CTX`, which is the link that keeps them together.
        let declared = capabilities::discover_ollama_as(
            &reqwest::Client::new(),
            runner::OLLAMA_BASE_URL,
            &model,
            triage::CAPABILITY_REQUIREMENT.context_tokens,
            "local model",
        )
        .await;
        let missing =
            capabilities::missing_capabilities(&triage::CAPABILITY_REQUIREMENT, &declared);
        match capabilities::triage_posture(&missing) {
            capabilities::Posture::Unaffected => {
                tracing::info!(%model, "local triage model enabled");
                (Some(Arc::new(local_runner)), None)
            }
            capabilities::Posture::SwitchedOff(reason) => {
                // Local triage is optional at startup: a bad probe must not take down unrelated
                // daemon services, just as a failed autostart registration does not.
                tracing::warn!(%model, %reason, "local triage model disabled");
                (None, Some(reason))
            }
            // `triage_posture` only ever returns `Unaffected` or `SwitchedOff` today —
            // `Posture` is shared across three roles, so the compiler still requires this arm.
            // `nucleos-core` is a daemon the OS autostarts; panicking here over a local triage
            // model's posture would take down email, chats, runs and the scheduler along with
            // it, which is exactly what the comment above this match says a bad probe must not
            // do. Fail closed onto the same posture as `SwitchedOff`: triage refusing to run is
            // the privacy-preserving direction, the whole reason this role switches off rather
            // than falling back like the other two roles below.
            other => {
                tracing::error!(
                    %model,
                    ?other,
                    "triage_posture returned an unexpected posture; disabling local triage"
                );
                let reason = format!("unexpected local-triage posture: {other:?}");
                (None, Some(reason))
            }
        }
    } else {
        (None, None)
    };

    let email_config_path = machine_file(machine_config::EMAIL_FILE);
    let email_config_found = email_config_path
        .as_deref()
        .is_some_and(std::path::Path::exists);
    let email_config = email_config_path
        .as_deref()
        .map(config::load_email_config)
        .unwrap_or_default();
    // Built whether or not the pillar is enabled: it is two small files, and having it always in a
    // known state means enabling email later is a config edit rather than a fresh directory.
    let triage_sandbox = dirs.data_local_dir().join("triage-sandbox");
    if let Err(error) = triage::ensure_sandbox(&triage_sandbox) {
        tracing::warn!(%error, "could not build the triage sandbox — the email pillar will stay off");
    }

    // The folder a person arranges their files in — uploads of their own, the mail they filed, and
    // now a workspace per team run. Created whether or not the email pillar is enabled, for the same
    // reason as the sandbox: a directory that always exists is one less thing to go wrong the day
    // email is switched on, and this one is reachable from its own tab with the pillar off. `None`
    // means every route under it refuses, which is the right answer when it could not be made.
    let files_root = match files::ensure_root(dirs.data_local_dir()) {
        Ok(root) => Some(root),
        Err(error) => {
            tracing::warn!(%error, "could not create the files folder — the Files tab will be unavailable");
            None
        }
    };
    // Where a delete from that folder goes instead of being destroyed — beside the root and never
    // in it (`files::trash_for`). The retention sweep runs here once, off the startup path because
    // an expired folder can be a large tree, and again on every delete; nothing else keeps a timer
    // for it. `None` means a delete refuses rather than removing for good.
    let files_trash = match files::ensure_trash(dirs.data_local_dir()) {
        Ok(trash) => {
            let sweep = trash.clone();
            tokio::task::spawn_blocking(move || {
                match files::purge(&sweep, files::TRASH_RETENTION) {
                    Ok(0) => {}
                    Ok(removed) => {
                        tracing::info!(removed, "cleared expired entries from the files trash")
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not clear expired entries from the files trash")
                    }
                }
            });
            Some(trash)
        }
        Err(error) => {
            tracing::warn!(%error, "could not create the files trash — deleting from the Files tab will be unavailable");
            None
        }
    };

    let voice_config = machine_file(machine_config::VOICE_FILE)
        .as_deref()
        .map(config::load_voice_config)
        .unwrap_or_default();
    let calendar_config = machine_file(machine_config::CALENDAR_FILE)
        .as_deref()
        .map(config::load_calendar_config)
        .unwrap_or_default();
    let web_config = machine_file(machine_config::WEB_FILE)
        .as_deref()
        .map(config::load_web_config)
        .unwrap_or_default();
    let browser_config = machine_file(machine_config::BROWSER_FILE)
        .as_deref()
        .map(config::load_browser_config)
        .unwrap_or_default();
    let telegram_config = machine_file(machine_config::TELEGRAM_FILE)
        .as_deref()
        .map(config::load_telegram_config)
        .unwrap_or_default();
    // The path is named once and reused, because two facts come off it: what the file SAYS
    // (`load_github_config`) and whether it EXISTS at all. The second is the pillar's opt-in — see
    // `GithubRuntime::configured` — and deriving it from a second lookup is how the two would come
    // to disagree about which file they mean.
    let github_path = machine_file(machine_config::GITHUB_FILE);
    let github_config = github_path
        .as_deref()
        .map(config::load_github_config)
        .unwrap_or_default();
    let github_configured = github_path.as_deref().is_some_and(std::path::Path::exists);
    // Resolved here and carried on the runtime, so the daemon and the health probe can never end up
    // asking about two different programs — the mistake `cli_probe` names when it says to use "the
    // resolved path, not the configured name".
    let github_binary = std::env::var("NUCLEOS_GH_BIN").unwrap_or_else(|_| "gh".to_owned());
    // The web sidecar's own shared secret, minted per boot and never persisted.
    //
    // NOT the control token, and not for the reason the email sidecar has its own: this traffic
    // only ever flows daemon → sidecar, so the sidecar has nothing to authenticate itself FOR. It
    // needs a secret solely to refuse anything else on the machine that can open a socket. A
    // per-boot random value is therefore strictly better than a long-lived one — there is nothing
    // to leak and nothing to rotate.
    let web_sidecar_token = auth::mint_secret("web");
    // The browser sidecar's, minted the same way and for the same reason. It matters more here: the
    // process it authenticates drives browsers holding the owner's logged-in profiles, so a secret
    // that leaked would hand those sessions to anything on the machine that can open a socket.
    let browser_sidecar_token = auth::mint_secret("browser");
    // The quota sidecar's, the same way again. Nothing of the owner's travels on this connection in
    // either direction — the request is empty and the answer is a handful of percentages — so this
    // secret exists only to keep anything else on the machine from asking the daemon's sidecar how
    // much of the owner's limit is gone.
    let quota_sidecar_token = auth::mint_secret("quota");
    // Cleanup is armed SEPARATELY from transcription, and a failed probe costs only the tidying up.
    //
    // That asymmetry is deliberate. Local triage refuses to run at all when its probe fails, because
    // there the alternative is sending mail bodies off-machine. Here the fallback is a raw transcript
    // that never leaves the laptop, so the same failure should cost the polish and not the dictation —
    // exactly what `voice::clean_up` does at runtime, decided once here at startup.
    let voice_cleanup_model = match models_config.voice_cleanup_model.clone() {
        Some(model) if voice_config.armed() => {
            // The requirement's own field, not `voice::CLEANUP_NUM_CTX` directly — see the same
            // note on the local-triage probe above; `CAPABILITY_REQUIREMENT` is defined from
            // `CLEANUP_NUM_CTX`, so the two stay linked without a second number to keep in step.
            let declared = capabilities::discover_ollama_as(
                &reqwest::Client::new(),
                runner::OLLAMA_BASE_URL,
                &model,
                voice::CAPABILITY_REQUIREMENT.context_tokens,
                "voice cleanup",
            )
            .await;
            let missing =
                capabilities::missing_capabilities(&voice::CAPABILITY_REQUIREMENT, &declared);
            // voice_posture reuses runner::local_triage_decision's own wording for a context gap —
            // a pure mapping from a context-probe result to an operator-readable reason, and every
            // message it produces talks about "the local model" rather than about mail. A second
            // copy would drift from this one.
            match capabilities::voice_posture(&missing) {
                capabilities::Posture::Unaffected => {
                    tracing::info!(%model, "voice cleanup model enabled");
                    Some(model)
                }
                capabilities::Posture::Degraded(reason) => {
                    tracing::warn!(%model, %reason, "voice cleanup disabled; transcripts will be delivered raw");
                    None
                }
                // voice_posture only ever returns Unaffected or Degraded today, for the same
                // reason the local-triage match above keeps this arm despite it: `Posture` is
                // shared across three roles. Treat an unexpected posture as Degraded rather than
                // panic — cleanup is an enhancement, and a bad probe here must cost only the
                // polish, never take the daemon down with it.
                other => {
                    tracing::error!(
                        %model,
                        ?other,
                        "voice_posture returned an unexpected posture; disabling voice cleanup"
                    );
                    None
                }
            }
        }
        _ => None,
    };
    if voice_config.armed() {
        tracing::info!("voice pillar armed");
    }

    // Which agent CLI answers a run. Ship-dark like the local triage model above: an absent or
    // unrecognised name keeps the proven Claude path, so the second runner is reachable only once an
    // operator has asked for it by name. An unrecognised name warns rather than fails startup, for
    // the same reason a bad local-model probe does — a typo in one config key must not take down
    // every unrelated daemon service.
    let claude_runner = || runner::ClaudeCliRunner {
        model: models_config.claude_model.clone(),
        plan_model: models_config.plan_model.clone(),
        review_model: models_config.review_model.clone(),
    };
    let configured_runner = models_config.primary_runner.as_deref();
    let primary_runner: Arc<dyn runner::CommandRunner> = match configured_runner {
        Some("codex") => {
            tracing::info!(model = %models_config.codex_model, "codex CLI selected as the run runner");
            Arc::new(runner::CodexCliRunner {
                model: models_config.codex_model.clone(),
                // A run keeps the user's Codex config; only chats pin a sandbox (`for_chat`).
                sandbox_mode: None,
            })
        }
        Some(other) => {
            tracing::warn!(%other, "unknown primary_runner — keeping the Claude CLI");
            Arc::new(claude_runner())
        }
        None => Arc::new(claude_runner()),
    };
    // The llm-router as an adviser. Off unless `~/.nucleos/router.yaml` turns it on, and off hands
    // back the very runner built above. Read from the same root the settings page writes and the
    // health row probes, so the three never disagree about which file is in force.
    let primary_runner = route_advice::front(
        primary_runner,
        &models_config,
        machine_file(machine_config::ROUTER_FILE)
            .map(|path| route_advice::load_config(&path))
            .unwrap_or_else(route_advice::RouterConfig::off),
    );

    // The email sidecar's own key, minted before `AppState` exists rather than beside the spawn.
    //
    // It moved here because two things now need it: the sidecar, which is handed it in its
    // environment, and `POST /email/send`, which must present the SIDECAR's key to the sidecar and
    // never the control token. Minting it twice would produce two keys and rotate the live one out
    // from under a running process (`INSERT OR REPLACE`), so it is minted once and read from
    // `state.email` by both. Gated on `enabled` so a daemon with the pillar off writes no key it
    // will never use, and a failure here is not fatal: it leaves the sidecar unstarted and the send
    // route answering 503, which is the same "off rather than half-on" posture as the rest.
    let email_sidecar_token = if email_config.enabled {
        match auth::mint_service_token(&pool, auth::Service::Email).await {
            Ok(token) => Some(token),
            Err(error) => {
                tracing::error!(
                    %error,
                    "could not mint the email sidecar's token — the sidecar will not start"
                );
                None
            }
        }
    } else {
        None
    };

    // WHICH local server answers a local turn, and whether the local route may run at all, read
    // from the file ONCE, here — above BOTH of its readers: the capability probe immediately below
    // and the assistant factory further down. One resolution and not two is the whole point. A
    // probe that picked its own dialect and a factory that read the file again could disagree about
    // which engine is configured, and the way that disagreement shows up is the local assistant
    // being reported disabled while the server the operator named answers perfectly — so both are
    // handed the same `ResolvedLocalEngine` and cannot disagree.
    //
    // A refusal — an engine name this daemon does not serve, `openai_compatible` with no address, an address
    // off this machine — DISABLES the route rather than falling back to Ollama. That fallback is
    // the worst outcome available: the operator named a server, was told nothing, and their turns
    // went somewhere else. `local_model: None` is how this crate already says "route off" —
    // `assistants::serves(Brain::Local)` refuses `RouteNotConfigured`, which `http.rs` renders as
    // `assistant::NO_LOCAL_MODEL` — so no new refusal variant is needed to say it here.
    let resolved_local_engine = match models_config.local_engine() {
        Ok(resolved) => Some(resolved),
        Err(refusal) => {
            // `error!` and not `warn!`, unlike the hosted half-setup lines below: those describe a
            // setup somebody has not finished, while this one is a line somebody WROTE that this
            // daemon will not honour, and the message names the key that repairs it.
            tracing::error!(
                reason = %refusal.message(),
                "the configured local engine is refused; chats marked local will refuse \
                 rather than answer"
            );
            None
        }
    };

    // The model that answers a chat turn asking to be answered on this machine — resolved here to
    // an `Option<String>` (was, before the assistant factory, a whole `LocalAssistant` built once)
    // and handed to `assistants::ConfiguredAssistants` below, which builds the assistant per turn.
    //
    // Probed against this feature's own window, in whichever dialect the engine resolved above
    // names: a turn accumulates its tool schemas and every result on each round, so it needs more
    // room than a single triage prompt and the probe has to say so, or a local server silently
    // truncates the middle of a conversation. Local triage and voice cleanup above still probe
    // Ollama directly — they are different roles on a different config key, and moving them is
    // not this change.
    //
    // A failed probe falls back rather than disabling, which is the opposite of local triage and for
    // a reason worth stating: triage refuses because the alternative is mail bodies leaving the
    // machine, while this turn reads only the daemon's own state, so the fallback is what already
    // happens today. Warning and carrying on is right here and would be wrong there.
    let local_model = match (
        models_config.local_assistant_model.clone(),
        resolved_local_engine.as_ref(),
    ) {
        (Some(model), Some(resolved)) => {
            // The requirement's own field, not `local_agent::TURN_NUM_CTX` directly — see the
            // same note on the local-triage probe above; `CAPABILITY_REQUIREMENT` is defined
            // from `TURN_NUM_CTX`, so the two stay linked without a second number to keep in
            // step.
            let declared = capabilities::discover_local_as(
                &reqwest::Client::new(),
                resolved.engine,
                &resolved.base_url,
                &model,
                local_agent::CAPABILITY_REQUIREMENT.context_tokens,
                resolved.declared_context_tokens,
                "local assistant",
            )
            .await;
            let missing =
                capabilities::missing_capabilities(&local_agent::CAPABILITY_REQUIREMENT, &declared);
            match capabilities::local_assistant_posture(&missing) {
                capabilities::Posture::Unaffected => {
                    tracing::info!(%model, "local assistant enabled for chat turns that ask for it");
                    Some(model)
                }
                capabilities::Posture::FellBackToCli(reason) => {
                    tracing::warn!(%model, %reason, "local assistant disabled; chat turns stay on the CLI");
                    None
                }
                // local_assistant_posture only ever returns Unaffected or FellBackToCli today,
                // for the same reason the two matches above keep this arm despite it: `Posture`
                // is shared across three roles. Treat an unexpected posture as FellBackToCli
                // rather than panic — the cloud CLI already exists as this role's fallback, so
                // failing closed here costs nothing this feature does not already accept
                // elsewhere.
                other => {
                    tracing::error!(
                        %model,
                        ?other,
                        "local_assistant_posture returned an unexpected posture; falling back to the CLI"
                    );
                    None
                }
            }
        }
        // No model named, or an engine this daemon refused: either way there is nothing to probe
        // and nothing to enable, and the refusal was already said out loud where it was read.
        _ => None,
    };

    // The hosted model (and key) that answer a chat turn asking to be answered over OpenRouter —
    // `local_model`'s sibling and the same ship-dark posture: a daemon that has configured neither
    // `hosted_assistant_model` nor an OpenRouter key behaves exactly as one that has never heard of
    // this feature, silently, the same way `local_model` above stays `None` when
    // `local_assistant_model` alone is absent.
    //
    // No probe, unlike `local_model`: that probe exists because Ollama silently truncates a prompt
    // that does not fit its context window, and this daemon is the one that has to know the window
    // before it happens. OpenRouter states each model's window in its own catalogue and this call
    // never claims one of its own, so there is nothing here for a probe to catch.
    let (hosted_model, hosted_key) = match (
        models_config.hosted_assistant_model.clone(),
        secrets::load_secret(OPENROUTER_KEY),
    ) {
        (Some(model), Ok(Some(key))) => {
            tracing::info!(%model, "hosted assistant enabled for chat turns marked openrouter");
            (Some(model), Some(key))
        }
        // A model named but no key stored: an operator part-way through turning this on. Worth a
        // line, unlike the fully-unconfigured case below, because there is now a `chats` row this
        // CAN reach (`assistant::NO_HOSTED_MODEL`) and somebody watching the log should know why it
        // refuses.
        (Some(model), Ok(None)) => {
            tracing::info!(
                %model,
                "hosted_assistant_model is configured but no OpenRouter key is stored; \
                 chats marked openrouter will refuse rather than answer"
            );
            (Some(model), None)
        }
        (Some(model), Err(error)) => {
            tracing::warn!(
                %model,
                %error,
                "could not read the OpenRouter key from the system credential store; \
                 chats marked openrouter will refuse rather than answer"
            );
            (Some(model), None)
        }
        // A key stored with no model named: the other half of an unfinished setup, and just as
        // worth a line — otherwise the only sign anything is wrong is a refusal nobody can explain.
        (None, Ok(Some(key))) => {
            tracing::info!(
                "an OpenRouter key is stored but hosted_assistant_model is not configured; \
                 chats marked openrouter will refuse rather than answer"
            );
            (None, Some(key))
        }
        // Neither configured: the ship-dark default, and silent for the same reason
        // `local_model`'s own equivalent arm is — an untouched install must look exactly like
        // one that predates this feature.
        (None, Ok(None)) => (None, None),
        (None, Err(_)) => (None, None),
    };

    // Model discovery keys: the credential store first, the environment second. Only whether each
    // is present is ever logged. A key stored while the daemon runs takes effect after a restart.
    {
        let key = |name: &str, env: &str| {
            secrets::load_secret(name)
                .ok()
                .flatten()
                .or_else(|| std::env::var(env).ok())
                .filter(|k| !k.trim().is_empty())
        };
        let keys = model_catalog::Keys {
            anthropic: key("anthropic-api-key", "ANTHROPIC_API_KEY"),
            openai: key("openai-api-key", "OPENAI_API_KEY"),
        };
        tracing::info!(
            anthropic = keys.anthropic.is_some(),
            openai = keys.openai.is_some(),
            "model discovery keys"
        );
        model_catalog::install_keys(keys);
    }

    // Read after the local model has been probed, because whether a `kind: local` seat is runnable
    // is not something a config file can assert — startup PROVES it, and a roster naming a local
    // seat this daemon cannot answer with is refused rather than quietly re-routed to the cloud.
    let council_config = match council::config_path() {
        Some(path) => config::load_council_config(&path, local_model.is_some()),
        // No home directory means there is nowhere a roster could be, so there is no council —
        // and that is all it means. Warned rather than fatal, like every other missing-pillar
        // path here: a daemon that will not boot costs the operator mail, autopilot and the API
        // over a feature that ships off.
        None => {
            tracing::warn!(
                "no home directory, so {} cannot be read; the council stays off",
                council::CONFIG_DISPLAY_PATH
            );
            None
        }
    };
    // Minted only when there is a council to use it, and never the control token: a seat is an
    // agent CLI deciding what to call next, and `auth::COUNCIL_ROUTES` is what it can reach. A
    // failure to mint leaves `None`, and `council::start` refuses — a seat with a key that
    // authenticates nothing is a council that costs money to answer badly.
    let council_token = match council_config.as_ref() {
        Some(_) => match auth::mint_service_token(&pool, auth::Service::Council).await {
            Ok(token) => Some(token),
            Err(error) => {
                tracing::error!(%error, "could not mint the council's key; the council stays off");
                None
            }
        },
        None => None,
    };

    // The factory reads the SAME resolution the probe above read — resolved once, beside the probe,
    // and destructured here. Reading `local_engine()` a second time would be a second decision, and
    // two decisions about which engine is configured are two decisions that can disagree; this pair
    // cannot, because there is only one of it.
    let (local_model, local_base_url, local_engine, local_context_tokens) =
        match resolved_local_engine {
            Some(resolved) => (
                local_model,
                resolved.base_url,
                resolved.engine,
                resolved.declared_context_tokens,
            ),
            // Inert values beside a `local_model` of `None`: with the route off, nothing ever
            // reads the address or the engine. Today's constant and today's engine, so that if
            // anything ever does read them it reads the daemon's own default rather than half of
            // the configuration that was just refused. `local_model` is stated `None` here rather
            // than left to the probe having skipped: the route being off is what the factory is
            // handed, never something it has to infer from a branch it cannot see.
            None => (
                None,
                runner::OLLAMA_BASE_URL.to_string(),
                config::LocalEngine::Ollama,
                None,
            ),
        };

    // Assembles the assistant that answers a turn from what was just resolved above — one factory
    // in place of the two singletons `local_assistant`/`hosted_assistant` used to be. No production
    // caller until this packet; `AppState.assistants` below is the first one.
    let assistants: Arc<dyn assistants::Assistants> = Arc::new(
        assistants::ConfiguredAssistants::new(
            local_model,
            hosted_model,
            hosted_key,
            "http://127.0.0.1:8791".to_string(),
            token_value.clone(),
            pool.clone(),
            // The RESOLVED address, not `runner::OLLAMA_BASE_URL` directly. Identical for an
            // install that named no engine — `local_engine()` resolves absence to Ollama on that
            // same constant — and the whole point for one that named another server.
            local_base_url,
        )
        .with_local_engine(local_engine, local_context_tokens)
        .with_agent_clis(Arc::new(claude_runner()), models_config.codex_model.clone()),
    );

    let state = AppState {
        token: Token(token_value),
        pool,
        runner: primary_runner,
        triage_runner,
        local_triage_disabled,
        assistants,
        files_root,
        files_trash,
        // `None` when this machine has no home directory to hang a library off. Resolved here and
        // not per request, like `files_root` above: it is a fact about the machine.
        workflow_library: seeded_library(),
        secrets: std::sync::Arc::new(secrets::OsCredentialStore),
        // The root every file above was read from, so the settings page writes the same files the
        // daemon reads. `None` was already warned about where it was resolved.
        machine_config_root: machine_config_root.clone(),
        telegram_doctrine: telegram_config.doctrine,
        email: Arc::new(state::EmailRuntime::from_config(
            &email_config,
            triage_sandbox,
            email_sidecar_token,
        )),
        voice: Arc::new(voice::VoiceRuntime::from_config(
            &voice_config,
            voice_cleanup_model,
        )),
        calendar: Arc::new(calendar::CalendarRuntime::from_config(&calendar_config)),
        council: Arc::new(council::CouncilRuntime::new(council_config, council_token)),
        github: Arc::new(github::GithubRuntime::from_config(
            &github_config,
            github_configured,
            github_binary,
        )),
        browser: Arc::new(browser::BrowserRuntime {
            enabled: browser_config.enabled,
            client: browser_client::BrowserClient::new(
                sidecar::BROWSER_ADDR,
                browser_sidecar_token.clone(),
            ),
        }),
        web: Arc::new(web::WebRuntime {
            enabled: web_config.enabled,
            trusted_hosts: web_config.trusted_hosts.clone(),
            retain_pages_days: web_config.retain_pages_days,
            client: web_client::WebClient::new(sidecar::WEB_ADDR, web_sidecar_token.clone()),
            // The local model that reads quarantined pages. It is the same one the voice pillar
            // probed at startup — one local model, one place it is pinned — but the consequence of
            // its absence is the opposite: voice degrades to a raw transcript, and this one
            // REFUSES. A typing aid may fail soft; a barrier may not.
            quarantine_model: models_config.voice_cleanup_model.clone(),
            ollama_base_url: runner::OLLAMA_BASE_URL.to_string(),
            http: reqwest::Client::new(),
        }),
        quota: Arc::new(quota::QuotaRuntime::new(
            quota_client::QuotaClient::new(sidecar::QUOTA_ADDR, quota_sidecar_token.clone()),
            models_config.active_runner().to_string(),
        )),
        judge: Arc::new(judge::JudgeRuntime::jev()),
        run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_tails: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        progress_timeout: state::DEFAULT_PROGRESS_TIMEOUT,
        run_timeout: state::DEFAULT_RUN_TIMEOUT,
    };

    let app = http::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", daemon_client::port()))
        .await
        .unwrap();
    tracing::info!(
        "nucleos-core listening on {}",
        listener.local_addr().unwrap()
    );
    // Every sidecar below is gated on this, and the reason is not tidiness. Secrets live in
    // Windows Credential Manager, which is NOT under the data directory — so a secondary instance
    // with its own empty database still loads the real Telegram bot token and the real mail
    // password. Two supervised telegram sidecars poll one account and answer every message twice;
    // two browser or web sidecars fight over the same fixed ports. A second daemon is for
    // exercising this process's own HTTP and MCP surface, and it does that without any of them.
    let sidecars_wanted = is_primary;
    let sidecar_path = sidecar::binary(sidecar::ECHO);
    if sidecars_wanted {
        tokio::spawn(sidecar::supervise(
            sidecar::ECHO.to_string(),
            sidecar_path,
            vec![],
        ));
    }

    // The quota sidecar. Supervised beside `echo` and NOT behind a pillar switch, which is the one
    // choice here worth defending, because every other process that reaches off this machine is
    // opt-in.
    //
    // It reaches a vendor only when the owner already has that vendor's CLI signed in on this
    // machine, and it reads the credential that CLI wrote. On a machine with no Claude Code it
    // makes no outbound call at all — it answers `unmeasured` and stops. So the switch an opt-in
    // would offer is one the owner has already thrown, in the other application, and a second one
    // here would mean the notch ships dark with no settings page to light it.
    if sidecars_wanted {
        tokio::spawn(sidecar::supervise(
            sidecar::QUOTA.to_string(),
            sidecar::binary(sidecar::QUOTA),
            sidecar::quota_env(&daemon_client::daemon_url(), &quota_sidecar_token),
        ));
        tracing::info!(addr = sidecar::QUOTA_ADDR, "quota sidecar supervised");
    }

    // The browser sidecar. Started only when the pillar is on, like the web one beside it.
    //
    // Before it starts, every session this database still calls open is retired. Spec §9.1: the
    // adapter is the parent of the browsers, so a daemon restart takes every live session with it —
    // and a row saying "open" about a browser that no longer exists is worse than no row at all,
    // because the shell would offer to take the wheel of it.
    if browser_config.enabled {
        match browser::retire_open_sessions(&state.pool, &chrono::Utc::now().to_rfc3339()).await {
            Ok(retired) if retired > 0 => {
                tracing::warn!("retired {retired} browsing session(s) left open by a previous run")
            }
            Ok(_) => {}
            Err(error) => tracing::error!(%error, "could not retire open browsing sessions"),
        }

        let path = sidecar::binary(sidecar::BROWSER);
        let env = sidecar::browser_env(
            &daemon_client::daemon_url(),
            &browser_sidecar_token,
            &browser_config,
        );
        if sidecars_wanted {
            tokio::spawn(sidecar::supervise(sidecar::BROWSER.to_string(), path, env));
        }
        tracing::info!(
            max_sessions = browser_config.max_sessions,
            "browser sidecar supervised"
        );
    }

    // The web sidecar. Started only when the pillar is on: an unstarted one means `/web/*` answers
    // 502, which is the honest reading of "there is nothing to ask".
    if web_config.enabled {
        // The key is optional and its absence is not fatal — `/fetch` works without a search
        // provider, so an installation with no API key can still be handed a URL to read.
        let search_key = match secrets::load_secret(WEB_SEARCH_KEY) {
            Ok(Some(key)) => key,
            Ok(None) => {
                tracing::info!("no web search key stored; the sidecar will serve reads only");
                String::new()
            }
            Err(error) => {
                tracing::warn!(%error, "could not read the web search key; serving reads only");
                String::new()
            }
        };
        let path = sidecar::binary(sidecar::WEB);
        let env = sidecar::web_env(
            &daemon_client::daemon_url(),
            &web_sidecar_token,
            &web_config,
            &search_key,
        );
        if sidecars_wanted {
            tokio::spawn(sidecar::supervise(sidecar::WEB.to_string(), path, env));
        }
        tracing::info!(provider = %web_config.provider, "web sidecar supervised");

        // Retention. Hourly rather than on a timer tied to reads: a cache that is never read again
        // must still empty, or "30 days" means "30 days after the last time anyone looked".
        let retention_state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                match web::prune(
                    &retention_state.pool,
                    retention_state.web.retain_pages_days,
                    chrono::Utc::now(),
                )
                .await
                {
                    Ok(0) => {}
                    Ok(pruned) => tracing::info!(pruned, "web: pages past the retention window"),
                    // Best-effort, like the activity feed: a failed sweep is a full disk later, not
                    // a reason to take the daemon down now.
                    Err(error) => tracing::warn!(%error, "web: retention sweep failed"),
                }
            }
        });
    }
    match secrets::load_secret(TELEGRAM_TOKEN_KEY) {
        Ok(Some(bot_token)) => {
            let telegram_path = sidecar::binary(sidecar::TELEGRAM);
            let telegram_env =
                sidecar::telegram_env(&daemon_client::daemon_url(), &state.token.0, &bot_token);
            if sidecars_wanted {
                tokio::spawn(sidecar::supervise(
                    sidecar::TELEGRAM.to_string(),
                    telegram_path,
                    telegram_env,
                ));
            }
            tracing::info!("telegram sidecar supervised");
        }
        Ok(None) => {
            tracing::info!(
                "no telegram-token stored — telegram sidecar not started (set with --set-telegram-token)"
            );
        }
        Err(e) => {
            tracing::warn!("failed to read telegram-token from the system credential store: {e}");
        }
    }
    tokio::spawn(scheduler::run_scheduler(state.clone()));
    // Its own loop, not a step inside the scheduler's: a job pass can sit inside `run_gate` for the
    // whole gate timeout, and sharing a loop would stall every scheduled rule in the daemon behind
    // one project's test suite.
    tokio::spawn(job::run_job_loop(state.clone()));
    // AFTER `runs::reconcile_orphaned_runs`, which ran near the top of this function and is what
    // marks the abandoned subprocesses `interrupted` — the ordering this depends on, and the same
    // one `job::reconcile_orphaned_jobs` respects. Awaited rather than spawned, so the loop below
    // never meets a half-reconciled run.
    if let Err(error) = team::reconcile_orphaned_team_runs(&state).await {
        tracing::warn!(%error, "could not reconcile the team runs a previous daemon left behind");
    }
    // Its own loop again, and for this pillar's own reason rather than the job's: a team pass
    // launches up to `max_parallel` subprocesses and writes files at a cadence nothing else in the
    // house shares. It runs no gate, so the argument above does not transfer — this one stands on
    // its own.
    tokio::spawn(team::run_team_loop(state.clone()));
    tokio::spawn(team::run_workspace_gc_loop(state.clone()));
    // A third loop and not a branch in either of the two above. What DECIDES that a department
    // starts is a different question from how it runs — the same separation `scheduler.rs` has from
    // `job.rs` — and it could not have gone in `scheduler_tick` at all: that loop is per project,
    // and a department has no project, no root and no HEAD.
    tokio::spawn(team_trigger::run_team_trigger_loop(state.clone()));
    tokio::spawn(repo_trigger::run_repo_poller(state.clone()));
    tokio::spawn(worktree::run_gc(state.pool.clone()));
    // The worktree GC's counterpart inside the database. It collects the directories a finished run
    // leaves on disk; this expires the transcript it leaves in `runs`, the events it leaves in
    // `run_events`, and eventually its line in the feed. Nothing removed any of those, so all three
    // grew for as long as the daemon was ever used — and a transcript is stored twice and indexed a
    // third time, so they grew at three times the obvious rate.
    tokio::spawn(runs::run_retention_loop(state.clone()));
    tokio::spawn(vcs::run_queue_worker(
        state.pool.clone(),
        std::sync::Arc::new(git_exec::GitExecutor {
            machine_root: machine_config_root.clone(),
            ..git_exec::GitExecutor::default()
        }),
    ));
    // Its own loop and not a step inside the queue worker's, for the reason `resolver.rs` opens
    // with: the worker holds a pool and a repository lock, and starting an agent needs an
    // `AppState` and the time an agent takes. The queue escalates and lets go; this picks the
    // conflict up afterwards.
    tokio::spawn(resolver::run_resolution_loop(state.clone()));
    // Only when a local model is already configured, and reusing the triage one rather than adding
    // a key: this reads mail-derived text, which is the text that model was chosen for, and
    // `web.rs` sets the precedent of one local model pinned in one place serving more than one
    // reader. Without it the sweep simply never runs, and the observation table stays empty —
    // which is the correct behaviour for a measurement nobody asked for.
    if let Some(model) = models_config.local_triage_model.clone() {
        tokio::spawn(pii_shadow::run_sweep_loop(
            state.pool.clone(),
            runner::OLLAMA_BASE_URL.to_string(),
            model,
        ));
    }

    // Spawned whether or not the pillar is enabled: the loop also owns retention, and bodies
    // already stored do not stop needing to expire because polling was switched off.
    tokio::spawn(triage::run_triage_loop(state.clone()));

    // Unconditional for exactly the reason above, applied to dictations. Gating this on the pillar
    // being armed would FREEZE the transcript history at the moment somebody switched voice off, which
    // is the opposite of what switching it off is for — the recordings of what they said would then
    // outlive the feature that made them.
    tokio::spawn(voice::run_retention_loop(state.clone()));
    // Held notifications are reconciled at startup for the same reason orphaned runs and stranded
    // approvals are: the daemon may have been down when the meeting ended, and a queue that only
    // drains on the tick would sit there until the NEXT meeting ended instead.
    match notify::flush_due(&state.pool).await {
        Ok(delivered) if delivered > 0 => {
            tracing::warn!("delivered {delivered} notification(s) held over from a previous run")
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not flush held notifications on startup"),
    }
    tokio::spawn(notify::run_flush_loop(state.clone()));

    // The email pillar starts only after its hook barrier has been PROVEN, and the proof can only
    // be attempted once this listener is serving — the hook reaches the daemon over HTTP, and a
    // daemon that is not up yet produces `ask_daemon.py`'s fail-closed `block`, which looks like
    // success and proves nothing (spec §5.5). Hence a task that waits for `axum::serve` below
    // rather than a check inline here.
    if state.email.enabled {
        let state = state.clone();
        tokio::spawn(async move {
            match triage::verify_hook_barrier(
                &state.pool,
                &state.email.sandbox,
                &daemon_client::daemon_url(),
                &state.token.0,
            )
            .await
            {
                Ok(()) => {
                    state
                        .email
                        .armed
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!("email triage barrier verified — the pillar is armed");
                }
                // Off rather than unprotected. The pillar's whole premise is that untrusted content
                // never meets a tool, and an unproven barrier is not a barrier.
                Err(error) => {
                    tracing::error!(
                        %error,
                        "email triage barrier could not be verified — the pillar stays OFF"
                    );
                    return;
                }
            }
            // Supervision needs all three: enabled, a password, and a PROVEN barrier. Any one
            // missing leaves the sidecar unstarted and the mailbox untouched — the pillar is off
            // rather than half-on.
            match secrets::load_secret(EMAIL_PASSWORD_KEY) {
                Ok(Some(password)) => {
                    // Its own key, minted above before `AppState` was built and read back from it
                    // here so the sidecar and the send route present the same one. An absent key
                    // leaves the sidecar unstarted rather than started with the control token: this
                    // process parses MIME written by strangers, and the fallback that hands it
                    // everything is the arrangement being removed.
                    match state.email.sidecar_token.as_deref() {
                        Some(token) => {
                            let path = sidecar::binary(sidecar::EMAIL);
                            let env = sidecar::email_env(
                                &daemon_client::daemon_url(),
                                token,
                                &email_config,
                                &password,
                            );
                            if sidecars_wanted {
                                tokio::spawn(sidecar::supervise(
                                    sidecar::EMAIL.to_string(),
                                    path,
                                    env,
                                ));
                            }
                            tracing::info!("email sidecar supervised");
                        }
                        None => tracing::error!(
                            "could not mint the email sidecar's token — the sidecar will not start"
                        ),
                    }
                }
                Ok(None) => tracing::warn!(
                    "no email-imap-password stored — the email sidecar will not start (set with --set-email-password)"
                ),
                Err(error) => {
                    tracing::warn!(%error, "could not read the email password from the system credential store")
                }
            }
        });
    } else if email_config_found {
        tracing::info!(
            "email pillar disabled ({} says enabled: false)",
            machine_config::display_path(machine_config::EMAIL_FILE)
        );
    } else {
        // Not the same thing, and saying so cost a diagnosis: a daemon started from the wrong
        // directory once reported the user's config as switched off while that file sat there
        // reading `enabled: true`. The files no longer depend on where the daemon was started, but
        // "off because absent" and "off because it says so" are still two different sentences.
        tracing::info!(
            "email pillar off — {} does not exist",
            machine_config::display_path(machine_config::EMAIL_FILE)
        );
    }

    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uma equipa que nunca correu nao se desenha como uma tabela vazia.
    ///
    /// Este e o caso de hoje -- `team_runs` esta a zero -- e uma tabela de zeros lia-se como
    /// aprovacao. Nada foi aprovado: nada aconteceu.
    #[test]
    fn a_team_that_never_ran_is_drawn_as_that_and_not_as_an_empty_table() {
        let drawn = render_pressure(&serde_json::json!({
            "outcome": "never_ran",
            "team": "NucleOS",
        }));

        assert_eq!(drawn, "equipa NucleOS · nunca correu");
    }

    /// O que nao foi medido escreve-se `—`, nunca `0`.
    ///
    /// Um pico ausente e um ajuste que se recusou a existir sao as duas coisas que um desenho
    /// descuidado transforma em zeros -- e um zero aqui e uma afirmacao sobre uma janela que
    /// ninguem mediu.
    #[test]
    fn what_was_not_measured_is_drawn_as_a_dash_and_never_as_a_zero() {
        let drawn = render_pressure(&serde_json::json!({
            "outcome": "measured",
            "team": "NucleOS",
            "job": "job-1",
            "rounds": 1,
            "items_measured": 1,
            "items_without_run": 0,
            "items_without_steps": 0,
            "rollups": [{
                "round": 1,
                "agent_name": "Nucleo",
                "items": 1,
                "compacted_items": 0,
                "peak_p50": serde_json::Value::Null,
                "peak_p90": serde_json::Value::Null,
                "steps_median": 3,
                "fit": {"kind": "insufficient"},
                "verdicts": [],
            }],
        }));

        assert!(drawn.contains("job job-1"), "o job aparece: {drawn}");
        let row = drawn.lines().last().expect("a linha do rollup");
        assert!(row.contains("Nucleo"), "o agente aparece: {row}");
        // Os dois picos, as tres colunas do ajuste que nao existe, e o veredicto que nao ha. As
        // contagens que valem zero -- `comp`, `sem run` -- ficam a zero de proposito: essas foram
        // medidas.
        assert_eq!(
            row.matches('—').count(),
            6,
            "cada celula por medir sai como travessao: {row}"
        );
    }

    /// Um ajuste que existe desenha-se com o R2 ao lado.
    ///
    /// O R2 nao e decoracao: com uma variavel e tarefas heterogeneas isto e diagnostico
    /// populacional, e um arranque sem ele le-se com uma confianca que nao tem.
    #[test]
    fn a_fit_that_exists_is_drawn_with_its_r2_beside_it() {
        let drawn = render_pressure(&serde_json::json!({
            "outcome": "measured",
            "team": "NucleOS",
            "job": "job-1",
            "rounds": 1,
            "items_measured": 3,
            "items_without_run": 0,
            "items_without_steps": 0,
            "rollups": [{
                "round": 1,
                "agent_name": "Nucleo",
                "items": 3,
                "compacted_items": 1,
                "peak_p50": 120_000,
                "peak_p90": 180_000,
                "steps_median": 4,
                "fit": {"kind": "line", "intercept": 50_000.0, "slope": 1_400.0, "r2": 0.87},
                "verdicts": ["split_speciality", "trim_prompt"],
            }],
        }));

        assert!(drawn.contains("50000"), "o arranque: {drawn}");
        assert!(drawn.contains("0.87"), "o R2: {drawn}");
        assert!(
            drawn.contains("split_speciality, trim_prompt"),
            "os dois veredictos, e nao so o primeiro: {drawn}"
        );
    }

    /// `nucleos-core --land` on its own is what almost every session types, and it has to keep
    /// meaning what it has always meant. An argument that is not there is not an error.
    #[test]
    fn a_land_with_nothing_after_it_names_no_target() {
        let args = ["nucleos-core".to_owned(), "--land".to_owned()];
        assert_eq!(land_target_from(&args), None);
    }

    /// The branch is the argument straight after the flag, and what follows it is not this
    /// function's business.
    #[test]
    fn a_land_takes_the_branch_that_follows_it() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "release".to_owned(),
            "--verbose".to_owned(),
        ];
        assert_eq!(land_target_from(&args), Some("release".to_owned()));
    }

    /// The same when the branch ends the command line, which is how it is actually typed. The
    /// `get(at + 1)` is what keeps this from being an index past the end.
    #[test]
    fn a_land_takes_a_branch_that_ends_the_command_line() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "release".to_owned(),
        ];
        assert_eq!(land_target_from(&args), Some("release".to_owned()));
    }

    /// A session that wrote `--land --something` meant the flag. Reading the next flag as a
    /// branch name would send a typo to the daemon as a landing target, and the daemon would
    /// refuse it with a message about a branch nobody ever asked for.
    #[test]
    fn a_land_followed_by_another_flag_names_no_target() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "--verbose".to_owned(),
        ];
        assert_eq!(land_target_from(&args), None);
    }

    /// And a command line with no `--land` at all has no target to find, however many other
    /// arguments it carries.
    #[test]
    fn a_command_line_without_land_names_no_target() {
        let args = [
            "nucleos-core".to_owned(),
            "--pressao".to_owned(),
            "--equipa".to_owned(),
            "NucleOS".to_owned(),
        ];
        assert_eq!(land_target_from(&args), None);
    }

    /// `--land=release` is the form that used to fall past the guard and start a daemon. It reads
    /// as the same request the space-separated form does, because that is the only thing a person
    /// who typed it can have meant.
    #[test]
    fn a_land_joined_by_an_equals_names_the_branch_after_it() {
        let args = ["nucleos-core".to_owned(), "--land=release".to_owned()];
        assert_eq!(land_target_from(&args), Some("release".to_owned()));
    }

    /// The two empty forms, and the asymmetry between them is the deliberate answer rather than an
    /// oversight: `--land=` carries no value to send, while `--land ""` carries one and goes as it
    /// was typed — the daemon reads an empty target as the integration branch, which is where a
    /// bare `--land` was going anyway.
    #[test]
    fn an_empty_equals_names_no_target_and_an_empty_argument_travels_as_typed() {
        let joined = ["nucleos-core".to_owned(), "--land=".to_owned()];
        assert_eq!(land_target_from(&joined), None);

        let separate = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            String::new(),
        ];
        assert_eq!(land_target_from(&separate), Some(String::new()));
    }

    /// One `--land` is what anybody types, so the answer to two of them is worth pinning rather
    /// than leaving to whichever way the scan happens to run: the FIRST spelling is the command,
    /// and a second one is a mistake rather than an override. It holds across the two forms, which
    /// is the half a separate scan for `--land=` would have got wrong.
    #[test]
    fn the_first_land_wins_whichever_form_the_others_take() {
        let separated = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "trunk".to_owned(),
            "--land".to_owned(),
            "release".to_owned(),
        ];
        assert_eq!(land_target_from(&separated), Some("trunk".to_owned()));

        let separated_then_joined = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "trunk".to_owned(),
            "--land=release".to_owned(),
        ];
        assert_eq!(
            land_target_from(&separated_then_joined),
            Some("trunk".to_owned())
        );

        let joined_then_separated = [
            "nucleos-core".to_owned(),
            "--land=release".to_owned(),
            "--land".to_owned(),
            "trunk".to_owned(),
        ];
        assert_eq!(
            land_target_from(&joined_then_separated),
            Some("release".to_owned())
        );
    }

    /// A shell that kept the space in `--land " --verbose"` hands this one argument that is a flag
    /// wearing a space. The daemon trims before it decides what a target is, so this has to trim
    /// before it decides what a flag is — otherwise the CLI calls it a branch and the daemon calls
    /// it a flag, and the two ends of one argument disagree.
    #[test]
    fn an_argument_a_shell_padded_with_a_space_is_still_a_flag() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            " --verbose".to_owned(),
        ];
        assert_eq!(land_target_from(&args), None);
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    /// Every part of a packaging request is required: a version is written once, so a guessed
    /// folder or manifest would be a permanent mistake.
    #[test]
    fn packaging_needs_every_argument_and_takes_them_in_any_flag_order() {
        let parsed = workflow_package_args(&words(
            "nucleos-core --workflow-package dev 1.0.0 --manifest m.yaml --from /p",
        ))
        .unwrap();
        assert_eq!(parsed.0, "dev");
        assert_eq!(parsed.1, "1.0.0");
        assert_eq!(parsed.2, std::path::PathBuf::from("/p"));
        assert_eq!(parsed.3, std::path::PathBuf::from("m.yaml"));

        for line in [
            "nucleos-core --workflow-package dev --from /p --manifest m.yaml",
            "nucleos-core --workflow-package dev 1.0.0 --from /p",
            "nucleos-core --workflow-package dev 1.0.0 --from --manifest m.yaml",
        ] {
            assert!(workflow_package_args(&words(line)).is_err(), "{line}");
        }
    }

    /// A named checkout is sent as given, made absolute; no name means the working directory.
    #[test]
    fn workflow_sync_names_the_argument_or_the_working_directory() {
        let named = workflow_sync_target(&words("nucleos-core --workflow-sync some/tree")).unwrap();
        assert!(std::path::Path::new(&named).is_absolute());
        assert!(named.replace('\\', "/").ends_with("some/tree"));

        let bare = workflow_sync_target(&words("nucleos-core --workflow-sync")).unwrap();
        assert_eq!(
            std::path::PathBuf::from(bare),
            std::path::absolute(std::env::current_dir().unwrap()).unwrap()
        );
    }

    /// `--project` names which of several projects on one repository the sync is for; absent, the
    /// daemon decides alone, and a flag with no value is refused rather than read as absent.
    #[test]
    fn workflow_sync_passes_the_named_project_through() {
        let line = "nucleos-core --workflow-sync some/tree --project beta";
        assert_eq!(
            workflow_sync_project(&words(line)).unwrap().as_deref(),
            Some("beta")
        );
        assert!(
            workflow_sync_target(&words(line))
                .unwrap()
                .replace('\\', "/")
                .ends_with("some/tree")
        );

        // The flag right after `--workflow-sync` is not mistaken for the checkout, and a checkout
        // after it is still found.
        let before = "nucleos-core --workflow-sync --project beta some/tree";
        assert!(
            workflow_sync_target(&words(before))
                .unwrap()
                .replace('\\', "/")
                .ends_with("some/tree")
        );
        let bare = "nucleos-core --workflow-sync --project beta";
        assert_eq!(
            std::path::PathBuf::from(workflow_sync_target(&words(bare)).unwrap()),
            std::path::absolute(std::env::current_dir().unwrap()).unwrap()
        );
        assert_eq!(
            workflow_sync_project(&words(bare)).unwrap().as_deref(),
            Some("beta")
        );

        assert_eq!(
            workflow_sync_project(&words("nucleos-core --workflow-sync some/tree")).unwrap(),
            None
        );
        for line in [
            "nucleos-core --workflow-sync some/tree --project",
            "nucleos-core --workflow-sync --project --other",
        ] {
            assert!(workflow_sync_project(&words(line)).is_err(), "{line}");
        }
    }

    /// The daemon's ambiguity refusal is printed as the choice it is, one candidate per line with
    /// the flag that picks it; any other body is not this refusal.
    #[test]
    fn an_ambiguous_sync_is_printed_as_a_choice() {
        let body = serde_json::json!({
            "refusal": "ambiguous_project",
            "candidates": [
                { "project_id": "alpha", "project_root": "C:/repo" },
                { "project_id": "beta", "project_root": "C:/repo" },
            ],
        });
        let said = ambiguous_sync_message(&body.to_string()).unwrap();
        assert!(said.contains("alpha") && said.contains("beta"), "{said}");
        assert!(said.contains("--project"), "{said}");
        assert_eq!(
            said.lines().filter(|line| line.contains("C:/repo")).count(),
            2
        );

        assert!(ambiguous_sync_message(r#"{"refusal":"no_project"}"#).is_none());
        assert!(ambiguous_sync_message("not json").is_none());
    }
}
