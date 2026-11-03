#![forbid(unsafe_code)]
//! Binário do Atom: CLI bootstrap sobre o atom-core (spec cli.md, parcial v0.1).
//!
//! Modelo v0.1: tudo roda em processo (open → boot → call → shutdown), exceto
//! `serve`, que estaciona com as extensões `onStartup` ativas.
//! `shortcut: sem socket-server; CLI não conecta em daemon (só pid/stop), quando houver daemon de verdade.`

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use tokio::io::AsyncWriteExt as _;

#[derive(Parser)]
#[command(name = "atom", version, about = "Kernel do Atom (v0.1)")]
struct Cli {
    /// Perfil (ou env ATOM_PROFILE; padrão "default").
    #[arg(global = true, long, short = 'p')]
    profile: Option<String>,
    /// Diretório ATOM_HOME (ou env ATOM_HOME).
    #[arg(global = true, long)]
    home: Option<PathBuf>,
    /// Log em JSON no stderr (nível).
    #[arg(global = true, long, default_value = "warn")]
    log_level: String,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Versão do core, protocolo e plataforma.
    Version,
    /// Sobe o daemon do perfil (estaciona com onStartup ativas).
    Serve {
        /// Roda em primeiro plano (único modo v0.1).
        #[arg(long, default_value_t = true)]
        foreground: bool,
    },
    /// Para o daemon do perfil.
    Stop,
    /// Extensões instaladas, bindings e daemon.
    Status,
    /// Perfis.
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// Extensões.
    Ext {
        #[command(subcommand)]
        cmd: ExtCmd,
    },
    /// Fixa contrato@major em um pacote.
    Bind { contract: String, package: String },
    /// Remove binding de contrato@major.
    Unbind { contract: String },
    /// Config de pacote no perfil.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Secrets do perfil (valor via stdin).
    Secret {
        #[command(subcommand)]
        cmd: SecretCmd,
    },
    /// Chamada direta no bus (debug/scripting).
    Call {
        service: String,
        method: String,
        /// JSON de params, ou "-" para ler do stdin.
        params: Option<String>,
        /// Força provedor publisher/nome.
        #[arg(long)]
        target: Option<String>,
        /// Consome stream (imprime chunks `data` e o resultado final).
        #[arg(long)]
        stream: bool,
    },
    /// Executa comando contribuído por extensão (`[contributes.commands]`).
    Run {
        /// Nome do comando (ex.: `chat`).
        command: String,
        /// Argumentos repassados à extensão.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Logs (v0.1: só via serve --foreground no stderr).
    Logs { package: Option<String> },
}

#[derive(Subcommand)]
enum ProfileCmd {
    List,
    Create {
        name: String,
    },
    Delete {
        name: String,
    },
    /// Define perfil padrão em <config>/atom.toml.
    Use {
        name: String,
    },
    Export {
        name: String,
        out: PathBuf,
    },
    Import {
        file: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    Reload,
}

#[derive(Subcommand)]
enum ExtCmd {
    Install {
        /// Pacote store (v0.5) ou caminho .tar.gz.
        package: Option<String>,
        /// Instala de uma pasta local.
        #[arg(long)]
        dev: Option<PathBuf>,
    },
    List,
    Remove {
        package: String,
    },
    Restart {
        package: String,
    },
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Define secret (valor via stdin).
    Set { name: String },
    /// Remove secret.
    Delete { name: String },
    /// Lista nomes (nunca valores).
    List,
}

#[derive(Subcommand)]
enum ConfigCmd {
    Get {
        package: String,
        key: Option<String>,
    },
    Set {
        package: String,
        key: String,
        value: String,
    },
}

fn profile_name(opt: Option<&str>, home: Option<&std::path::Path>) -> String {
    if let Some(p) = opt {
        return p.to_string();
    }
    if let Ok(p) = std::env::var("ATOM_PROFILE")
        && !p.is_empty()
    {
        return p;
    }
    default_profile_from_global(home).unwrap_or_else(|| "default".to_string())
}

fn global_file(home: Option<&std::path::Path>) -> Option<PathBuf> {
    let a = atom_core::profile::AtomPaths::resolve_with(home);
    Some(a.config.join("atom.toml"))
}

fn default_profile_from_global(home: Option<&std::path::Path>) -> Option<String> {
    let f = global_file(home)?;
    let t = std::fs::read_to_string(f).ok()?;
    let v: toml::Value = t.parse().ok()?;
    v.get("default_profile")?.as_str().map(|s| s.to_string())
}

/// `default_command` de `<config>/atom.toml` (atalho `atom` → `atom run <cmd>`).
fn default_command(home: Option<&std::path::Path>) -> Option<String> {
    let f = global_file(home)?;
    let t = std::fs::read_to_string(f).ok()?;
    let v: toml::Value = t.parse().ok()?;
    v.get("default_command")?.as_str().map(|s| s.to_string())
}

async fn core_open(profile: &str, home: Option<&std::path::Path>) -> Result<atom_core::bus::Core> {
    atom_core::bus::Core::open_with(profile, home)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let home: Option<PathBuf> = cli.home.clone();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&cli.log_level)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .json()
        .init();

    let profile = profile_name(cli.profile.as_deref(), home.as_deref());
    let cmd = match cli.cmd {
        Some(c) => c,
        None => match default_command(home.as_deref()) {
            Some(c) => Cmd::Run {
                command: c,
                args: vec![],
            },
            None => {
                Cli::command().print_help()?;
                println!();
                return Ok(());
            }
        },
    };
    match cmd {
        Cmd::Version => {
            println!(
                "{}",
                serde_json::json!({
                    "core": atom_core::CORE_VERSION,
                    "protocol": atom_core::PROTOCOL,
                    "os": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                })
            );
        }
        Cmd::Serve { .. } => {
            atom_core::daemon::run(&profile, home.as_deref())
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }
        Cmd::Stop => {
            let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
            let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            if p.socket.exists() {
                match atom_core::daemon::stop_via_daemon(&p.socket).await {
                    Ok(_) => {
                        println!("parado via daemon");
                        return Ok(());
                    }
                    Err(e) => eprintln!("daemon não respondeu ({e}); tentando pid"),
                }
            }
            let pid = std::fs::read_to_string(&p.pid_file)
                .map_err(|_| anyhow::anyhow!("daemon não está rodando (sem pid_file)"))?;
            let pid: u32 = pid.trim().parse().context("pid inválido")?;
            #[cfg(unix)]
            {
                let st = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status()?;
                if !st.success() {
                    bail!("kill {pid} falhou");
                }
            }
            #[cfg(not(unix))]
            {
                let st = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string()])
                    .status()?;
                if !st.success() {
                    bail!("taskkill {pid} falhou");
                }
            }
            std::fs::remove_file(&p.pid_file).ok();
            println!("parado (pid {pid})");
        }
        Cmd::Status => {
            let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
            let paths = atom_core::profile::ProfilePaths::new(&a, &profile)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            if paths.socket.exists() {
                match atom_core::daemon::status_via_daemon(&paths.socket).await {
                    Ok(v) => {
                        println!("{v}");
                        return Ok(());
                    }
                    Err(e) => eprintln!("daemon não respondeu ({e}); lendo disco"),
                }
            }
            let core = core_open(&profile, home.as_deref()).await?;
            let lock = atom_core::pkg::load_lock(&core.paths)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let daemon = core.paths.pid_file.exists();
            println!(
                "{}",
                serde_json::json!({
                    "profile": profile,
                    "daemon": daemon,
                    "packages": lock.package.iter().map(|p| format!("{}@{}", p.name, p.version)).collect::<Vec<_>>(),
                    "bindings": core.config.bindings,
                })
            );
        }
        Cmd::Profile { cmd } => match cmd {
            ProfileCmd::List => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let list = atom_core::profile::list_profiles(&a)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                for p in list {
                    println!("{p}");
                }
            }
            ProfileCmd::Create { name } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                a.ensure().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let p = atom_core::profile::ProfilePaths::new(&a, &name)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                atom_core::profile::load_or_create(&p)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("perfil {name} pronto");
            }
            ProfileCmd::Delete { name } => {
                if name == profile {
                    bail!("não dá para apagar o perfil em uso ({name})");
                }
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let p = atom_core::profile::ProfilePaths::new(&a, &name)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !p.dir.exists() {
                    bail!("perfil {name} não existe");
                }
                std::fs::remove_dir_all(&p.dir).context("config")?;
                std::fs::remove_dir_all(&p.data_dir).ok();
                println!("perfil {name} apagado");
            }
            ProfileCmd::Use { name } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                a.ensure().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let p = atom_core::profile::ProfilePaths::new(&a, &name)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                atom_core::profile::load_or_create(&p)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let f = global_file(home.as_deref()).unwrap();
                std::fs::write(&f, format!("default_profile = {name:?}\n")).context("atom.toml")?;
                println!("perfil padrão: {name}");
            }
            ProfileCmd::Export { name, out } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let p = atom_core::profile::ProfilePaths::new(&a, &name)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let tmp = tempfile::tempdir().context("tmp")?;
                for f in [&p.profile_toml, &p.lock, &p.grants] {
                    if f.exists() {
                        std::fs::copy(f, tmp.path().join(f.file_name().unwrap()))
                            .context("copy")?;
                    }
                }
                let tar_gz = std::fs::File::create(&out).context("out")?;
                let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
                let mut tar = tar::Builder::new(enc);
                tar.append_dir_all(".", tmp.path()).context("tar")?;
                println!("exportado para {}", out.display());
            }
            ProfileCmd::Import { file, name } => {
                let gz = std::fs::File::open(&file).context("open")?;
                let dec = flate2::read::GzDecoder::new(gz);
                let mut ar = tar::Archive::new(dec);
                let tmp = tempfile::tempdir().context("tmp")?;
                ar.unpack(tmp.path()).context("unpack")?;
                let dest_name = name.unwrap_or_else(|| {
                    file.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("imported")
                        .to_string()
                });
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let p =
                    atom_core::profile::ProfilePaths::new(&a, dest_name.trim_end_matches(".tar"))
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                p.ensure().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                for f in ["profile.toml", "atom.lock", "grants.toml"] {
                    let s = tmp.path().join(f);
                    if s.exists() {
                        std::fs::copy(&s, p.dir.join(f)).context("copy")?;
                    }
                }
                println!("perfil {} importado", p.name);
            }
            ProfileCmd::Reload => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("perfil {profile} recarregado");
            }
        },
        Cmd::Ext { cmd } => match cmd {
            ExtCmd::Install { package, dev } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                if let Some(src) = dev {
                    let id = atom_core::pkg::install_dev(&core.paths, &core.data_store, &src)
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    ensure_entrypoint_binary(&src, &core, &id)?;
                    core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    println!("instalado {id}");
                } else if let Some(pkg) = package {
                    let f = PathBuf::from(&pkg);
                    if f.exists() && f.extension().map(|e| e == "gz").unwrap_or(false) {
                        let id = atom_core::pkg::install_tarball(&core.paths, &core.data_store, &f)
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        println!("instalado {id}");
                    } else {
                        bail!(
                            "loja ainda não implementada (v0.5); use --dev <pasta> ou <arquivo.tar.gz>"
                        );
                    }
                } else {
                    bail!("informe <pacote.tar.gz> ou --dev <pasta>");
                }
            }
            ExtCmd::List => {
                let core = core_open(&profile, home.as_deref()).await?;
                let lock = atom_core::pkg::load_lock(&core.paths)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                for p in &lock.package {
                    println!("{}@{} {}", p.name, p.version, p.source);
                }
            }
            ExtCmd::Remove { package } => {
                let core = core_open(&profile, home.as_deref()).await?;
                let ok = atom_core::pkg::remove(&core.paths, &package)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !ok {
                    bail!("{package} não instalado");
                }
                println!("removido {package}");
            }
            ExtCmd::Restart { package } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                core.host.stop(&package).await;
                core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                core.boot()
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("reiniciado {package}");
            }
        },
        Cmd::Bind { contract, package } => {
            let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
            let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let mut cfg = atom_core::profile::load_or_create(&p)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            cfg.bindings.insert(contract.clone(), package.clone());
            atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            println!("{contract} -> {package}");
        }
        Cmd::Unbind { contract } => {
            let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
            let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let mut cfg = atom_core::profile::load_or_create(&p)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            cfg.bindings.remove(&contract);
            atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            println!("{contract} sem binding");
        }
        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Get { package, key } => {
                let core = core_open(&profile, home.as_deref()).await?;
                let table = core.config.config.get(&package);
                match (table, key) {
                    (None, _) => println!("null"),
                    (Some(t), None) => println!(
                        "{}",
                        toml::Value::try_from(t)
                            .map(|v| {
                                serde_json::to_value(&v).unwrap_or(serde_json::Value::Null)
                            })
                            .unwrap_or(serde_json::Value::Null)
                    ),
                    (Some(t), Some(k)) => {
                        let v = t
                            .get(&k)
                            .cloned()
                            .unwrap_or(toml::Value::String(String::new()));
                        let j = serde_json::to_value(&v).unwrap_or(serde_json::Value::Null);
                        println!("{j}");
                    }
                }
            }
            ConfigCmd::Set {
                package,
                key,
                value,
            } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let mut cfg = atom_core::profile::load_or_create(&p)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let json: serde_json::Value = serde_json::from_str(&value)
                    .unwrap_or_else(|_| serde_json::Value::String(value.clone()));
                let tv =
                    toml::Value::try_from(&json).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let entry = cfg
                    .config
                    .entry(package.clone())
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
                if let toml::Value::Table(t) = entry {
                    t.insert(key.clone(), tv);
                } else {
                    bail!("config de {package} não é tabela");
                }
                atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("{package}.{key} atualizado");
            }
        },
        Cmd::Secret { cmd } => {
            let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
            a.ensure().map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            p.ensure().map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let secrets = atom_core::secrets::Secrets::new(&profile, &p.data_dir);
            match cmd {
                SecretCmd::Set { name } => {
                    let mut v = String::new();
                    use std::io::Read as _;
                    std::io::stdin().read_to_string(&mut v).context("stdin")?;
                    let v = v.trim_end_matches(['\r', '\n']).to_string();
                    secrets
                        .set(&name, &v)
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    println!("secret {name} atualizado");
                }
                SecretCmd::Delete { name } => {
                    if !secrets
                        .delete(&name)
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?
                    {
                        bail!("secret {name} não existe");
                    }
                    println!("secret {name} removido");
                }
                SecretCmd::List => {
                    for n in secrets.list().map_err(|e| anyhow::anyhow!(e.to_string()))? {
                        println!("{n}");
                    }
                }
            }
        }
        Cmd::Call {
            service,
            method,
            params,
            target,
            stream,
        } => {
            let raw = match params.as_deref() {
                None => serde_json::Value::Null,
                Some("-") => {
                    let mut s = String::new();
                    use std::io::Read as _;
                    std::io::stdin().read_to_string(&mut s).context("stdin")?;
                    if s.trim().is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::from_str(&s).context("params JSON")?
                    }
                }
                Some(s) => serde_json::from_str(s).context("params JSON")?,
            };
            if stream {
                // Stream sempre em processo (relay de socket no CLI fica para a v0.8).
                let mut core = core_open(&profile, home.as_deref()).await?;
                let (tx, mut rx_ev) =
                    tokio::sync::mpsc::unbounded_channel::<atom_core::bus::StreamEvent>();
                let cancel = Arc::new(tokio::sync::Notify::new());
                {
                    let c = cancel.clone();
                    tokio::spawn(async move {
                        let _ = tokio::signal::ctrl_c().await;
                        c.notify_one();
                    });
                }
                let drain = tokio::spawn(async move {
                    while let Some(ev) = rx_ev.recv().await {
                        if ev.method == "atom.stream.chunk" {
                            print_chunk(&ev.params);
                        }
                    }
                });
                let out = core
                    .call_stream_chan(
                        atom_core::bus::StreamCall {
                            id: atom_protocol::new_stream_id(),
                            service: service.clone(),
                            method: method.clone(),
                            params: raw,
                            target: target.clone(),
                        },
                        tx,
                        cancel,
                    )
                    .await;
                let _ = drain.await;
                match out {
                    Ok(v) => println!("{v}"),
                    Err(e) => {
                        let rpc = e.rpc("t_cli");
                        eprintln!("erro {rpc}");
                        std::process::exit(1);
                    }
                }
                core.shutdown().await;
                return Ok(());
            }
            let sock = {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                atom_core::profile::ProfilePaths::new(&a, &profile)
                    .map(|p| p.socket)
                    .unwrap_or_default()
            };
            if sock.exists() {
                match atom_core::daemon::call_via_daemon(
                    &sock,
                    &service,
                    &method,
                    raw.clone(),
                    target.as_deref(),
                )
                .await
                {
                    Ok(v) => {
                        println!("{v}");
                        return Ok(());
                    }
                    Err(e) => eprintln!("daemon não respondeu ({e}); executando em processo"),
                }
            }
            let mut core = core_open(&profile, home.as_deref()).await?;
            let out = core
                .call(&service, &method, raw, target.as_deref(), None)
                .await;
            match out {
                Ok(v) => println!("{v}"),
                Err(e) => {
                    let rpc = e.rpc("t_cli");
                    eprintln!("erro {rpc}");
                    std::process::exit(1);
                }
            }
            core.shutdown().await;
        }
        Cmd::Run { command, args } => {
            run_command(&profile, home.as_deref(), &command, &args).await?;
        }
        Cmd::Logs { .. } => {
            eprintln!(
                "logs persistentes ainda não existem na v0.1; rode `atom serve` e veja o stderr"
            );
        }
    }
    Ok(())
}

/// Imprime `data` de um `atom.stream.chunk` (texto cru, JSON senão).
fn print_chunk(params: &serde_json::Value) {
    let data = params.get("data").unwrap_or(&serde_json::Value::Null);
    if let Some(s) = data.as_str() {
        print!("{s}");
    } else if let Some(o) = data.as_object() {
        if let Some(t) = o.get("delta_text").and_then(|v| v.as_str()) {
            print!("{t}");
        } else {
            print!("{data}");
        }
    } else if !data.is_null() {
        print!("{data}");
    }
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
}

/// `atom run <comando>`: inicia o contribuidor e, se interativo, liga o
/// terminal ao `io` socket da extensão até `command.run` responder.
/// `shortcut: sempre em processo (sem daemon); Ctrl+C cancela a troca atual, o segundo sai.`
async fn run_command(
    profile: &str,
    home: Option<&std::path::Path>,
    command: &str,
    args: &[String],
) -> Result<()> {
    let mut core = core_open(profile, home).await?;
    let (pkg, interactive) = core.find_command(command).ok_or_else(|| {
        anyhow::anyhow!("comando `{command}` não encontrado (nenhum pacote contribui)")
    })?;
    core.start_package(&pkg)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let base = serde_json::json!({"name": command, "args": args});
    if !interactive {
        let mut params = base;
        params["tty"] = serde_json::json!(false);
        let out = core
            .request_pumped(&pkg, "command.run", params)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        println!("{out}");
        core.shutdown().await;
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        let _ = (pkg, base);
        core.shutdown().await;
        anyhow::bail!("`run` interativo exige unix na v0.2");
    }
    #[cfg(unix)]
    {
        let sock = core
            .paths
            .socket
            .with_file_name(format!("atom-run-{command}-{}.sock", std::process::id()));
        if sock.exists() {
            std::fs::remove_file(&sock).ok();
        }
        let listener = tokio::net::UnixListener::bind(&sock).context("io socket")?;
        let mut params = base;
        params["tty"] = serde_json::json!(true);
        params["io"] = serde_json::Value::String(sock.to_string_lossy().to_string());
        let mut rx = core
            .host
            .post(&pkg, "command.run", params)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // Uma conexão: a extensão reconecta? Não — EOF/Ctrl+C encerram (a extensão
        // cancela o stream ativo e responde `command.run` antes de sair).
        let (stream, _) = listener.accept().await.context("accept")?;
        let (mut r, mut w) = stream.into_split();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut stdin_eof = false;
        let outcome: Result<serde_json::Value, String> = loop {
            tokio::select! {
                n = tokio::io::copy(&mut stdin, &mut w), if !stdin_eof => {
                    // EOF do terminal/pipe: half-close; a extensão consome o resto e sai.
                    let _ = n;
                    stdin_eof = true;
                    let _ = w.shutdown().await;
                }
                _ = tokio::io::copy(&mut r, &mut stdout) => {
                    // Extensão fechou: usa a resposta se já chegou, senão espera.
                    match rx.try_recv() {
                        Ok(done) => break end_of_command(Ok(done)),
                        _ => match rx.await {
                            Ok(done) => break end_of_command(Ok(done)),
                            Err(e) => break end_of_command(Err(e)),
                        },
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("(interrompido — cancelando)");
                    drop(r);
                    drop(w);
                    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
                        Ok(done) => break end_of_command(done),
                        Err(_) => break Err("tempo esgotado após Ctrl+C".to_string()),
                    }
                }
                inc = core.host.next_incoming() => {
                    // Bomba do bus: o comando pode fazer `atom.call` aninhados.
                    if let Some(i) = inc {
                        core.pump_one(i).await;
                    }
                }
                done = &mut rx, if stdin_eof => break end_of_command(done),
            }
        };
        std::fs::remove_file(&sock).ok();
        match outcome {
            Ok(v) => {
                if !v.is_null() {
                    println!("{v}");
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        core.shutdown().await;
        Ok(())
    }
}

/// Converte o fim de `command.run` em resultado imprimível.
fn end_of_command(
    r: Result<atom_protocol::RpcResponse, tokio::sync::oneshot::error::RecvError>,
) -> Result<serde_json::Value, String> {
    let resp = r.map_err(|_| "extensão encerrou a conexão".to_string())?;
    if let Some(e) = resp.error {
        return Err(format!("erro {} ({})", e.message, e.code));
    }
    Ok(resp.result.unwrap_or(serde_json::Value::Null))
}
///
/// Crate Rust sem binário copiado: compila (`cargo build --manifest-path`) e
/// copia o binário para o caminho do entrypoint. Sem isso o aceite da v0.1
/// (`install --dev testdata/echo` + `call`) falha.
fn ensure_entrypoint_binary(
    src: &std::path::Path,
    core: &atom_core::bus::Core,
    id: &str,
) -> Result<()> {
    let name = id.split('@').next().unwrap_or(id);
    let lock =
        atom_core::pkg::load_lock(&core.paths).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let lp = lock.find(name).context("pacote não está no lock")?;
    let dir = atom_core::pkg::package_dir(&core.data_store, lp)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let m = atom_core::pkg::read_manifest(&dir).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let ep = match m.entrypoint {
        Some(e) => e,
        None => return Ok(()),
    };
    if dir.join(&ep).exists() {
        return Ok(());
    }
    if !src.join("Cargo.toml").exists() {
        bail!("entrypoint {ep} não encontrado em {}", dir.display());
    }
    let stem = PathBuf::from(&ep)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    #[cfg(windows)]
    let candidates = vec![
        PathBuf::from("target/debug").join(&stem),
        src.join("target/debug").join(&stem),
        PathBuf::from("target/debug").join(format!("{stem}.exe")),
    ];
    #[cfg(not(windows))]
    let candidates = vec![
        PathBuf::from("target/debug").join(&stem),
        src.join("target/debug").join(&stem),
    ];
    if !candidates.iter().any(|c| c.exists()) {
        let st = std::process::Command::new("cargo")
            .args([
                "build",
                "--manifest-path",
                &src.join("Cargo.toml").display().to_string(),
            ])
            .status()
            .context("cargo build")?;
        if !st.success() {
            bail!("cargo build de {} falhou", src.display());
        }
    }
    let bin = candidates
        .into_iter()
        .find(|c| c.exists())
        .context(format!("binário {stem} não gerado pelo cargo build"))?;
    let dst = dir.join(&ep);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).context("bin dir")?;
    }
    std::fs::copy(&bin, &dst).context("copy bin")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dst).context("meta")?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dst, perms).context("chmod")?;
    }
    Ok(())
}
