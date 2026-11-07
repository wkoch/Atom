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
    /// Saída em JSON (`ext search|info|list`, `source list`).
    #[arg(global = true, long)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[allow(clippy::large_enum_variant)]
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
    /// Eventos do bus (replay via journal; spec protocol §7).
    Events {
        #[command(subcommand)]
        cmd: EventsCmd,
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

#[allow(clippy::large_enum_variant)]
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
        /// Só prévia via `preview_updates` (sem instalar).
        #[arg(long)]
        check: bool,
    },
    /// Restaura o `atom.lock` anterior mais recente.
    Rollback {
        /// Prefixo do arquivo em lock.prev (padrão: mais recente).
        #[arg(long)]
        to: Option<String>,
    },
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
        /// Raiz `v1` da loja (cria layout se nova; ignorado com `--v2`).
        #[arg(long)]
        store_dir: Option<PathBuf>,
        /// Chave privada da loja (assina índices/catalog; ignorado com `--v2`).
        #[arg(long)]
        store_key: Option<PathBuf>,
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
        /// Publica no layout `store/v2` (TUF).
        #[arg(long)]
        v2: bool,
        /// Raiz `v2/` da loja.
        #[arg(long)]
        v2_dir: Option<PathBuf>,
        /// Chave privada do publisher (lado publisher do v2).
        #[arg(long)]
        publisher_key: Option<PathBuf>,
        /// Chaves do papel `targets` (lado loja; repita p/ limiar).
        #[arg(long)]
        targets_key: Vec<PathBuf>,
        /// Chaves do papel `snapshot` (lado loja).
        #[arg(long)]
        snapshot_key: Vec<PathBuf>,
        /// Chaves do papel `timestamp` (lado loja).
        #[arg(long)]
        timestamp_key: Vec<PathBuf>,
        /// Pasta `publishers/` com `<pub>.toml` (lado loja; padrão `<v2-dir>/../publishers`).
        #[arg(long)]
        publishers_dir: Option<PathBuf>,
        /// Só re-assina `timestamp.json` (cron diário).
        #[arg(long)]
        timestamp_only: bool,
        /// Rotaciona o root (`N+1.root.json`).
        #[arg(long)]
        rotate_root: bool,
        /// Chaves do root genesis (primeira publicação; repita p/ limiar N-de-M).
        #[arg(long)]
        root_key: Vec<PathBuf>,
        /// Limiar do papel `root` no genesis (padrão: nº de chaves).
        #[arg(long)]
        root_threshold: Option<u32>,
        /// Inicializa `1.root.json` (genesis) antes de publicar.
        #[arg(long)]
        init_root: bool,
        /// Chaves do root atual (rotação; repita p/ limiar).
        #[arg(long)]
        prev_root_key: Vec<PathBuf>,
        /// Chaves do root novo (rotação; repita p/ limiar).
        #[arg(long)]
        next_root_key: Vec<PathBuf>,
        /// Alvo extra `<rel>=<arquivo>` (p/ o canal de releases do core).
        #[arg(long)]
        extra_target: Vec<String>,
    },
}
#[derive(Subcommand)]
enum SourceCmd {
    /// Lista sources do perfil (ordem de prioridade).
    List,
    /// Adiciona source (`store/v1` file:// ou https://; `store/v2` com `--root`).
    Add {
        name: String,
        url: String,
        /// Chaves raiz `ed25519:BASE64,…` (confiança fixada pelo usuário, v1).
        #[arg(long)]
        keys: Vec<String>,
        /// Menor vence (padrão 0).
        #[arg(long, default_value_t = 0)]
        priority: i32,
        /// sha256 do `1.root.json` (loja `store/v2`, TUF; spec store-v2 §3).
        #[arg(long)]
        root: Option<String>,
    },
    /// Migra source `v1` → `v2` (mesmo nome/prioridade; exige `--root`).
    Migrate {
        name: String,
        /// sha256 do `1.root.json` da loja `v2`.
        #[arg(long)]
        root: String,
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
enum EventsCmd {
    /// Cauda de eventos retidos (ring em memória; `spec protocol §7`).
    Tail {
        /// Glob de tópico (padrão: todos).
        glob: Option<String>,
        /// Cursor de retomada `{epoch}:{offset}`; imprime lacuna explícita.
        #[arg(long, value_name = "EPOCH:OFFSET")]
        from: Option<String>,
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
    /// Aceita o `grants.toml` externo (só cliente confiável; peer recebe -32005).
    Reload,
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
    /// Edita o config do pacote em `$EDITOR` (JSON num temp; sem `$EDITOR` = erro).
    Edit { package: String },
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

/// Aprovação de capabilities na instalação (spec capabilities §3, `[C-UI-1]`).
/// TTY = pergunta [y/N] (padrão N); sem TTY = concede com aviso explícito
/// (instalação local já é ato de aprovação; `grants deny` revoga).
/// Capabilities advisory (`fs.*`, `net:`, `exec:`) são listadas à
/// parte com o aviso de não-enforcement: o core não as bloqueia.
fn approve_on_install(core: &mut atom_core::bus::Core, package: &str) -> Result<()> {
    let missing = core.missing_grants(package);
    if missing.is_empty() {
        return Ok(());
    }
    let (advisory, enforced): (Vec<&String>, Vec<&String>) =
        missing.iter().partition(|c| is_advisory_cap(c));
    println!("novas capabilities de {package}: {}", missing.join(", "));
    if !advisory.is_empty() {
        let list: Vec<&str> = advisory.iter().map(|c| c.as_str()).collect();
        println!(
            "advisory: {} (não bloqueado pelo Atom: a extensão já pode fazer isso como seu usuário. Aprove só se confia no publisher.)",
            list.join(", ")
        );
    }
    let _ = enforced;
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

/// `true` para capabilities advisory (não aplicadas pelo core; spec `[C-UI-1]`).
/// Lista: `fs.*`, `net:*`, `exec:*` (ver spec capabilities §2).
// spec: [C-UI-1]
fn is_advisory_cap(cap: &str) -> bool {
    let (name, _) = match cap.split_once(':') {
        Some((n, s)) => (n, Some(s)),
        None => (cap, None),
    };
    matches!(name, "fs" | "net" | "exec") || name.starts_with("fs.")
}

/// Flags de adulteração externa p/ `atom status` (`[C-AMB-2/3]`).
/// `grants`: `grants.toml` difere do `.core` (ignorado); `security_fields`:
/// campos de segurança de `profile.toml` no disco que divergem do core aberto.
// spec: [C-AMB-2]
// spec: [C-AMB-3]
fn status_tamper_flags(core: &atom_core::bus::Core) -> TamperFlags {
    let grants = atom_core::profile::load_grants_checked(&core.paths).tampered;
    let security_fields = match atom_core::profile::load_or_create(&core.paths) {
        Ok(on_disk) => atom_core::profile::security_fields_diff(&core.config, &on_disk),
        Err(_) => Vec::new(),
    };
    TamperFlags {
        grants,
        security_fields,
    }
}

struct TamperFlags {
    grants: bool,
    security_fields: Vec<String>,
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
            let tampered = status_tamper_flags(&core);
            if tampered.grants || !tampered.security_fields.is_empty() {
                eprintln!(
                    "aviso: alteração externa ignorada (vale o estado do core; use `atom grants reload` / `atom profile reload`): grants_tampered={} campos={:?}",
                    tampered.grants, tampered.security_fields
                );
            }
            let v1: Vec<&str> = core
                .config
                .sources
                .iter()
                .filter(|s| !atom_core::store::tuf::is_v2_source(&s.url))
                .map(|s| s.name.as_str())
                .collect();
            if !v1.is_empty() {
                // spec: [ST2-1]
                eprintln!(
                    "aviso: sources v1 (sem TUF; migre com `atom source migrate <nome> --root <sha256>`): {}",
                    v1.join(", ")
                );
            }
            println!(
                "{}",
                serde_json::json!({
                    "profile": profile,
                    "daemon": daemon,
                    "packages": lock.package.iter().map(|p| format!("{}@{}", p.name, p.version)).collect::<Vec<_>>(),
                    "bindings": core.config.bindings,
                    "tampered": {"grants": tampered.grants, "security_fields": tampered.security_fields},
                    "v1_sources": v1,
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
                    let mut core = core_open(&name, home.as_deref()).await?;
                    apply_distribution_from(&mut core, &p, &f).await?;
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
                // Daemon manda (cliente confiável via socket; peer descendente recebe
                // `-32005` do gate). Sem daemon = em-processo. `[C-AMB-3]`.
                // spec: [C-AMB-3]
                if profile_reload_via_daemon(&profile, home.as_deref()).await? {
                    println!("perfil {profile} recarregado");
                    return Ok(());
                }
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
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!(
                            lock.package
                                .iter()
                                .map(|p| serde_json::json!({
                                    "name": p.name,
                                    "version": p.version,
                                    "source": p.source,
                                }))
                                .collect::<Vec<_>>()
                        )
                    );
                } else {
                    for p in &lock.package {
                        println!("{}@{} {}", p.name, p.version, p.source);
                    }
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
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!(
                            hits.iter()
                                .map(|(src, e)| serde_json::json!({
                                    "source": src,
                                    "name": e.name,
                                    "latest": e.latest,
                                    "kind": e.kind,
                                    "description": e.description,
                                }))
                                .collect::<Vec<_>>()
                        )
                    );
                } else {
                    for (src, e) in hits {
                        println!(
                            "{} {} [{}] ({src}) {}",
                            e.name, e.latest, e.kind, e.description
                        );
                    }
                }
            }
            ExtCmd::Info { package } => {
                let core = core_open(&profile, home.as_deref()).await?;
                let infos = atom_core::store::info(&core, &package)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!(infos
                            .iter()
                            .map(|(src, index)| serde_json::json!({
                                "source": src,
                                "name": index.name,
                                "versions": index.versions.iter().map(|v| serde_json::json!({
                                    "version": v.version,
                                    "kind": v.kind,
                                    "engine": v.engine,
                                    "yanked": v.yanked,
                                    "platforms": v.artifacts.iter().map(|a| a.platform.as_str()).collect::<Vec<_>>(),
                                })).collect::<Vec<_>>(),
                            }))
                            .collect::<Vec<_>>())
                    );
                } else {
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
            }
            ExtCmd::Update { package, check } => {
                let mut core = core_open(&profile, home.as_deref()).await?;
                if check {
                    // Só prévia via `preview_updates` filtrada (sem instalar).
                    let plans = atom_core::update::preview_updates(&core)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    let plans: Vec<_> = plans
                        .into_iter()
                        .filter(|p| package.as_ref().is_none_or(|n| *n == p.name))
                        .collect();
                    if plans.is_empty() {
                        println!("nada a atualizar");
                    } else {
                        for p in &plans {
                            println!("{}: {} -> {} disponível", p.name, p.from, p.to);
                        }
                    }
                    return Ok(());
                }
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
            ExtCmd::Rollback { to } => {
                let core = core_open(&profile, home.as_deref()).await?;
                let prev = rollback_lock_to(&core.paths, to.as_deref())
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let mut core = core_open(&profile, home.as_deref()).await?;
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
                // 0600 desde a criação (revisão v1.0): sem janela legível entre escrita e chmod.
                atom_core::profile::atomic_write_private(
                    &path,
                    format!("ed25519-seed:{seed}\n").as_bytes(),
                )
                .context("escrever chave")?;
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
                v2,
                v2_dir,
                publisher_key,
                targets_key,
                snapshot_key,
                timestamp_key,
                publishers_dir,
                timestamp_only,
                rotate_root,
                root_key,
                root_threshold,
                init_root,
                prev_root_key,
                next_root_key,
                extra_target,
            } => {
                if v2 {
                    let v2_dir =
                        v2_dir.ok_or_else(|| anyhow::anyhow!("`--v2` exige `--v2-dir`"))?;
                    if check_only {
                        let msg = atom_core::store::tuf::check_repo(&v2_dir)
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        println!("{msg}");
                        return Ok(());
                    }
                    if timestamp_only {
                        let keys = load_keyfiles(&timestamp_key)?;
                        let msg = atom_core::store::tuf::refresh_timestamp_only(&v2_dir, &keys, 1)
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        println!("{msg}");
                        return Ok(());
                    }
                    if rotate_root {
                        let prev = load_keyfiles(&prev_root_key)?;
                        let next = load_keyfiles(&next_root_key)?;
                        let msg = atom_core::store::tuf::rotate_root(&v2_dir, &prev, &next)
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        println!("{msg}");
                        return Ok(());
                    }
                    if init_root {
                        let threshold = match root_threshold {
                            Some(t) => t,
                            None => load_keyfiles(&root_key)
                                .map(|k| k.len() as u32)
                                .unwrap_or(1),
                        };
                        let msg = atom_core::store::tuf::init_root(
                            &v2_dir,
                            &load_keyfiles(&root_key)?,
                            threshold,
                            &load_keyfiles(&targets_key)?,
                            &load_keyfiles(&snapshot_key)?,
                            &load_keyfiles(&timestamp_key)?,
                        )
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        println!("{msg}");
                    }
                    // Primeiro publish: genesis automático quando há `--root-key`
                    // e ainda não existe `1.root.json` (equivale a `--init-root`).
                    if !v2_dir.join("metadata/1.root.json").is_file() && !root_key.is_empty() {
                        let threshold = match root_threshold {
                            Some(t) => t,
                            None => load_keyfiles(&root_key)
                                .map(|k| k.len() as u32)
                                .unwrap_or(1),
                        };
                        let genesis = atom_core::store::tuf::init_root(
                            &v2_dir,
                            &load_keyfiles(&root_key)?,
                            threshold,
                            &load_keyfiles(&targets_key)?,
                            &load_keyfiles(&snapshot_key)?,
                            &load_keyfiles(&timestamp_key)?,
                        )
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                        println!("{genesis}");
                    }
                    let publishers_dir = publishers_dir.unwrap_or_else(|| {
                        v2_dir
                            .parent()
                            .map(|p| p.join("publishers"))
                            .unwrap_or_else(|| std::path::PathBuf::from("publishers"))
                    });
                    // Snippets + chave do publisher: lado publisher primeiro (90d).
                    let mut msg = String::new();
                    if !snippets.is_empty()
                        && let Some(pkey) = publisher_key.as_deref()
                    {
                        msg = atom_core::store::tuf::publish_delegated(
                            &v2_dir,
                            &snippets,
                            pkey,
                            artifacts_dir.as_deref(),
                            90,
                        )
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    }
                    let extra = parse_extra_targets(&extra_target)?;
                    let store_msg = atom_core::store::tuf::publish_store(
                        &v2_dir,
                        &load_keyfiles(&targets_key)?,
                        &load_keyfiles(&snapshot_key)?,
                        &load_keyfiles(&timestamp_key)?,
                        &publishers_dir,
                        &atom_core::store::tuf::ExpiryPolicy::store_default(),
                        &extra,
                    )
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    if msg.is_empty() {
                        msg = store_msg;
                    } else {
                        msg.push('\n');
                        msg.push_str(&store_msg);
                    }
                    println!("{msg}");
                    return Ok(());
                }
                let (Some(store_dir), Some(store_key)) = (store_dir, store_key) else {
                    bail!(
                        "`publish` v1 exige `--store-dir` e `--store-key` (modo `--v2` não usa esses)"
                    );
                };
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
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!(
                            srcs.iter()
                                .map(|s| serde_json::json!({
                                    "name": s.name,
                                    "url": s.url,
                                    "keys": s.keys.len(),
                                    "priority": s.priority,
                                }))
                                .collect::<Vec<_>>()
                        )
                    );
                } else {
                    for s in srcs {
                        let v2 = atom_core::store::tuf::is_v2_source(&s.url);
                        let warn = if v2 {
                            ""
                        } else {
                            " [aviso: source v1 — migre com `atom source migrate`] "
                        };
                        println!(
                            "{} {} keys={} priority={}{warn}",
                            s.name,
                            s.url,
                            s.keys.len(),
                            s.priority
                        );
                    }
                }
            }
            SourceCmd::Add {
                name,
                url,
                keys,
                priority,
                root,
            } => {
                source_add(&profile, home.as_deref(), &name, &url, keys, priority, root).await?;
            }
            SourceCmd::Migrate { name, root } => {
                source_migrate(&profile, home.as_deref(), &name, &root).await?;
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
            GrantsCmd::Reload => {
                // Daemon manda (cliente confiável via socket; peer descendente
                // recebe `-32005` do gate). Sem daemon = em-processo. `[C-AMB-2]`.
                // spec: [C-AMB-2]
                if grants_reload_via_daemon(&profile, home.as_deref()).await? {
                    println!("grants recarregados de grants.toml");
                    return Ok(());
                }
                let core = core_open(&profile, home.as_deref()).await?;
                atom_core::profile::reload_grants_from_disk(&core.paths)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                println!("grants recarregados de grants.toml");
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
            ConfigCmd::Edit { package } => {
                config_edit(&profile, home.as_deref(), &package)?;
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
                    tokio::sync::mpsc::channel::<atom_core::bus::StreamEvent>(256);
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
                            ctx: None,
                            flow: None,
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
        Cmd::Events { cmd } => match cmd {
            EventsCmd::Tail { glob, from } => {
                events_tail(&profile, home.as_deref(), glob.as_deref(), from.as_deref()).await?;
            }
        },
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

/// Chamada genérica ao daemon via socket unix (cliente confiável local).
/// Retorna `Ok(None)` se não há daemon; erro de protocolo (incl. `-32005`) propaga.
async fn daemon_rpc(
    profile: &str,
    home: Option<&std::path::Path>,
    method: &str,
) -> Result<Option<serde_json::Value>> {
    let a = atom_core::profile::AtomPaths::resolve_with(home);
    let sock = atom_core::profile::ProfilePaths::new(&a, profile)
        .map(|p| p.socket)
        .unwrap_or_default();
    if !sock.exists() {
        return Ok(None);
    }
    let stream = tokio::net::UnixStream::connect(&sock)
        .await
        .map_err(|e| anyhow::anyhow!("daemon inacessível: {e}"))?;
    let (r, mut w) = stream.into_split();
    let mut r = tokio::io::BufReader::new(r);
    let req = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}});
    let line = serde_json::to_string(&req).context("request")?;
    w.write_all(line.as_bytes()).await.context("socket")?;
    w.write_all(b"\n").await.context("socket")?;
    let mut buf = String::new();
    use tokio::io::AsyncBufReadExt as _;
    let n = r.read_line(&mut buf).await.context("socket")?;
    if n == 0 {
        anyhow::bail!("daemon fechou a conexão");
    }
    let resp: serde_json::Value = serde_json::from_str(buf.trim()).context("resposta")?;
    if let Some(err) = resp.get("error") {
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603);
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("erro do daemon");
        if code == i64::from(atom_protocol::ErrorCode::UNTRUSTED_PEER) {
            anyhow::bail!(
                "-32005 UntrustedPeer: peer descendente do daemon não é cliente confiável"
            );
        }
        anyhow::bail!("daemon: {msg} ({code})");
    }
    Ok(resp.get("result").cloned())
}

/// `atom grants reload` via daemon quando há daemon (cliente confiável no socket;
/// peer não-confiável recebe `-32005` do gate — NAMING: método `atom.grants.reload`).
/// Retorna `true` se o daemon tratou. Sem daemon = `false` (em-processo).
// spec: [C-AMB-2]
async fn grants_reload_via_daemon(profile: &str, home: Option<&std::path::Path>) -> Result<bool> {
    match daemon_rpc(profile, home, "atom.grants.reload").await? {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

/// `atom profile reload` via daemon quando há daemon (mesmo gate `-32005`).
/// Retorna `true` se o daemon tratou. Sem daemon = `false` (em-processo).
// spec: [C-AMB-3]
async fn profile_reload_via_daemon(profile: &str, home: Option<&std::path::Path>) -> Result<bool> {
    match daemon_rpc(profile, home, "atom.profile.reload").await? {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

/// Rollback p/ arquivo de `lock.prev`: `--to` = prefixo do nome; `None` = mais recente.
fn rollback_lock_to(
    paths: &atom_core::profile::ProfilePaths,
    to: Option<&str>,
) -> Result<String, atom_core::error::CoreError> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&paths.lock_prev)
        .map_err(|e| atom_core::error::CoreError::Io(e.to_string()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    let pick = match to {
        None => files.pop(),
        Some(prefix) => files.into_iter().rev().find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix))
        }),
    };
    let prev = pick.ok_or_else(|| {
        atom_core::error::CoreError::Package(match to {
            Some(t) => format!("sem rollback `{t}` em lock.prev"),
            None => "sem rollback disponível".to_string(),
        })
    })?;
    let name = prev
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string();
    let bytes = std::fs::read(&prev).map_err(|e| atom_core::error::CoreError::Io(e.to_string()))?;
    atom_core::profile::atomic_write(&paths.lock, &bytes)
        .map_err(|e| atom_core::error::CoreError::Io(e.to_string()))?;
    Ok(name)
}

/// Carrega chaves privadas (`ed25519-seed:BASE64`) de arquivos via `tuf::load_keys_file`.
fn load_keyfiles(paths: &[std::path::PathBuf]) -> Result<Vec<[u8; 32]>, anyhow::Error> {
    let mut out = vec![];
    for p in paths {
        out.extend(
            atom_core::store::tuf::load_keys_file(p).map_err(|e| anyhow::anyhow!(e.to_string()))?,
        );
    }
    Ok(out)
}

/// Parse de `--extra-target` (`<rel>=<arquivo>` → bytes).
fn parse_extra_targets(items: &[String]) -> Result<Vec<(String, Vec<u8>)>, anyhow::Error> {
    let mut out = vec![];
    for item in items {
        let (rel, file) = item.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("`--extra-target` exige `<rel>=<arquivo>` (recebido `{item}`)")
        })?;
        let bytes = std::fs::read(file).with_context(|| format!("lendo {file}"))?;
        out.push((rel.to_string(), bytes));
    }
    Ok(out)
}

/// `atom config edit <pacote>`: despeja o config em JSON num temp, abre `$EDITOR`,
/// relê e salva. Sem `$EDITOR` = erro claro.
fn config_edit(profile: &str, home: Option<&std::path::Path>, package: &str) -> Result<()> {
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("sem $EDITOR (defina EDITOR p/ `atom config edit`)"))?;
    let a = atom_core::profile::AtomPaths::resolve_with(home);
    let p = atom_core::profile::ProfilePaths::new(&a, profile)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let cfg = atom_core::profile::load_or_create(&p).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let table = cfg.config.get(package);
    let json = serde_json::to_value(table).unwrap_or(serde_json::Value::Null);
    let tmp = tempfile::NamedTempFile::with_suffix(".json").context("temp")?;
    std::fs::write(
        tmp.path(),
        serde_json::to_string_pretty(&json).context("json")?,
    )
    .context("temp")?;
    let st = std::process::Command::new(&editor)
        .arg(tmp.path())
        .status()
        .with_context(|| format!("abrindo {editor}"))?;
    anyhow::ensure!(st.success(), "{editor} falhou");
    let raw = std::fs::read_to_string(tmp.path()).context("lendo editado")?;
    let edited: serde_json::Value =
        serde_json::from_str(&raw).context("JSON inválido no editor")?;
    let mut cfg =
        atom_core::profile::load_or_create(&p).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    match edited {
        serde_json::Value::Null => {
            cfg.config.remove(package);
        }
        other => {
            let tv = toml::Value::try_from(&other).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            cfg.config.insert(package.to_string(), tv);
        }
    }
    atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("{package} atualizado via {editor}");
    Ok(())
}

/// `atom source add`: v1 grava direto; v2 (`--root`) baixa `1.root.json`,
/// confere o sha256 e grava o trust no cache antes de persistir a source.
async fn source_add(
    profile: &str,
    home: Option<&std::path::Path>,
    name: &str,
    url: &str,
    keys: Vec<String>,
    priority: i32,
    root: Option<String>,
) -> Result<()> {
    let want_v2 = root.is_some() || atom_core::store::tuf::is_v2_source(url);
    if want_v2 && root.is_none() {
        bail!("source v2 exige `--root <sha256 do 1.root.json>`");
    }
    if let Some(expected) = root.as_deref() {
        let root_bytes = fetch_root_json(url).await?;
        let actual = sha256_hex(&root_bytes);
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            bail!("sha256 do 1.root.json diverge do --root (esperado {expected}, obtido {actual})");
        }
        let a = atom_core::profile::AtomPaths::resolve_with(home);
        let tuf_dir = tuf_cache_dir(&a, name);
        std::fs::create_dir_all(&tuf_dir).context("cache tuf")?;
        atom_core::profile::atomic_write(&tuf_dir.join("1.root.json"), &root_bytes)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        atom_core::profile::atomic_write(&tuf_dir.join("trusted-root.json"), &root_bytes)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    let a = atom_core::profile::AtomPaths::resolve_with(home);
    let p = atom_core::profile::ProfilePaths::new(&a, profile)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut cfg =
        atom_core::profile::load_or_create(&p).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    cfg.sources.retain(|s| s.name != name);
    cfg.sources.push(atom_core::profile::SourceCfg {
        name: name.to_string(),
        url: url.to_string(),
        keys,
        priority,
        root_sha256: root.clone(),
    });
    atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("source {name} -> {url}");
    Ok(())
}

/// `atom source migrate`: troca a URL p/ `/v2/` mantendo nome/prioridade.
/// Pacotes instalados não mudam (lock tem sha256; spec store-v2 §6).
async fn source_migrate(
    profile: &str,
    home: Option<&std::path::Path>,
    name: &str,
    root: &str,
) -> Result<()> {
    let a = atom_core::profile::AtomPaths::resolve_with(home);
    let p = atom_core::profile::ProfilePaths::new(&a, profile)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut cfg =
        atom_core::profile::load_or_create(&p).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let idx = cfg
        .sources
        .iter()
        .position(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("source {name} não existe"))?;
    let v2_url = to_v2_url(&cfg.sources[idx].url);
    let root_bytes = fetch_root_json(&v2_url).await?;
    let actual = sha256_hex(&root_bytes);
    if !actual.eq_ignore_ascii_case(root.trim()) {
        bail!("sha256 do 1.root.json diverge do --root (esperado {root}, obtido {actual})");
    }
    let tuf_dir = tuf_cache_dir(&a, name);
    std::fs::create_dir_all(&tuf_dir).context("cache tuf")?;
    atom_core::profile::atomic_write(&tuf_dir.join("1.root.json"), &root_bytes)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    atom_core::profile::atomic_write(&tuf_dir.join("trusted-root.json"), &root_bytes)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    cfg.sources[idx].url = v2_url.clone();
    atom_core::profile::save(&p, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("source {name} migrada p/ {v2_url}");
    Ok(())
}

/// Normaliza a base p/ `/v2/` (troca sufixo `/v1/` ou acrescenta).
fn to_v2_url(url: &str) -> String {
    let t = url.trim_end_matches('/');
    if let Some(base) = t.strip_suffix("/v1") {
        return format!("{base}/v2/");
    }
    if t.ends_with("/v2") {
        return format!("{t}/");
    }
    format!("{t}/v2/")
}

/// Baixa `<base>/metadata/1.root.json` (`file://` ou https).
async fn fetch_root_json(base_url: &str) -> Result<Vec<u8>> {
    let base = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{base_url}/")
    };
    if let Some(rest) = base.strip_prefix("file://") {
        let p = std::path::PathBuf::from(rest).join("metadata/1.root.json");
        return std::fs::read(&p).with_context(|| format!("lendo {}", p.display()));
    }
    let url = format!("{base}metadata/1.root.json");
    let bytes = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .bytes()
        .await
        .context("corpo")?;
    Ok(bytes.to_vec())
}

/// `<cache>/tuf/<source>/` (nome sanitizado).
fn tuf_cache_dir(atom: &atom_core::profile::AtomPaths, source: &str) -> std::path::PathBuf {
    let safe: String = source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    atom.cache.join("tuf").join(safe)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `atom events tail [glob] [--from epoch:offset]`: replay do ring retido
/// (`atom.events.subscribe` com cursor; spec protocol §7 `[P-EV-3]`).
/// Imprime `gap` explícito quando o `epoch` diverge ou o `offset` saiu do anel.
// spec: [P-EV-3]
async fn events_tail(
    profile: &str,
    home: Option<&std::path::Path>,
    glob: Option<&str>,
    from: Option<&str>,
) -> Result<()> {
    let cursor = match from {
        Some(s) => Some(parse_cursor(s)?),
        None => None,
    };
    let core = core_open(profile, home).await?;
    let (epoch, retained) = core.retained_events();
    let pat = glob.unwrap_or("*");
    let matcher = globset::Glob::new(pat)
        .map(|g| g.compile_matcher())
        .map_err(|e| anyhow::anyhow!("glob inválido: {e}"))?;
    let mut list: Vec<&atom_core::bus::RetainedEvent> = retained
        .iter()
        .filter(|e| matcher.is_match(&e.topic))
        .collect();
    if let Some((from_epoch, from_offset)) = cursor.as_ref() {
        if from_epoch != &epoch {
            println!(
                "gap epoch={from_epoch} -> {epoch} (instância diferente; tudo após o cursor foi perdido)"
            );
        } else {
            let oldest = list.first().map(|e| e.offset);
            let newest = list.last().map(|e| e.offset);
            let out_of_ring = match (oldest, newest) {
                (Some(o), Some(_)) => *from_offset < o.saturating_sub(1),
                (None, None) => from_offset + 1 < core_event_next_hint(&core),
                _ => false,
            };
            if out_of_ring {
                println!(
                    "gap epoch={epoch} from={from_offset} (cursor fora do anel; parte perdida)"
                );
            }
        }
        list.retain(|e| e.epoch == epoch && e.offset > *from_offset);
        if list.is_empty() {
            println!("sem eventos após {from_epoch}:{from_offset} (epoch atual {epoch})");
            return Ok(());
        }
    }
    for e in list {
        println!("{} {}:{} {}", e.topic, e.epoch, e.offset, e.data);
    }
    Ok(())
}

fn parse_cursor(s: &str) -> Result<(String, u64)> {
    let (epoch, off) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("--from deve ser `{{epoch}}:{{offset}}`"))?;
    if epoch.is_empty() {
        anyhow::bail!("--from deve ser `{{epoch}}:{{offset}}`");
    }
    let offset: u64 = off
        .parse()
        .map_err(|_| anyhow::anyhow!("--from: offset deve ser inteiro"))?;
    Ok((epoch.to_string(), offset))
}

/// Dica do próximo `offset` p/ detectar anel vazio com cursor antigo.
/// Usa o maior retido + 1 (o ring vazio do boot frio indica "nada após").
fn core_event_next_hint(core: &atom_core::bus::Core) -> u64 {
    let (_, retained) = core.retained_events();
    retained.last().map(|e| e.offset + 1).unwrap_or(1)
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
        let (mut rx, _) = core
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
/// Aplica template `kind=distribution` a um perfil novo (spec profiles §7).
/// `from`: pasta/arquivo local com `atom.toml`, ou id da loja
/// (`publisher/nome[@req]`, exige `source add` prévio).
/// Aplica `sources`, instala `packages`, depois `bindings` + `config`.
async fn apply_distribution_from(
    core: &mut atom_core::bus::Core,
    paths: &atom_core::profile::ProfilePaths,
    from: &str,
) -> Result<()> {
    let m = if std::path::Path::new(from).exists() {
        let manifest_path = {
            let p = PathBuf::from(from);
            if p.is_dir() { p.join("atom.toml") } else { p }
        };
        let text = std::fs::read_to_string(&manifest_path).context("ler distribution")?;
        atom_protocol::Manifest::parse(&text).map_err(|e| anyhow::anyhow!(e.to_string()))?
    } else {
        let (dist_name, req) = match from.split_once('@') {
            Some((n, r)) => (n.to_string(), Some(r.to_string())),
            None => (from.to_string(), None),
        };
        if !dist_name.contains('/') {
            bail!("distribution {from} não é pasta local nem id publisher/nome[@req]");
        }
        let id = atom_core::store::install_package(core, &dist_name, req.as_deref(), None, false)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        core.ensure_kinds()
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let name = id.split('@').next().unwrap_or(&id).to_string();
        approve_on_install(core, &name)?;
        read_installed_manifest(core, &name)?
    };
    if m.kind != "distribution" {
        bail!("kind={} não é distribution", m.kind);
    }
    apply_distribution_manifest(core, paths, &m).await
}

/// Lê o manifesto de um pacote já instalado no perfil.
fn read_installed_manifest(
    core: &atom_core::bus::Core,
    name: &str,
) -> Result<atom_protocol::Manifest> {
    let lock =
        atom_core::pkg::load_lock(&core.paths).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let lp = lock.find(name).context("pacote não está no lock")?;
    let dir = atom_core::pkg::package_dir(&core.data_store, lp)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    atom_core::pkg::read_manifest(&dir).map_err(|e| anyhow::anyhow!(e.to_string()))
}

/// Aplica o conteúdo de uma distribution: sources → packages → bindings/config.
async fn apply_distribution_manifest(
    core: &mut atom_core::bus::Core,
    paths: &atom_core::profile::ProfilePaths,
    m: &atom_protocol::Manifest,
) -> Result<()> {
    let dist = m
        .contributes
        .get("distribution")
        .context("sem [contributes.distribution]")?;
    if let Some(srcs) = dist.get("sources").and_then(|v| v.as_array()) {
        let mut cfg = atom_core::profile::load_or_create(paths)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        for s in srcs {
            let t = s.as_table().context("source inválida na distribution")?;
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .context("source sem name")?;
            let url = t
                .get("url")
                .and_then(|v| v.as_str())
                .context("source sem url")?;
            let mut keys: Vec<String> = t
                .get("keys")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(k) = t.get("key").and_then(|v| v.as_str()) {
                keys.push(k.to_string());
            }
            let priority = t.get("priority").and_then(|v| v.as_integer()).unwrap_or(0) as i32;
            cfg.sources.retain(|x| x.name != name);
            cfg.sources.push(atom_core::profile::SourceCfg {
                name: name.to_string(),
                url: url.to_string(),
                keys,
                priority,
                root_sha256: None,
            });
        }
        atom_core::profile::save(paths, &cfg).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    if let Some(pkgs) = dist.get("packages").and_then(|v| v.as_table()) {
        for (pkg_name, req) in pkgs {
            let req = req.as_str();
            let id = atom_core::store::install_package(core, pkg_name, req, None, false)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            core.ensure_kinds()
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let name = id.split('@').next().unwrap_or(&id).to_string();
            approve_on_install(core, &name)?;
            println!("instalado {id}");
        }
    }
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
    core.refresh().map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
