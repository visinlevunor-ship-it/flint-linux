mod arith;
mod ast;
mod builtins;
mod expand;
mod glob;
mod jobs;
mod lexer;
mod lineedit;
mod parser;
mod printf;
mod repl;
mod shell;
mod testcmd;
mod util;

/// Entry point; `args[0]` is the name the shell was started as (`-sh` means a login shell).
pub fn main(args: Vec<String>) -> i32 {
    repl::main(args)
}
