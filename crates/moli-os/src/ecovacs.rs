use std::io::{self, IsTerminal as _};

use anyhow::{Context as _, ensure};
use moli_i18n::tr;

use crate::{Config, master_key};

#[derive(clap::Args)]
pub(crate) struct Pair {
    /// Ecovacs driver instance whose vault entry will be set.
    #[arg(long, default_value = "ecovacs")]
    instance: String,
}

pub(crate) fn run(config: &Config, args: &Pair) -> anyhow::Result<()> {
    let stdin = io::stdin();
    ensure!(!stdin.is_terminal(), "{}", tr!("ecovacs.pipe_input"));
    ensure!(
        config
            .drivers
            .iter()
            .any(|driver| driver.id == args.instance && driver.kind == "ecovacs"),
        "{}",
        tr!("ecovacs.invalid_instance")
    );
    let data = &config.server.data_dir;
    let key = master_key(data, false)?.with_context(|| tr!("ecovacs.no_vault"))?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let session = moli_ecovacs::pair(&mut stdin.lock()).await?;
            moli_runtime::set_secret(
                &data.join("secrets.enc"),
                &key,
                &format!("{}/{}", args.instance, moli_ecovacs::SESSION_SECRET),
                &session,
            )
            .await?;
            println!("{}", tr!("ecovacs.paired"));
            Ok(())
        })
}
