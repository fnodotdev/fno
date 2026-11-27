//! The `fno-agent` client binary: the library's client dispatch, unchanged.
//! The `fno` front door runs the same dispatch in process.

fn main() {
    fno_agent::client_cli::main(std::env::args().skip(1).collect())
}
