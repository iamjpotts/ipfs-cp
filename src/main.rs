
use std::env;
use std::process::ExitCode;

use ipfs_api::{IpfsApi, IpfsClient, TryFromUri};
use ipfs_api::request::FilesLs;
use ipfs_api::response::FilesEntry;
use ipfs_cp::errors::MainError;

const IPFS_ENTRY_TYPE_FILE: u64 = 0;
const IPFS_ENTRY_TYPE_FOLDER: u64 = 1;

enum UnpinnedRule {
    Ban,
    Copy,
    Ignore
}

fn expect_env(name: &str) -> String {
    env::var(name)
        .expect(name)
}

#[tokio::main]
async fn main() -> ExitCode {
    println!("Hello, world!");

    if std::env::args().len() > 2 {
        eprintln!("Incorrect number of arguments; should be none, --copy-unpinned, or --skip-unpinned.");
        return ExitCode::FAILURE;
    }

    let rule = if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--copy-unpinned" => UnpinnedRule::Copy,
            "--skip-unpinned" => UnpinnedRule::Ignore,
            _ => {
                eprintln!("Invalid flag: {}", arg);
                return ExitCode::FAILURE;
            }
        }
    }
    else {
        UnpinnedRule::Ban
    };

    let src_api_url = expect_env("SRC_API_URL");
    let src_username = expect_env("SRC_USERNAME");
    let src_password = expect_env("SRC_PASSWORD");

    let dst_api_url = expect_env("DST_API_URL");
    let dst_username = expect_env("DST_USERNAME");
    let dst_password = expect_env("DST_PASSWORD");
    let dst_folder = expect_env("DST_FOLDER");

    println!("Source        : {}", src_api_url);
    println!("Target        : {}", dst_api_url);
    println!("Target folder : {}", dst_folder);

    if !dst_folder.starts_with("/") {
        eprintln!("DST_FOLDER must start with / but was: {}", dst_folder);
        return ExitCode::FAILURE;
    }

    let source = IpfsClient::from_str(&src_api_url)
        .unwrap()
        .with_credentials(&src_username, &src_password);

    let target = IpfsClient::from_str(&dst_api_url)
        .unwrap()
        .with_credentials(&dst_username, &dst_password);

    println!("Running");

    if let Err(e) = run(&source, &target, rule, &dst_folder).await {
        println!();
        eprintln!("Oh, no: {:?}", e);
        ExitCode::FAILURE
    }
    else {
        println!();
        println!("Done.");
        ExitCode::SUCCESS
    }
}

async fn run<A: IpfsApi<Error = ipfs_api::Error>>(source: &A, target: &A, rule: UnpinnedRule, target_folder: &str) -> Result<(), MainError> {
    println!("Getting file list");

    let request = FilesLs {
        path: None,
        long: Some(true),
        unsorted: Some(false)
    };

    let response = source.files_ls_with_options(request)
        .await?;

    let mut entries = response.entries;

    println!("Found {} files:", entries.len());

    for entry in &entries {
        println!("  name: {}, size: {}, type: {}, hash: {}", entry.name, entry.size, entry.typ, entry.hash);
    }

    println!();
    println!("Creating target folder {}", target_folder);

    target.files_mkdir(target_folder, true)
        .await?;

    // Sort files first, then folders. Among files, sort by size ascending.
    // Folders do not report their size; it is reported as zero.
    entries.sort_by_key(|e| (e.typ, e.size));

    // Find out which entries, if any, are NOT pinned on the source
    let mut unpinned = Vec::new();

    println!();
    println!("Identifying any unpinned source entries");

    for (i, entry) in entries.iter().enumerate() {
        println!("  ..inspecting {}", entry.name);

        // Quick when pinned, takes a few seconds when not pinned.
        let pinned = match source.pin_ls(Some(&entry.hash), None).await {
            Ok(pin_response) => {
                Ok(pin_response.keys.contains_key(&entry.hash))
            },
            Err(e) => {
                // TODO: Match against proper enum
                let msg = format!("{}", e);

                if msg.contains(&entry.hash) && msg.contains("is not pinned") {
                    Ok(false)
                }
                else {
                    Err(e)
                }
            }
        }?;

        if !pinned {
            println!("  ..not pinned.");
            unpinned.push(i);
        }
    }

    println!();

    if unpinned.is_empty() {
        println!("No unpinned source entries.");
    }
    else {
        println!("Found {} unpinned source entries.", unpinned.len());
        println!();

        match rule {
            UnpinnedRule::Ban => {
                return Err(
                    MainError::Message(
                        "Either add --skip-unpinned or --copy-unpinned; however copying is likely to fail (hang indefinitely) due to source data having been garbage collected.".into()
                    )
                );
            },
            UnpinnedRule::Ignore => {
                println!("Will ignore unpinned source entries:");

                // Remove from entry list (last to first, so indices don't shift)
                for i in unpinned.into_iter().rev() {
                    println!("  ..ignoring {}", entries[i].name);
                    entries.remove(i);
                }
            },
            UnpinnedRule::Copy => {
                println!("Will copy unpinned source entries.");
            }
        }
    }
    println!();

    copy_all_entries(&entries, target, target_folder)
        .await?;

    println!();
    println!("{} - getting hash", target_folder);
    let stat = target.files_stat(target_folder)
        .await?;

    println!("{} - hash is {}", target_folder, stat.hash);

    println!("{} - pinning final version", target_folder);
    target.pin_add(&stat.hash, true)
        .await?;

    Ok(())
}

async fn copy_file<A: IpfsApi<Error = ipfs_api::Error>>(source_file: &FilesEntry, target: &A, target_folder: &str) -> Result<(), MainError> {
    let target_file = format!("{}/{}", target_folder, source_file.name);

    let stat_result = target.files_stat(&target_file)
        .await;

    let target_file_stat = match stat_result {
        Err(e) => {
            if let ipfs_api::Error::Api(ipfs_api::ApiError { message, .. }) = &e {
                if message.contains("file does not exist") {
                    Ok(None)
                }
                else {
                    Err(e)
                }
            }
            else {
                Err(e)
            }
        },
        Ok(stat) => Ok(Some(stat))
    }?;

    let established = if let Some(stat) = target_file_stat {
        if stat.hash == source_file.hash {
            println!("{} - already pinned with matching hash", target_file);
            true
        }
        else {
            println!("{} - previous hash is {}", target_file, stat.hash);
            // Might need to delete/unpin this file first.
            false
        }
    }
    else {
        false
    };

    if !established {
        println!("{} - establishing pin in mfs", target_file);

        target.files_cp(&format!("/ipfs/{}", source_file.hash), &target_file)
            .await?;
    }

    Ok(())
}

async fn copy_all_entries<A: IpfsApi<Error = ipfs_api::Error>>(entries: &Vec<FilesEntry>, target: &A, target_folder: &str) -> Result<(), MainError> {
    for (i, entry) in entries.iter().enumerate() {
        println!();
        println!(
            "Copying {} of {} ({})",
            i + 1,
            entries.len(),
            match entry.typ {
                IPFS_ENTRY_TYPE_FILE => format!("{} mb", entry.size / (1024 * 1024)),
                IPFS_ENTRY_TYPE_FOLDER => "folder".into(),
                _ => "other".into()
            }
        );
        println!();

        pin(&entry, target)
            .await?;

        copy_file(&entry, target, &target_folder)
            .await?;
    }

    Ok(())
}

async fn pin<A: IpfsApi<Error = ipfs_api::Error>>(file: &FilesEntry, target: &A) -> Result<(), MainError> {
    println!("{} - pinning from {}", file.name, file.hash);
    target.pin_add(&file.hash, true)
        .await?;
    println!("{} - pinned", file.name);

    Ok(())
}
