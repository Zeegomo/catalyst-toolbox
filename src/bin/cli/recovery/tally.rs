use catalyst_toolbox::recovery::tally::recover_ledger_from_logs;
use chain_core::property::{Deserialize, Fragment};
use chain_impl_mockchain::block::Block;
use jcli_lib::utils::{
    output_file::{Error as OutputFileError, OutputFile},
    output_format::{Error as OutputFormatError, OutputFormat},
};
use jormungandr_lib::interfaces::{
    load_persistent_fragments_logs_from_folder_path, VotePlanStatus,
};

use log::warn;
use std::io::{BufReader, Write};
use std::path::PathBuf;

use reqwest::Url;
use structopt::StructOpt;

#[allow(clippy::large_enum_variant)]
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),

    #[error(transparent)]
    Recovery(#[from] catalyst_toolbox::recovery::tally::Error),

    #[error(transparent)]
    OutputFile(#[from] OutputFileError),

    #[error(transparent)]
    OutputFormat(#[from] OutputFormatError),

    #[error(transparent)]
    Request(#[from] reqwest::Error),

    #[error("Block0 should be provided either from a path (block0-path) or an url (block0-url)")]
    Block0Unavailable,

    #[error("Could not load persistent logs from path")]
    PersistenLogsLoading(#[source] std::io::Error),

    #[error("Could not load block0")]
    Block0Loading(#[source] std::io::Error),
}

/// Recover the tally from fragment log files and the initial preloaded block0 binary file.
#[derive(StructOpt)]
#[structopt(rename_all = "kebab")]
pub struct Replay {
    /// Path to the block0 binary file
    #[structopt(long, conflicts_with = "block0-url")]
    block0_path: Option<PathBuf>,

    /// Url to a block0 endpoint
    #[structopt(long)]
    block0_url: Option<Url>,

    /// Path to the folder containing the log files used for the tally reconstruction
    #[structopt(long)]
    logs_path: PathBuf,

    #[structopt(flatten)]
    output: OutputFile,

    #[structopt(flatten)]
    output_format: OutputFormat,

    /// Verbose mode (-v, -vv, -vvv, etc)
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: usize,
}

fn read_block0(path: PathBuf) -> Result<Block, Error> {
    let reader = std::fs::File::open(path)?;
    Block::deserialize(BufReader::new(reader)).map_err(Error::Block0Loading)
}

fn load_block0_from_url(url: Url) -> Result<Block, Error> {
    let block0_body = reqwest::blocking::get(url)?.bytes()?;
    Block::deserialize(BufReader::new(&block0_body[..])).map_err(Error::Block0Loading)
}

impl Replay {
    pub fn exec(self) -> Result<(), Error> {
        let Replay {
            block0_path,
            block0_url,
            logs_path,
            output,
            output_format,
            verbose,
        } = self;
        stderrlog::new().verbosity(verbose).init().unwrap();

        let block0 = if let Some(path) = block0_path {
            read_block0(path)?
        } else if let Some(url) = block0_url {
            load_block0_from_url(url)?
        } else {
            return Err(Error::Block0Unavailable);
        };

        let fragments = load_persistent_fragments_logs_from_folder_path(&logs_path)
            .map_err(Error::PersistenLogsLoading)?;

        let (ledger, failed) = recover_ledger_from_logs(&block0, fragments)?;
        if !failed.is_empty() {
            warn!("{} fragments couldn't be properly processed", failed.len());
            for failed_fragment in failed {
                warn!("{}", failed_fragment.id());
            }
        }
        let voteplans = ledger.active_vote_plans();
        let voteplan_status: Vec<VotePlanStatus> =
            voteplans.into_iter().map(VotePlanStatus::from).collect();
        let mut out_writer = output.open()?;
        let content = output_format.format_json(serde_json::to_value(&voteplan_status)?)?;
        out_writer.write_all(content.as_bytes())?;
        Ok(())
    }
}
