pub mod cli;
pub mod langs;
pub mod lib;

use clap::{Command, Parser};
use colored::*;
use dialoguer::{theme::ColorfulTheme, Select};
use dirs::data_dir;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::get;
use std::{
    cmp::min,
    fmt::Write as _,
    fs::{create_dir, read_dir, read_to_string, remove_file, File},
    io::{self, BufRead, Error, Write},
};

use cli::Cli;
use langs::{LOCALES, SUPPORTED_LANGS};
use lib::{edit_distance, insert_and_shift, yank};

fn main() {
    std::process::exit(match run_app() {
        Ok(_) => 0,
        Err(error) => {
            eprintln!("Error: {:?}", error);
            1
        }
    });
}

/// Main function to run the application. Return `std::result::Result<(), std::io::Error>`.
fn run_app() -> std::result::Result<(), Error> {
    // Correctly output ANSI escape codes on Windows.
    #[cfg(windows)]
    colored::control::set_virtual_terminal(true).ok();

    // Parse args using clap.
    let args = Cli::parse();

    // Print all supported languages.
    if args.print_langs {
        println!("Supported Languages:");
        let mut langs: Vec<String> = vec![];

        // Add words to vector.
        for key in SUPPORTED_LANGS.keys() {
            langs.push(format!(" - {}: {}", key, SUPPORTED_LANGS.get(key).unwrap()));
        }

        // Sort and print vector.
        langs.sort();
        for lang in langs {
            println!("{}", lang);
        }

        std::process::exit(0);
    }

    // Update all downloaded languages.
    if args.update_langs {
        update_langs();
        std::process::exit(0);
    }

    // Unwrap Option<String> or check if something was piped in as the search term.
    let search_term = args.search_term.unwrap_or_else(|| {
        // Check if stdin is empty, produce error if so.
        if atty::is(atty::Stream::Stdin) {
            Command::new("dym [OPTIONS] <SEARCH_TERM>")
                .error(
                    clap::ErrorKind::MissingRequiredArgument,
                    format!(
                        "The {} argument was not provided.\n\n\tEither provide it as an argument or pass it in from standard input.",
                        "<SEARCH_TERM>".green()
                    )
                )
                .exit();
        } else {
            // Read search_term from standard input if stdin is not empty.
            let mut search_term = String::new();
            io::stdin().lock().read_line(&mut search_term).unwrap();
            search_term
        }
    });

    if SUPPORTED_LANGS.contains_key(args.lang.as_str()) {
        fetch_word_list(args.lang.to_owned());
    } else {
        // Not supported.
        // Whether or not locale code is valid.
        let error_string = if LOCALES.contains_key(args.lang.as_str()) {
            format!(
                "There is currently no word list for {}",
                LOCALES.get(args.lang.as_str()).cloned().unwrap()
            )
        } else {
            format!("{} is not a recognized localed code", args.lang)
        };

        // Exit with error.
        Command::new("dym [OPTIONS] <SEARCH_TERM>")
            .error(clap::ErrorKind::MissingRequiredArgument, error_string)
            .exit();
    }

    // Get word list. The program will only get here if/when this is a valid word list.
    let word_list = read_to_string(dirs::data_dir().unwrap().join("didyoumean").join(args.lang))
        .expect("Error reading file");

    // Get dictionary of words from words.txt.
    let dictionary = word_list.split('\n');

    // Create mutable vecs for storing the top n words.
    let mut top_n_words = vec![""; args.number];
    let mut top_n_dists = vec![search_term.len() * 10; args.number];

    // Loop over the words in the dictionary, run the algorithm, and
    // add to the list if appropriate.
    let search_chars = search_term.chars().collect::<Vec<_>>();
    for word in dictionary {
        // Get edit distance.
        let dist = edit_distance(&search_chars, word);

        // Add to the list if appropriate.
        if dist < top_n_dists[args.number - 1] {
            for i in 0..args.number {
                if dist < top_n_dists[i] {
                    insert_and_shift(&mut top_n_dists, i, dist);
                    insert_and_shift(&mut top_n_words, i, word);
                    break;
                }
            }
        }
    }

    // Print out results.
    if !args.clean_output {
        println!("{}", "Did you mean?".blue().bold());
    }
    let mut items = vec!["".to_string(); args.number];
    for i in 0..args.number {
        let mut output = String::new();
        let indent = args.number.to_string().len();

        // Add numbers if not clean.
        if !args.clean_output {
            write!(
                output,
                "{:>indent$}{} ",
                (i + 1).to_string().purple(),
                ".".purple()
            )
            .unwrap();
        }

        // Add words in order of edit distance.
        output.push_str(top_n_words[i]);

        // Add edit distance if verbose.
        if args.verbose {
            write!(output, " (edit distance: {})", top_n_dists[i]).unwrap();
        }

        // Print concatenated string.
        items[i] = output;
    }

    // If the yank argument is set, copy the item to the clipboard.
    if args.yank {
        // Get the chosen argument with prompt.
        let chosen = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("[↑↓ to move, ↵ to select, esc/q to cancel]")
            .items(&items)
            .default(0)
            .report(false)
            .clear(false)
            .interact_opt()?;

        match chosen {
            // If the chosen arguemnt is valid.
            Some(index) => {
                yank(top_n_words[index]);
                println!(
                    "{}",
                    format!("\"{}\" copied to clipboard", top_n_words[index]).green()
                );
            }
            // If no argument is chosen.
            None => {
                println!("{}", "No selection made".red());
                std::process::exit(1);
            }
        }
    } else {
        // If yank is not set, print out all the items.
        for item in items {
            println!("{}", item);
        }
    }

    Ok(())
}

/// Fetch the word list specified by `lang` from https://github.com/hisbaan/wordlists
///
/// # Arguments
///
/// * `lang` - A locale code string to define the word list file to fetch.
#[tokio::main]
async fn fetch_word_list(lang: String) {
    // Get data directory.
    let data_dir = dirs::data_dir().unwrap().join("didyoumean");

    // Create data directory if it doesn't exist.
    if !data_dir.is_dir() {
        create_dir(data_dir).expect("Failed to create data directory");
    }

    // Get file path.
    let file_path = dirs::data_dir().unwrap().join("didyoumean").join(&lang);

    // If the file does not exist, fetch it from the server.
    if !file_path.is_file() {
        println!(
            "Downloading {} word list...",
            LOCALES.get(&lang).unwrap().to_string().blue()
        );

        let url = format!(
            "https://raw.githubusercontent.com/hisbaan/wordlists/main/{}",
            &lang
        );

        // Setup reqwest.
        let response = get(&url).await.expect("Request failed");
        let total_size = response.content_length().unwrap();
        let mut file = File::create(file_path).expect("Failed to create file");
        let mut downloaded: u64 = 0;
        let mut stream = response.bytes_stream();

        // Setup indicatif.
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(
                    "[{elapsed_precise}] [{wide_bar:.blue/cyan}] {bytes}/{total_bytes} ({eta})",
                )
                .progress_chars("#>-"),
        );

        // Read from stream into file.
        while let Some(item) = stream.next().await {
            let chunk = item.expect("Error downloading file");
            file.write_all(&chunk).expect("Error while writing to file");
            let new = min(downloaded + (chunk.len() as u64), total_size);
            downloaded = new;
            pb.set_position(new);
        }

        // Print completed bar.
        pb.finish_at_current_pos();
    }
}

/// Update the word list files by deleting and downloading the files from the repository.
fn update_langs() {
    let data = data_dir().unwrap().join("didyoumean");

    // Create data directory if it doesn't exist.
    if !data.is_dir() {
        create_dir(&data).expect("Failed to create data directory");
    }

    // Get files in data directory.
    let data_dir_files = read_dir(&data).unwrap();

    // Delete and update all files.
    for file in data_dir_files {
        let file_name = file.unwrap().file_name();
        let string: &str = file_name.to_str().unwrap();

        // Only delete and download if the language is supported.
        if SUPPORTED_LANGS.contains_key(string) {
            remove_file(data.join(&string)).expect("Failed to update file (deletion failed)");
            fetch_word_list(string.to_string());
        }
    }
}
