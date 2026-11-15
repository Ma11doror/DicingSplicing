//! ds-tool: changes DicingSplicing projects from the command line, for scripts and AI agents.
//! docs/ds-tool.md tells how to use it; `ds-tool help` lists its commands.

fn main() {
    std::process::exit(dicing_splicing::cli::run(std::env::args().skip(1).collect()));
}
