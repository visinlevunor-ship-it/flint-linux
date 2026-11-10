mod arith;
mod ast;
mod builtins;
mod expand;
mod glob;
mod jobs;
mod lexer;
mod parser;
mod printf;
mod repl;
mod shell;
mod testcmd;
mod util;

fn main() {
    std::process::exit(repl::main());
}
