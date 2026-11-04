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
        /// Expõe o bus via WebSocket (v0.8), ex. `--ws 127.0.0.1:0`.
        /// Token bearer em `<data>/profiles/<perfil>/serve.token` (0600).
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:0")]
        ws: Option<String>,
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
    /// Sources `store/v1` do perfil (spec store §6).
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// Aprovações de capabilities (spec capabilities §3).
    Grants {
        #[command(subcommand)]
        cmd: GrantsCmd,
    },
    /// Auto-update do core e das extensões (spec updates.md, v0.7).
    Update {
        /// Só checa e lista (não aplica).
        #[arg(long)]
        check: bool,
        /// Override da base de releases (`<url>/v1/core/<canal>.json`).
        #[arg(long)]
        url: Option<String>,
        /// Chave raiz de release (`ed25519:…`, repita p/ várias).
        #[arg(long)]
        key: Vec<String>,
        /// Canal (`stable`, `beta`, `nightly`).
        #[arg(long, default_value = "stable")]
        channel: String,
    },
    /// Manutenção da store local.
    Store {
        #[command(subcommand)]
        cmd: StoreCmd,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    List,
    Create {
        name: String,
        /// Template `kind=distribution`: pasta local com `atom.toml` ou
        /// `publisher/nome` já instalado (loja real é v0.5).
        #[arg(long)]
        from: Option<String>,
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
        /// `nome[@req]`, caminho .tar.gz ou pasta (com --dev).
        package: Option<String>,
        /// Instala de uma pasta local.
        #[arg(long)]
        dev: Option<PathBuf>,
        /// Source específica (padrão: prioridade).
        #[arg(long)]
        source: Option<String>,
        /// Força refresh dos índices antes de resolver.
        #[arg(long)]
        refresh: bool,
    },
    List,
    Remove {
        package: String,
    },
    Restart {
        package: String,
    },
    /// Busca no catalog das sources (`store/v1` §3).
    Search {
        query: String,
        /// Filtra por kind.
        #[arg(long)]
        kind: Option<String>,
    },
    /// Versões e artefatos de um pacote no índice.
    Info {
        package: String,
    },
    /// Atualiza pacote(s) da loja p/ a maior versão do mesmo req.
    Update {
        package: Option<String>,
    },
    /// Restaura o `atom.lock` anterior mais recente.
    Rollback,
    /// Gera par ed25519 p/ assinar pacotes/loja (spec store §4).
    Keygen {
        /// Arquivo da chave privada (criado 0600).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Empacota pasta → tar.gz + sig + snippet do índice.
    Pack {
        dir: PathBuf,
        /// Chave privada do publisher.
        #[arg(long)]
        key: PathBuf,
        /// Plataforma (`linux-amd64`, `any`, …; padrão: atual se executável).
        #[arg(long)]
        platform: Option<String>,
        /// Pasta de saída (padrão: atual).
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
    /// Publica snippets no layout `store/v1` (+ assina índices e catalog).
    Publish {
        /// Snippets `.index.json` do `pack`.
        snippets: Vec<PathBuf>,
        /// Raiz `v1` da loja (cria layout se nova).
        #[arg(long)]
        store_dir: PathBuf,
        /// Chave privada da loja (assina índices/catalog).
        #[arg(long)]
        store_key: PathBuf,
        /// Pasta com os artefatos (padrão: ao lado dos snippets).
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
        /// Reassina índices existentes + regen catalog (rotação de chave).
        #[arg(long)]
        reindex: bool,
        /// Só verifica (sig, sha, manifesto); não escreve.
        #[arg(long)]
        check_only: bool,
        /// Imprime próximos passos do fluxo GitHub (PR + Release).
        #[arg(long)]
        github: bool,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    /// Lista sources do perfil (ordem de prioridade).
    List,
    /// Adiciona source `store/v1` (file:// ou https://).
    Add {
        name: String,
        url: String,
        /// Chaves raiz `ed25519:BASE64,…` (confiança fixada pelo usuário).
        #[arg(long)]
        keys: Vec<String>,
        /// Menor vence (padrão 0).
        #[arg(long, default_value_t = 0)]
        priority: i32,
    },
    /// Remove source do perfil.
    Remove { name: String },
}

#[derive(Subcommand)]
enum StoreCmd {
    /// Remove da store versões sem referência em nenhum lock há +30 dias.
    Gc {
        /// Só lista o que removeria.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum GrantsCmd {
    /// Lista grants por pacote (+ pedidos pendentes de `atom.grants.request`).
    List,
    /// Concede capabilities a um pacote.
    Approve {
        package: String,
        /// Capabilities (`service:x`, `events.publish:t`, …).
        #[arg(required = true)]
        caps: Vec<String>,
    },
    /// Revoga capabilities (negação explícita; vale na hora).
    Deny {
        package: String,
        #[arg(required = true)]
        caps: Vec<String>,
    },
    /// Só os pedidos pendentes via bus.
    Pending,
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

/// Config global de update: delega ao núcleo (spec updates.md).
fn global_update_cfg(home: Option<&std::path::Path>) -> (Option<String>, Vec<String>, String) {
    atom_core::update::global_update_cfg(home)
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

/// Aprovação de capabilities na instalação (spec capabilities §3).
/// TTY = pergunta [y/N] (padrão N); sem TTY = concede com aviso explícito
/// (instalação local já é ato de aprovação; `grants deny` revoga).
fn approve_on_install(core: &mut atom_core::bus::Core, package: &str) -> Result<()> {
    let missing = core.missing_grants(package);
    if missing.is_empty() {
        return Ok(());
    }
    println!("novas capabilities de {package}: {}", missing.join(", "));
    let mut approved = false;
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        use std::io::Write as _;
        print!("Aprovar? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() {
            let t = line.trim().to_lowercase();
            approved = t == "y" || t == "yes" || t == "s" || t == "sim";
        }
    } else {
        println!("sem TTY: concedido automaticamente (revogue com `atom grants deny`)");
        approved = true;
    }
    if approved {
        core.approve_grants(package, &missing)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        println!("concedido: {}", missing.join(", "));
    } else {
        core.deny_grants(package, &missing)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        println!("negado: chamadas sem grant falham com -32003");
    }
    Ok(())
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
        Cmd::Serve { ws, .. } => {
            atom_core::daemon::run(&profile, home.as_deref(), ws.as_deref())
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
            ProfileCmd::Create { name, from } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                a.ensure().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let p = atom_core::profile::ProfilePaths::new(&a, &name)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                atom_core::profile::load_or_create(&p)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if let Some(f) = from {
                    apply_distribution(&p, &f)?;
                }
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
                core.reload_profile()
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("perfil {profile} recarregado");
            }
        },
        Cmd::Ext { cmd } => match cmd {
            ExtCmd::Install {
                package,
                dev,
                source,
                refresh,
            } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                if let Some(src) = dev {
                    let id = atom_core::pkg::install_dev(&core.paths, &core.data_store, &src)
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    ensure_entrypoint_binary(&src, &core, &id)?;
                    core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    let name = id.split('@').next().unwrap_or(&id).to_string();
                    approve_on_install(&mut core, &name)?;
                    println!("instalado {id}");
                } else if let Some(pkg) = package {
                    let f = PathBuf::from(&pkg);
                    if f.exists() && f.extension().map(|e| e == "gz").unwrap_or(false) {
                        let id = atom_core::pkg::install_tarball(&core.paths, &core.data_store, &f)
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        let name = id.split('@').next().unwrap_or(&id).to_string();
                        approve_on_install(&mut core, &name)?;
                        println!("instalado {id}");
                    } else {
                        // `nome[@req]` da loja.
                        let (name, req) = match pkg.split_once('@') {
                            Some((n, r)) => (n.to_string(), Some(r.to_string())),
                            None => (pkg, None),
                        };
                        let id = atom_core::store::install_package(
                            &mut core,
                            &name,
                            req.as_deref(),
                            source.as_deref(),
                            refresh,
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        core.ensure_kinds()
                            .await
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        let name = id.split('@').next().unwrap_or(&id).to_string();
                        approve_on_install(&mut core, &name)?;
                        println!("instalado {id}");
                    }
                } else {
                    bail!("informe <pacote[@req]>, <pacote.tar.gz> ou --dev <pasta>");
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
                let mut core = core_open(&profile, home.as_deref()).await?;
                let ok = core
                    .remove_package(&package)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if !ok {
                    bail!("{package} não instalado");
                }
                println!("removido {package}");
            }
            ExtCmd::Restart { package } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                core.reload_package(&package)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("reiniciado {package}");
            }
            ExtCmd::Search { query, kind } => {
                let core = core_open(&profile, home.as_deref()).await?;
                let hits = atom_core::store::search(&core, &query, kind.as_deref())
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                for (src, e) in hits {
                    println!(
                        "{} {} [{}] ({src}) {}",
                        e.name, e.latest, e.kind, e.description
                    );
                }
            }
            ExtCmd::Info { package } => {
                let core = core_open(&profile, home.as_deref()).await?;
                let infos = atom_core::store::info(&core, &package)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                for (src, index) in infos {
                    println!("{} (source {src})", index.name);
                    for v in &index.versions {
                        let yank = if v.yanked { " [yanked]" } else { "" };
                        let plats: Vec<&str> =
                            v.artifacts.iter().map(|a| a.platform.as_str()).collect();
                        println!(
                            "  {} kind={} engine={} platforms={}{yank}",
                            v.version,
                            v.kind,
                            v.engine,
                            plats.join(",")
                        );
                    }
                }
            }
            ExtCmd::Update { package } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                let done = atom_core::store::update_packages(&mut core, package.as_deref())
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if done.is_empty() {
                    println!("nada a atualizar");
                } else {
                    for (n, from, to) in &done {
                        println!("{n}: {from} -> {to}");
                        // Update que adiciona capabilities exige aprovação (v0.6).
                        approve_on_install(&mut core, n)?;
                        core.reload_package(n)
                            .await
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    }
                }
                core.ensure_kinds()
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            }
            ExtCmd::Rollback => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                let prev = atom_core::store::rollback_lock(&core.paths)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                core.ensure_kinds()
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("rollback p/ {prev}");
            }
            ExtCmd::Keygen { out } => {
                let (pubkey, seed) = atom_core::store::keygen();
                let path = match out {
                    Some(p) => p,
                    None => {
                        let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                        let d = a.config.join("keys");
                        std::fs::create_dir_all(&d).context("keys")?;
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        d.join(format!("ed25519-{ts}.key"))
                    }
                };
                std::fs::write(&path, format!("ed25519-seed:{seed}\n"))
                    .context("escrever chave")?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut p = std::fs::metadata(&path).context("chmod")?.permissions();
                    p.set_mode(0o600);
                    std::fs::set_permissions(&path, p).context("chmod")?;
                }
                println!("{pubkey}");
                eprintln!("privada em {} (não compartilhe)", path.display());
            }
            ExtCmd::Pack {
                dir,
                key,
                platform,
                out_dir,
            } => {
                let out = out_dir.unwrap_or_else(|| PathBuf::from("."));
                let (art, snip) =
                    atom_core::store::pack_package(&dir, platform.as_deref(), &key, &out)
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("artefato: {}", art.display());
                println!("snippet: {}", snip.display());
            }
            ExtCmd::Publish {
                snippets,
                store_dir,
                store_key,
                artifacts_dir,
                reindex,
                check_only,
                github,
            } => {
                let msg = atom_core::store::publish_to_store(
                    &store_dir,
                    &store_key,
                    &snippets,
                    artifacts_dir.as_deref(),
                    reindex,
                    check_only,
                    github,
                )
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("{msg}");
            }
        },
        Cmd::Source { cmd } => match cmd {
            SourceCmd::List => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let cfg = atom_core::profile::load_or_create(&p)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let mut srcs = cfg.sources.clone();
                srcs.sort_by(|x, y| {
                    x.priority
                        .cmp(&y.priority)
                        .then_with(|| x.name.cmp(&y.name))
                });
                for s in srcs {
                    println!(
                        "{} {} keys={} priority={}",
                        s.name,
                        s.url,
                        s.keys.len(),
                        s.priority
                    );
                }
            }
            SourceCmd::Add {
                name,
                url,
                keys,
                priority,
            } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let mut cfg = atom_core::profile::load_or_create(&p)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                cfg.sources.retain(|s| s.name != name);
                cfg.sources.push(atom_core::profile::SourceCfg {
                    name: name.clone(),
                    url: url.clone(),
                    keys,
                    priority,
                });
                atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("source {name} -> {url}");
            }
            SourceCmd::Remove { name } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let mut cfg = atom_core::profile::load_or_create(&p)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let before = cfg.sources.len();
                cfg.sources.retain(|s| s.name != name);
                if cfg.sources.len() == before {
                    bail!("source {name} não existe");
                }
                atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("source {name} removida");
            }
        },
        Cmd::Grants { cmd } => match cmd {
            GrantsCmd::List => {
                let core = core_open(&profile, home.as_deref()).await?;
                let mut names: Vec<String> = core
                    .grants()
                    .keys()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .chain(core.pending_grants().keys().cloned())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                names.sort();
                for n in names {
                    let g = core.granted_of(&n);
                    let g = if g.is_empty() {
                        "-".to_string()
                    } else {
                        g.join(",")
                    };
                    let p = core.pending_grants().get(&n).cloned().unwrap_or_default();
                    let p = if p.is_empty() {
                        String::new()
                    } else {
                        format!(" pendente:[{}]", p.join(","))
                    };
                    println!("{n} granted:[{g}]{p}");
                }
            }
            GrantsCmd::Approve { package, caps } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                core.approve_grants(&package, &caps)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("{} += {}", package, caps.join(","));
            }
            GrantsCmd::Deny { package, caps } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                core.deny_grants(&package, &caps)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("{} -= {}", package, caps.join(","));
            }
            GrantsCmd::Pending => {
                let core = core_open(&profile, home.as_deref()).await?;
                let mut names: Vec<&String> = core.pending_grants().keys().collect();
                names.sort();
                for n in names {
                    println!("{}: {}", n, core.pending_grants()[n].join(","));
                }
            }
        },
        Cmd::Update {
            check,
            url,
            key,
            channel,
        } => {
            let mut core = core_open(&profile, home.as_deref()).await?;
            // Cura marcador pendente de um boot anterior que não confirmou saúde.
            let bin = atom_core::update::bin_dir(&core);
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("atom"));
            if atom_core::update::heal_if_needed(&bin, &exe) {
                println!("rollback de update anterior aplicado na cura");
                core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
            }
            let (gurl, gkeys, gchannel) = global_update_cfg(home.as_deref());
            let base = url.or(gurl);
            let mut keys = if key.is_empty() { gkeys } else { key };
            if keys.is_empty() {
                keys = atom_core::update::EMBEDDED_RELEASE_KEYS
                    .split([',', ' ', '\n'])
                    .filter(|k| !k.is_empty())
                    .map(|k| k.to_string())
                    .collect();
            }
            let ch = if channel == "stable" {
                gchannel
            } else {
                channel
            };
            let ch = if ch.is_empty() {
                "stable".to_string()
            } else {
                ch
            };
            let rep = atom_core::update::run_update(
                &mut core,
                &atom_core::update::UpdateInputs {
                    base_url: base.as_deref(),
                    keys,
                    channel: &ch,
                    check_only: check,
                    current_exe: exe,
                    health_args: vec!["version".to_string()],
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            for m in &rep.messages {
                println!("{m}");
            }
            if let Some(v) = rep.core_updated_to {
                println!("core atualizado p/ {v} (reinicie o daemon)");
            }
            for (n, f, t) in &rep.ext_updated {
                println!("{n}: {f} -> {t}");
            }
            for (n, caps) in &rep.ext_pending_caps {
                println!(
                    "{n}: capabilities novas, aprove com `atom grants approve {n} {}`",
                    caps.join(" ")
                );
            }
            core.ensure_kinds()
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }
        Cmd::Store { cmd } => match cmd {
            StoreCmd::Gc { dry_run } => {
                let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
                // 30 dias (spec §2); dry_run só lista.
                let stats = atom_core::update::gc_store(&a, 30 * 24 * 3600)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if dry_run {
                    println!(
                        "(dry-run) removeria {} dirs ({} bytes)",
                        stats.removed_dirs, stats.bytes
                    );
                } else {
                    println!(
                        "gc: {} dirs removidos ({} bytes), {} mantidos",
                        stats.removed_dirs, stats.bytes, stats.kept_dirs
                    );
                }
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
                core.ensure_kinds()
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
            core.ensure_kinds()
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
        Cmd::Logs { package } => {
            // Journal do bus (v0.6): cauda do JSONL, filtro opcional por pacote.
            let a = atom_core::profile::AtomPaths::resolve_with(home.as_deref());
            let p = atom_core::profile::ProfilePaths::new(&a, &profile)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let f = p.data_dir.join("journal/bus.jsonl");
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let tail = if lines.len() > 50 {
                &lines[lines.len() - 50..]
            } else {
                &lines[..]
            };
            let mut shown = 0;
            for l in tail {
                if package.as_deref().is_none_or(|pkg| l.contains(pkg)) {
                    println!("{l}");
                    shown += 1;
                }
            }
            if shown == 0 {
                eprintln!("sem journal (ative com [journal] enabled=true) ou sem entradas");
            }
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
    core.ensure_kinds()
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
/// Aplica template `kind=distribution` a um perfil novo.
/// `from`: pasta local com `atom.toml` (loja real é v0.5).
/// Aplica `bindings` + `config`; `packages`/`sources` viram aviso.
fn apply_distribution(paths: &atom_core::profile::ProfilePaths, from: &str) -> Result<()> {
    let manifest_path = {
        let p = PathBuf::from(from);
        if p.is_dir() {
            p.join("atom.toml")
        } else if p.is_file() {
            p
        } else {
            bail!("distribution {from} não é pasta local (loja é v0.5)");
        }
    };
    let text = std::fs::read_to_string(&manifest_path).context("ler distribution")?;
    let m: atom_protocol::Manifest =
        atom_protocol::Manifest::parse(&text).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if m.kind != "distribution" {
        bail!("kind={} não é distribution", m.kind);
    }
    let dist = m
        .contributes
        .get("distribution")
        .context("sem [contributes.distribution]")?;
    let mut cfg =
        atom_core::profile::load_or_create(paths).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if let Some(bind) = dist.get("bindings").and_then(|v| v.as_table()) {
        for (k, v) in bind {
            if let Some(s) = v.as_str() {
                cfg.bindings.insert(k.clone(), s.to_string());
            }
        }
    }
    if let Some(conf) = dist.get("config").and_then(|v| v.as_table()) {
        for (k, v) in conf {
            cfg.config.insert(k.clone(), v.clone());
        }
    }
    atom_core::profile::save(paths, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if let Some(pkgs) = dist.get("packages") {
        eprintln!(
            "aviso: distribution lista packages ({pkgs}), loja é v0.5 — apliquei só bindings/config"
        );
    }
    Ok(())
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
