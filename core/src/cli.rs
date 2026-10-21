//! Command-line subcommands that run instead of the daemon.

use super::*;

/// Reads a secret from stdin rather than from `argv`.
///
/// A Windows command line is readable by any process running as the same user
/// (`Get-CimInstance Win32_Process | select CommandLine`) and is recorded verbatim in PSReadLine's
/// plaintext history file. Passing an IMAP app password or a bot token as an argument therefore put
/// it on disk in cleartext at the exact moment the operator was securely storing it, which is the
/// one thing this path exists to avoid.
pub(super) fn read_secret_from_stdin(prompt: &str) -> Option<String> {
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

/// Um numero, ou um travessao quando nao ha nenhum.
///
/// Nada por medir escreve-se `—` e nunca `0`: zero e uma afirmacao, e sobre uma janela que ninguem
/// mediu e falsa. E a mesma razao por que a coluna nasceu NULL.
pub(super) fn or_dash(value: &serde_json::Value) -> String {
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
pub(super) fn render_pressure(report: &serde_json::Value) -> String {
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
pub(super) fn is_land_flag(arg: &str) -> bool {
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
/// was typed: `execution::land::resolve_target` reads an empty target as the project's integration branch, so
/// the ends already agree, and a refusal invented here would be a second opinion nobody asked for.
/// Both land in the same place.
///
/// **The first spelling wins when a command line carries more than one**, whichever form it is:
/// that is the one the person typed as the command, and a second is a mistake rather than an
/// override.
pub(super) fn land_target_from(args: &[String]) -> Option<String> {
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
pub(super) fn workflow_sync_target(args: &[String]) -> Result<String, String> {
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
pub(super) fn workflow_sync_project(args: &[String]) -> Result<Option<String>, String> {
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
pub(super) fn ambiguous_sync_message(body: &str) -> Option<String> {
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

/// Where `--workflow-package` takes a bundle from.
#[derive(Debug, PartialEq)]
pub(super) enum PackageFrom {
    /// A project that runs the workflow, and the manifest naming which of its files it is.
    Project {
        root: std::path::PathBuf,
        manifest: std::path::PathBuf,
    },
    /// A tree laid out as the bundle itself, `bundle.yaml` at its top.
    Source(std::path::PathBuf),
}

/// `--workflow-package <name> <version>` and then either `--from <project> --manifest
/// <bundle.yaml>` or `--source <dir>`, all required. Refused as a usage line rather than guessed:
/// packaging writes a version that is never rewritten, so a default that picked the wrong folder
/// would be permanent — and so would a mix of both sources, which is refused rather than resolved.
pub(super) fn workflow_package_args(
    args: &[String],
) -> Result<(String, String, PackageFrom), String> {
    const USAGE: &str = "usage: nucleos-core --workflow-package <name> <version> \
                         (--from <project root> --manifest <bundle.yaml> | --source <bundle dir>)";
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
    let named = |flag: &str| args.iter().any(|arg| arg == flag);
    let from = if named("--source") {
        if named("--from") || named("--manifest") {
            return Err(USAGE.to_string());
        }
        PackageFrom::Source(value_of("--source")?)
    } else {
        PackageFrom::Project {
            root: value_of("--from")?,
            manifest: value_of("--manifest")?,
        }
    };
    Ok((positional(1)?, positional(2)?, from))
}

fn print_token() {
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

async fn land() {
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
}

// `nucleos-core --verify`, run from inside a worktree: "verify this, with cover."
//
// A subcommand for the reason `--land` is one: the token lives in Credential Manager and the
// session should run one word. Unlike `--land` it WAITS, because a verdict is the whole point;
// the wait is bounded (`--verify-wait`, `NUCLEOS_VERIFY_WAIT`, an hour by default) and a ticket
// that outlives it exits 3 with its id. All the logic lives in `verify_cli`, tested against an
// in-process server; this block gathers the token, the cwd and the arguments, prints, exits.
// It never starts a daemon. Which arguments mean what is decided by `verify_cli::invocation`
// (tested there); this block only maps its answer to a print, an exit or the run.

async fn verify() -> bool {
    let verify_args: Vec<String> = std::env::args().collect();
    let verify_env_wait = std::env::var("NUCLEOS_VERIFY_WAIT").ok();
    let verify_call = verify_cli::invocation(&verify_args, verify_env_wait.as_deref());
    if !matches!(verify_call, verify_cli::Invocation::NotVerify) {
        use std::io::Write;
        let limit = match verify_call {
            verify_cli::Invocation::Help => {
                println!("{}", verify_cli::USAGE);
                return true;
            }
            verify_cli::Invocation::NeedsVerify => {
                eprintln!(
                    "not verified: --verify-* needs --verify\n\n{}",
                    verify_cli::USAGE
                );
                std::process::exit(verify_cli::EXIT_NOT_VERIFIED);
            }
            verify_cli::Invocation::BadWait(reason) => {
                eprintln!("not verified: {reason}\n\n{}", verify_cli::USAGE);
                std::process::exit(verify_cli::EXIT_NOT_VERIFIED);
            }
            verify_cli::Invocation::Run(limit) => limit,
            verify_cli::Invocation::NotVerify => unreachable!("handled by the guard above"),
        };
        let token = match secrets::load_secret(TOKEN_KEY) {
            Ok(Some(token)) => token,
            _ => {
                eprintln!(
                    "not verified: no daemon token stored yet — start the daemon once to generate one"
                );
                std::process::exit(verify_cli::EXIT_NOT_VERIFIED);
            }
        };
        let worktree = match std::env::current_dir() {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(error) => {
                eprintln!("not verified: the current directory is unreadable: {error}");
                std::process::exit(verify_cli::EXIT_NOT_VERIFIED);
            }
        };
        let client = daemon_client::DaemonClient::new(daemon_client::daemon_url(), token);
        let outcome = verify_cli::run(&client, &worktree, limit).await;
        print!("{}", outcome.stdout);
        eprint!("{}", outcome.stderr);
        // `process::exit` does not flush a buffered stdout, and a pipe is block-buffered.
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(outcome.code);
    }
    false
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

async fn pressao() {
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
}

// The door the `github-token` comes in by. `secrets.rs` has always exposed store/load/delete and
// nothing put this key there, so the pillar could be configured, enabled and credential-less with
// no way to fix it that did not involve writing a second program.
//
// Stdin and never an argument, exactly like its two neighbours: a token on a command line is in
// the shell's history and in every process listing on the machine for as long as this runs.

fn set_github_token() {
    match read_secret_from_stdin(
        "paste the GitHub token (a fine-grained PAT or a classic one), then press Enter:",
    ) {
        Some(value) => match secrets::store_secret(integrations::github::TOKEN_KEY, &value) {
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
}

fn set_email_password() {
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

fn set_openrouter_key() {
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
}

async fn mcp_tools() {
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
    // Only the worktree box may fall back to the stored daemon token (an IDE session has no
    // NUCLEOS_DAEMON_TOKEN); every other box fails as it always did.
    let client =
        match mcp_tools::client_for_box(&served, daemon_client::DaemonClient::from_env(), || {
            secrets::load_secret(TOKEN_KEY).map_err(|e| e.to_string())
        }) {
            Ok(client) => client,
            Err(e) => {
                eprintln!("mcp-tools failed: {e}");
                std::process::exit(1);
            }
        };
    if let Err(e) = mcp_tools::run_stdio(served, client).await {
        eprintln!("mcp-tools failed: {e}");
        std::process::exit(1);
    }
}

// `nucleos-core --workflow-sync [<checkout>] [--project <id>]`: put the project's pinned
// workflows into a worktree made by hand. A thin client, for `--land`'s reason: which project
// the checkout belongs to is answered from the roster, and the roster is the running daemon's
// database. The daemon also holds the kill switch this write must respect. `--project` only
// chooses among the projects the repository already matches, when there is more than one.

async fn workflow_sync() {
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
}

/// Whether the command line asks for `--agents-sync`.
pub(super) fn agents_sync_flag(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg == "--agents-sync")
}

// `nucleos-core --agents-sync`: write the installed agent packages into the harness, no daemon.
fn agents_sync() {
    let Some(root) = machine_config::root() else {
        eprintln!("this machine has no home directory, so it has no installed agents");
        std::process::exit(1);
    };
    match agents::harness::sync_installed(&root.join("agents"), &root) {
        Ok(report) => {
            for entry in &report.entries {
                println!(
                    "{}: {} ({:?})",
                    entry.package_ref.as_deref().unwrap_or("-"),
                    entry.file,
                    entry.outcome
                );
            }
            println!("{}", report.summary());
        }
        Err(error) => {
            eprintln!("agents sync failed: {error}");
            std::process::exit(1);
        }
    }
    agents_sync_codex_prefixes(&root.join("agents"));
}

// The Codex prefixes are per project: written to `<state_dir>/agent-prefix` of the workflow
// project the current directory is in, and skipped (not an error) when there is none.
fn agents_sync_codex_prefixes(agents_home: &std::path::Path) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let synced = agents::prefix::project_root_from(&cwd)
        .and_then(|root| agents::prefix::sync_for_project(agents_home, &root));
    let Some((out, report)) = synced else {
        println!("no workflow project here: Codex prefixes skipped");
        return;
    };
    for name in &report.written {
        println!("codex prefix {name}: written");
    }
    for name in &report.unchanged {
        println!("codex prefix {name}: unchanged");
    }
    for name in &report.removed {
        println!("codex prefix {name}: removed");
    }
    for (name, message) in &report.errors {
        eprintln!("codex prefix {name}: {message}");
    }
    println!(
        "codex prefixes in {}: {} written, {} unchanged, {} removed, {} failed",
        out.display(),
        report.written.len(),
        report.unchanged.len(),
        report.removed.len(),
        report.errors.len()
    );
}

// `nucleos-core --workflow-package <name> <version> --from <project> --manifest <bundle.yaml>`:
// copy a workflow out of a project into the library as a new version, and commit it there.
// `--source <dir>` in place of the last two packages a tree laid out as the bundle itself
// (`core/assets/nucleos-dev/`), generating and checking what that needs.
//
// No daemon: this reads a folder and writes `~/.nucleos/workflows/`, and neither is the
// database. A library a running daemon lists mid-copy sees no manifest yet, which it already
// treats as somebody's scratch folder (`workflows::read_bundle`), until the manifest is written
// last.

async fn workflow_package() {
    let args: Vec<String> = std::env::args().collect();
    let (name, version, from) = match workflow_package_args(&args) {
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
    let packaged = match &from {
        PackageFrom::Project { root, manifest } => {
            workflows::package::package(&library, &name, &version, root, manifest).await
        }
        PackageFrom::Source(dir) => {
            workflows::package::package_source(&library, &name, &version, dir).await
        }
    };
    match packaged {
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
}

/// What `--version` prints: the crate's version and nothing else, so a script can compare it with
/// `VERSION` byte for byte (`nucleos-core --version` in the release workflow does).
///
/// Pure, and taking the arguments, so the test can drive it without a process. `args[0]` is the
/// program and never a flag.
pub(super) fn version_output(args: &[String]) -> Option<&'static str> {
    args.iter()
        .skip(1)
        .any(|arg| arg == "--version")
        .then_some(env!("CARGO_PKG_VERSION"))
}

pub(super) async fn run_subcommand() -> bool {
    // First, and positional on purpose: an argv-mode command may itself contain `--print-token`
    // or `--version`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(rest) = crate::execution::output_filter::run::filter_run_args(&args) {
        std::process::exit(crate::execution::output_filter::run::main(rest));
    }
    // Then, before anything that could open a database, bind a port or touch the autostart
    // entry: asking for the version must start nothing.
    if let Some(version) = version_output(&args) {
        println!("{version}");
        return true;
    }
    if std::env::args().any(|a| a == "--print-token") {
        print_token();
        return true;
    }
    if std::env::args().any(|a| is_land_flag(&a)) {
        land().await;
        return true;
    }
    if verify().await {
        return true;
    }
    if std::env::args().any(|a| a == "--pressao") {
        pressao().await;
        return true;
    }
    if std::env::args().any(|a| a == "--set-github-token") {
        set_github_token();
        return true;
    }
    if std::env::args().any(|a| a == "--set-email-password") {
        set_email_password();
        return true;
    }
    if std::env::args().any(|a| a == "--set-openrouter-key") {
        set_openrouter_key();
        return true;
    }
    if std::env::args().any(|a| a == "--mcp-tools") {
        mcp_tools().await;
        return true;
    }
    if std::env::args().any(|a| a == "--workflow-sync") {
        workflow_sync().await;
        return true;
    }
    if std::env::args().any(|a| a == "--workflow-package") {
        workflow_package().await;
        return true;
    }
    if agents_sync_flag(&std::env::args().collect::<Vec<_>>()) {
        agents_sync();
        return true;
    }
    false
}
