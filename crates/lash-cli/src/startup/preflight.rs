//! Pre-pipeline flows that exit before any provider/session work:
//! `--reset`, `--check-update`, and `--update`.

use crate::Args;

pub(super) async fn probe_runtime_store() -> anyhow::Result<lash::preflight::PreflightReport> {
    let handle =
        lash_sqlite_store::SqliteStorePreflight::for_durable_core(crate::paths::durable_core_db())
            .with_process_registry(crate::paths::processes_db())
            .with_trigger_store(crate::paths::triggers_db())
            .with_effect_journal(crate::paths::effects_db());
    let report =
        lash::preflight::probe_store(&handle, lash::preflight::PreflightOptions::summary()).await?;
    if let Some(message) = report.refusal_message() {
        anyhow::bail!(message);
    }
    Ok(report)
}

/// Handle flags that complete without starting a session. Returns `true`
/// when the process should exit successfully without continuing startup.
pub(super) async fn handle_early_exit_flags(args: &Args) -> anyhow::Result<bool> {
    if args.reset {
        run_reset()?;
        return Ok(true);
    }

    if args.check_update {
        println!("{}", crate::update::check_update_text().await?);
        return Ok(true);
    }

    if args.update {
        crate::update::install_latest_release().await?;
        return Ok(true);
    }

    Ok(false)
}

/// `--reset`: confirm, then delete the unified store and host session roster.
fn run_reset() -> anyhow::Result<()> {
    use std::io::Write;

    // Design system ANSI colors
    const SODIUM: &str = "\x1b[38;2;232;163;60m"; // #e8a33c
    const CHALK: &str = "\x1b[38;2;232;228;208m"; // #e8e4d0
    const ASH_TEXT: &str = "\x1b[38;2;90;90;80m"; // #5a5a50
    const LICHEN: &str = "\x1b[38;2;138;158;108m"; // #8a9e6c
    const ERR: &str = "\x1b[38;2;204;68;68m"; // #c44
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";

    let store_dir = crate::paths::store_dir();
    let sessions_dir = crate::session_log::sessions_dir();

    eprintln!();
    eprintln!("  {SODIUM}{BOLD}/ reset{RESET}");
    eprintln!();
    eprintln!("  {ERR}This will permanently delete Lash runtime data:{RESET}");
    eprintln!();
    eprintln!(
        "    {ASH_TEXT}durable store         {CHALK}{}{RESET}",
        store_dir.display()
    );
    eprintln!(
        "    {ASH_TEXT}session roster        {CHALK}{}{RESET}",
        sessions_dir.display()
    );
    eprintln!();
    eprint!("  {SODIUM}Are you sure? [y/N]{RESET} ");
    std::io::stderr().flush()?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        if store_dir.exists() {
            std::fs::remove_dir_all(&store_dir)?;
        }
        if sessions_dir.exists() {
            std::fs::remove_dir_all(&sessions_dir)?;
        }
        eprintln!("  {LICHEN}Done.{RESET} Runtime store and session roster removed.");
    } else {
        eprintln!("  {ASH_TEXT}Aborted.{RESET}");
    }
    eprintln!();
    Ok(())
}
