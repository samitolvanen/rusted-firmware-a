// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use clap::{Parser, ValueEnum};
use rustc_demangle::demangle;
use serde::{Deserialize, Serialize};
use stack_sizes::{Function, analyze_executable};
use std::{
    fs::{File, read},
    io::{Write, stdout},
    path::PathBuf,
    process::exit,
};

/// Gathers the stack usage of each function of the ELF file. Compile the binary with
/// '-Z emit-stack-sizes' rustc flag set.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path of the ELF file input
    #[arg(short = 'i', long)]
    input: PathBuf,

    /// Output file
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Output format
    #[arg(short = 'O', long)]
    output_format: Option<OutputFormat>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum OutputFormat {
    /// CSV
    Csv,
    /// JSON
    Json,
}

#[derive(Serialize, Deserialize, Debug)]
struct FunctionStat {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub stack_usage: Option<u64>,
}

impl From<(u64, Function<'_>)> for FunctionStat {
    fn from(value: (u64, Function)) -> Self {
        let (address, function) = value;
        let name = demangle(function.names()[0]).to_string();

        Self {
            name,
            address,
            size: function.size(),
            stack_usage: function.stack(),
        }
    }
}

fn main() {
    let args = Args::parse();

    let Ok(elf_data) = read(&args.input) else {
        eprintln!("Cannot read file: {:?}", args.input);
        exit(1);
    };

    let functions = analyze_executable(&elf_data).unwrap();

    let mut function_stats: Vec<_> = functions
        .defined
        .iter()
        .map(|(a, f)| FunctionStat::from((*a, f.clone())))
        .collect();

    // Ascending order by stack usage
    function_stats.sort_by_key(|fs| fs.stack_usage);

    let mut writer: Box<dyn Write> = match args.output {
        None => Box::new(stdout()),
        Some(path) => Box::new(File::create(path).expect("Failed to crate output file")),
    };

    match args.output_format {
        None => {
            writeln!(writer, "{:16} {:8} {:8} name", "address", "stack", "size")
                .expect("Failed to write header");
            for func in function_stats {
                writeln!(
                    writer,
                    "{:016x} {:8x} {:8x} {}",
                    func.address,
                    func.stack_usage.unwrap_or(0),
                    func.size,
                    func.name
                )
                .expect("Failed to write output");
            }
        }
        Some(OutputFormat::Csv) => {
            let mut csv_writer = csv::Writer::from_writer(writer);

            for record in function_stats {
                csv_writer
                    .serialize(record)
                    .expect("Failed to write CSV output");
            }
        }
        Some(OutputFormat::Json) => serde_json::to_writer_pretty(writer, &function_stats)
            .expect("Failed to write JSON output"),
    }
}
