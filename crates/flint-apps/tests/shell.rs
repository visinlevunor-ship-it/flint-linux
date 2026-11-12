//! Runs the shell (`flint sh`) on scripts.

// Link Flint's C library into this test, as into the programs it runs.
use flint_core as _;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_flint");

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_args(dir: &Path, args: &[&str], stdin: Option<&str>) -> Out {
    let mut cmd = Command::new(BIN);
    cmd.arg("sh").args(args).current_dir(dir).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().expect("spawn sh");
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    }
    let o = child.wait_with_output().unwrap();
    Out { stdout: String::from_utf8_lossy(&o.stdout).into_owned(), stderr: String::from_utf8_lossy(&o.stderr).into_owned(), code: o.status.code().unwrap_or(-1) }
}

fn sh(script: &str) -> Out {
    run_args(Path::new("/"), &["-c", script], None)
}

/// Assert on stdout; the script's own errors would show up in the failure message.
fn check(script: &str, want: &str) {
    let o = sh(script);
    assert_eq!(o.stdout, want, "script: {script}\nstderr: {}", o.stderr);
}

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("flint-sh-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&p).unwrap();
        Tmp(fs::canonicalize(&p).unwrap())
    }
    fn run(&self, script: &str) -> Out {
        run_args(&self.0, &["-c", script], None)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn quoting_and_echo() {
    check("echo hello world", "hello world\n");
    check("echo 'a  b' \"c  d\" e\\ f", "a  b c  d e f\n");
    check("echo \"a\\\"b\"", "a\"b\n");
    check("echo 'it'\"'\"'s'", "it's\n");
    check("echo a\\ \\ b", "a  b\n");
    check("echo \"\\$x\" \\$x", "$x $x\n");
    check("echo \"a\\\\b\"", "a\\b\n");
    check("echo -n no newline", "no newline");
    check("echo -e 'a\\tb'", "a\tb\n");
    check("echo # a comment\necho after", "\nafter\n");
}

#[test]
fn variables_and_parameter_expansion() {
    check("x=5; echo $x ${x} \"$x\"", "5 5 5\n");
    check("echo ${undefined:-def}", "def\n");
    check("y=; echo ${y:-d} ${y-d}x", "d x\n");
    check("echo ${u:=assigned}; echo $u", "assigned\nassigned\n");
    check("x=5; echo ${x:+yes} [${nope:+yes}]", "yes []\n");
    check("x=hello; echo ${#x}", "5\n");
    check("f=a/b/c.txt; echo ${f##*/} ${f%.*} ${f#*/} ${f%%/*}", "c.txt a/b/c b/c.txt a\n");
    check("x=1 y=2; echo $x$y", "12\n");
    check("a=1; b=$a$a; echo $b", "11\n");
    check("echo ${x?must be set}; echo not reached", "");
    let o = sh("echo ${x?must be set}");
    assert_ne!(o.code, 0);
    assert!(o.stderr.contains("must be set"), "{}", o.stderr);
}

#[test]
fn special_parameters_and_arguments() {
    check("false; echo $?; true; echo $?", "1\n0\n");
    let o = run_args(Path::new("/"), &["-c", "echo $# $1 $2 $0", "name", "a", "b"], None);
    assert_eq!(o.stdout, "2 a b name\n");
    check("set -- a 'b c' d; for x in \"$@\"; do echo \"[$x]\"; done", "[a]\n[b c]\n[d]\n");
    check("set -- a 'b c' d; for x in $*; do echo \"[$x]\"; done", "[a]\n[b]\n[c]\n[d]\n");
    check("set -- a 'b c' d; echo \"$*\"", "a b c d\n");
    check("set -- a 'b c' d; IFS=,; echo \"$*\"", "a,b c,d\n");
    check("set -- a b c; shift; echo $@ $#; shift 2; echo $#", "b c 2\n0\n");
    check("set -- '' a; echo $#", "2\n");
    check("f() { echo $#; }; f '' ''; x=; f $x; set --; f \"$@\"", "2\n0\n0\n");
    let o = sh("echo $$");
    assert!(o.stdout.trim().parse::<u32>().is_ok(), "{}", o.stdout);
}

#[test]
fn arithmetic() {
    check("echo $((2+3*4)) $(( (1+2)*3 )) $((10/3)) $((10%3))", "14 9 3 1\n");
    check("x=7; echo $((x*2)) $(($x+1))", "14 8\n");
    check("i=0; i=$((i+1)); i=$((i+1)); echo $i", "2\n");
    check("echo $((1<2)) $((2<1)) $((3==3 && 1))", "1 0 1\n");
    check("echo $((0x10)) $((010)) $((-5+2))", "16 8 -3\n");
}

#[test]
fn command_substitution() {
    check("echo $(echo hi)", "hi\n");
    check("echo `echo hi`", "hi\n");
    check("echo $(echo $(echo deep))", "deep\n");
    check("x=$(printf 'a\\nb\\n\\n\\n'); echo \"$x\"", "a\nb\n");
    check("echo \"$(printf 'a   b')\" $(printf 'a   b')", "a   b a b\n");
    check("x=$(false); echo $?", "1\n");
    check("for w in $(echo a b c); do echo \"<$w>\"; done", "<a>\n<b>\n<c>\n");
    check("echo \"[$(echo 'x y')]\"", "[x y]\n");
    check("x=outer; echo $(x=inner; echo $x) $x", "inner outer\n");
}

#[test]
fn pipelines_lists_and_status() {
    check("printf 'b\\na\\n' | sort", "a\nb\n");
    check("printf '3\\n1\\n2\\n' | sort | head -2 | tail -1", "2\n");
    check("echo abc | tr a-z A-Z", "ABC\n");
    check("false | true; echo $?; true | false; echo $?", "0\n1\n");
    check("! false; echo $?; ! true; echo $?", "0\n1\n");
    check("true && echo yes || echo no; false && echo yes || echo no", "yes\nno\n");
    check("false || echo b; true || echo never", "b\n");
    check("echo a; echo b\necho c", "a\nb\nc\n");
    check("echo a &&\necho b", "a\nb\n");
}

#[test]
fn conditionals_and_loops() {
    check("if false; then echo a; elif true; then echo b; else echo c; fi", "b\n");
    check("if false; then echo a; else echo c; fi", "c\n");
    check("if [ 3 -lt 5 ]; then echo lt; fi", "lt\n");
    check("if test -d /; then echo dir; fi; if [ -f / ]; then echo file; else echo notfile; fi", "dir\nnotfile\n");
    check("if ! false; then echo neg; fi", "neg\n");
    check("if true; then false; fi; echo $?", "1\n");
    check("i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done", "0\n1\n2\n");
    check("i=0; until [ $i -ge 2 ]; do echo $i; i=$((i+1)); done", "0\n1\n");
    check("for i in 1 2 3; do [ $i = 2 ] && continue; echo $i; done", "1\n3\n");
    check("for i in 1 2 3; do echo $i; [ $i = 2 ] && break; done", "1\n2\n");
    check("for i in 1 2; do for j in a b; do echo $i$j; break 2; done; done", "1a\n");
    check("for i in 1 2; do for j in a b; do [ $j = a ] && continue 2; echo never; done; done; echo end", "end\n");
    check("for i in a b; do :; done; echo $i", "b\n");
}

#[test]
fn case_statements() {
    check("case foo in f*) echo starts;; *) echo other;; esac", "starts\n");
    check("case x in a|x|b) echo m;; esac", "m\n");
    check("case 'a b' in \"a b\") echo q;; esac", "q\n");
    check("case abc in [a-c]*) echo cls;; esac", "cls\n");
    check("case zzz in a) echo a;; b) echo b;; esac; echo status $?", "status 0\n");
    check("x='*'; case abc in $x) echo glob;; esac", "glob\n");
    check("x='*'; case abc in \"$x\") echo glob;; *) echo literal;; esac", "literal\n");
    check("case a in\n a) echo one\n ;;\n b) echo two\n ;;\nesac", "one\n");
}

#[test]
fn functions() {
    check("f() { echo \"got $1 $#\"; return 3; }; f a b; echo $?", "got a 2\n3\n");
    check("x=outer; f() { local x=inner; echo $x; }; f; echo $x", "inner\nouter\n");
    check("fact() { if [ $1 -le 1 ]; then echo 1; else echo $(( $1 * $(fact $(($1-1))) )); fi; }; fact 5", "120\n");
    check("set -- p q; f() { echo $1; }; f z; echo $1", "z\np\n");
    check("f() { echo one; }; f() { echo two; }; f", "two\n");
    check("f() { return; }; false; f; echo $?", "1\n");
    check("f() { echo in f; }\nf\nunset -f f\ntype f", "in f\n");
    check("greet() {\n  echo \"hi $1\"\n}\ngreet bob", "hi bob\n");
}

#[test]
fn subshells_and_groups() {
    check("(x=1; echo $x); echo ${x:-unset}", "1\nunset\n");
    check("{ echo a; echo b; } | cat", "a\nb\n");
    check("cd /; (cd /tmp; pwd); pwd", "/tmp\n/\n");
    check("(exit 3); echo $?", "3\n");
    check("x=1; { x=2; }; echo $x", "2\n");
    check("(echo a; (echo b))", "a\nb\n");
}

#[test]
fn redirections() {
    let t = Tmp::new();
    assert_eq!(t.run("echo hi > f; cat f").stdout, "hi\n");
    assert_eq!(t.run("echo a > f; echo b >> f; cat f").stdout, "a\nb\n");
    assert_eq!(t.run("echo a > f; cat < f").stdout, "a\n");
    assert_eq!(t.run("{ echo out; echo err >&2; } > o 2> e; cat o e").stdout, "out\nerr\n");
    assert_eq!(t.run("{ echo out; echo err >&2; } 2>&1 | sort").stdout, "err\nout\n");
    assert_eq!(t.run("ls /nonexistent-flint 2>/dev/null; echo $?").stdout, "2\n");
    assert_eq!(t.run("x=5\ncat <<EOF\nvalue $x $(echo sub)\nEOF\n").stdout, "value 5 sub\n");
    assert_eq!(t.run("x=5\ncat <<'EOF'\nvalue $x\nEOF\n").stdout, "value $x\n");
    assert_eq!(t.run("cat <<-EOF\n\tindented\n\tEOF\n").stdout, "indented\n");
    assert_eq!(t.run("{ echo a; echo b; } > f; while read l; do echo \"<$l>\"; done < f").stdout, "<a>\n<b>\n");
    assert_eq!(t.run("read a b <<EOF\n1 2\nEOF\necho $b$a").stdout, "21\n");
    assert_eq!(t.run("echo x > f 2>&1; cat f").stdout, "x\n");
    let bad = t.run("cat < nonexistent; echo status $?");
    assert!(bad.stdout.contains("status 1") && bad.stderr.contains("nonexistent"), "{}{}", bad.stdout, bad.stderr);
    assert_eq!(t.run("echo a > f; echo b > f; cat f").stdout, "b\n", "> truncates");
    // a redirection on a builtin must not leak to the next command
    assert_eq!(t.run("echo hidden > f; echo shown").stdout, "shown\n");
}

#[test]
fn globbing_and_splitting() {
    let t = Tmp::new();
    assert_eq!(t.run("touch a.txt b.txt c.rs .hidden; echo *.txt; echo \"*.txt\"; echo *.zip").stdout, "a.txt b.txt\n*.txt\n*.zip\n");
    assert_eq!(t.run("touch a.txt b.txt c.rs; echo *").stdout, "a.txt b.txt c.rs\n");
    assert_eq!(t.run("touch a.txt b.txt c.rs; echo [ab].txt ?.rs").stdout, "a.txt b.txt c.rs\n");
    assert_eq!(t.run("mkdir d; touch d/x d/y; echo d/*").stdout, "d/x d/y\n");
    let fresh = Tmp::new();
    assert_eq!(fresh.run("touch 'a b'; for f in *; do echo \"[$f]\"; done").stdout, "[a b]\n");
    check("x=\"a b  c\"; for w in $x; do echo \"<$w>\"; done; echo \"<$x>\"", "<a>\n<b>\n<c>\n<a b  c>\n");
    check("IFS=:; x=a:b:c; for p in $x; do echo $p; done", "a\nb\nc\n");
    check("x='*'; echo \"$x\" '*'", "* *\n");
    check("HOME=/tmp; echo ~ ~/x", "/tmp /tmp/x\n");
}

#[test]
fn read_printf_test_and_friends() {
    check("echo 'a b c' | { read x y; echo \"$y|$x\"; }", "b c|a\n");
    check("printf 'l1\\nl2\\n' | while read l; do echo \"[$l]\"; done", "[l1]\n[l2]\n");
    check("echo 'a\\b' | { read -r v; echo \"$v\"; }", "a\\b\n");
    check("echo 'a\\b' | { read v; echo \"$v\"; }", "ab\n");
    check("printf 'x' | { read v; echo \"$? $v\"; }", "1 x\n");
    check("printf '' | { read v; echo \"$?\"; }", "1\n");
    check("printf '%s-%d\\n' a 5", "a-5\n");
    check("printf '%s\\n' a b c", "a\nb\nc\n");
    check("printf '%5.1s|%-4d|%04d\\n' abc 7 42", "    a|7   |0042\n");
    check("[ -z \"\" ] && echo empty; [ a = a ] && echo eq; [ a != b ] && echo ne; [ 2 -gt 1 ] && echo gt", "empty\neq\nne\ngt\n");
    check("[ -n \"x\" -a 1 -lt 2 ] && echo both; [ ! -e /nonexistent-flint ] && echo missing", "both\nmissing\n");
    check("test 5 -eq 5 && echo ok", "ok\n");
    check("type echo; command -v echo; command -v nonexistent-flint-cmd; echo $?", "echo is a shell builtin\necho\n1\n");
    check("umask 022; umask", "0022\n");
    check("pwd; cd /tmp; pwd; cd -; echo $?", "/\n/tmp\n/\n0\n");
}

#[test]
fn eval_source_exec_and_env() {
    check("eval 'x=3; echo $x'", "3\n");
    check("cmd='echo built'; eval $cmd", "built\n");
    let t = Tmp::new();
    assert_eq!(t.run("echo 'f() { echo from-source $1; }; V=set' > lib.sh; . ./lib.sh; f arg; echo $V").stdout, "from-source arg\nset\n");
    assert_eq!(t.run("echo 'echo args: $1 $2' > s.sh; . ./s.sh p q").stdout, "args: p q\n");
    check("exec echo replaced; echo not reached", "replaced\n");
    check("export FOO=bar; env | grep '^FOO='", "FOO=bar\n");
    check("BAR=1; env | grep -c '^BAR='; echo done", "0\ndone\n");
    check("FOO=tmp env | grep '^FOO='; echo ${FOO:-none}", "FOO=tmp\nnone\n");
    check("f() { echo $V; }; V=x f; echo ${V:-unset}", "x\nunset\n");
    check("export A=1 B=2; export -p | grep -c '^export [AB]='", "2\n");
    check("unset HOME; echo ${HOME:-gone}", "gone\n");
}

#[test]
fn errors_and_exit_codes() {
    let o = sh("nonexistent_cmd_flint; echo $?");
    assert_eq!(o.stdout, "127\n");
    assert!(o.stderr.contains("not found"), "{}", o.stderr);
    let o = sh("/etc; echo $?");
    assert_eq!(o.stdout, "126\n", "{}", o.stderr);
    assert_eq!(sh("exit 7").code, 7);
    assert_eq!(sh("true; exit").code, 0);
    assert_eq!(sh("false; exit").code, 1);
    for bad in ["if true", "echo 'abc", "fi", "a | | b", "(a", "echo >", "for 1x in a; do b; done"] {
        let o = sh(bad);
        assert_eq!(o.code, 2, "{bad:?} -> {}{}", o.stdout, o.stderr);
        assert!(o.stderr.contains("syntax error"), "{bad:?} -> {}", o.stderr);
    }
    check("cd /nonexistent-flint; echo $?", "1\n");
    check("break; continue; echo still here", "still here\n");
    check("f() { break; echo in f; }; f", "in f\n");
    let o = sh("echo $((1/0)); echo after");
    assert!(o.stderr.contains("division by zero"), "{}", o.stderr);
    let o = sh("shift 5; echo $?");
    assert_eq!(o.stdout, "1\n");
    let o = sh("trap '' INT; echo $?");
    assert_eq!(o.stdout, "1\n", "trap is not supported yet and says so");
}

#[test]
fn scripts_stdin_and_background() {
    let t = Tmp::new();
    fs::write(t.0.join("script.sh"), "echo \"$0: $# args: $1 $2\"\nexit 4\n").unwrap();
    let o = run_args(&t.0, &["script.sh", "a", "b"], None);
    assert_eq!((o.stdout.as_str(), o.code), ("script.sh: 2 args: a b\n", 4));
    let o = run_args(&t.0, &["nonexistent-script"], None);
    assert_eq!(o.code, 127);
    assert!(o.stderr.contains("nonexistent-script"));

    let o = run_args(&t.0, &[], Some("echo from stdin\nx=1\necho $x\n"));
    assert_eq!(o.stdout, "from stdin\n1\n");
    let o = run_args(&t.0, &["-s", "p"], Some("echo $1\n"));
    assert_eq!(o.stdout, "p\n");

    check("sleep 0.2 & wait; echo done", "done\n");
    let o = sh("sleep 0.1 & echo $!");
    assert!(o.stdout.trim().parse::<u32>().is_ok(), "{}", o.stdout);
    check("echo bg > /dev/null & wait; echo ok", "ok\n");
    check("(sleep 0.1; exit 5) & wait $!; echo $?", "5\n");
    // background jobs do not read the script's stdin
    let o = run_args(&t.0, &[], Some("cat & wait\necho end\n"));
    assert_eq!(o.stdout, "end\n");
    let o = run_args(Path::new("/"), &["--version"], None);
    assert!(o.stdout.starts_with("sh (Flint Linux"), "{}", o.stdout);
}

#[test]
fn tricky_but_common() {
    check("yes | head -3", "y\ny\ny\n");
    check("echo a b | while read x y; do echo $y $x; done", "b a\n");
    check("x=0; for i in 1 2 3; do x=$((x+i)); done; echo $x", "6\n");
    check("echo \"$(echo \"nested \\\"quotes\\\"\")\"", "nested \"quotes\"\n");
    check("echo $(echo a; echo b)", "a b\n");
    check("echo first; { echo second; } ; echo third", "first\nsecond\nthird\n");
    check("a=1\nif [ $a = 1 ]\nthen\n  echo one\nfi", "one\n");
    check("for i in 1 2 3\ndo\n  echo $i\ndone", "1\n2\n3\n");
    check("while true; do echo loop; break; done", "loop\n");
    check("echo a\\\nb", "ab\n");
    check("f() { for x in \"$@\"; do echo \"($x)\"; done; }; f 'a b' c", "(a b)\n(c)\n");
    check("cat <<EOF | tr a-z A-Z\nshout\nEOF\n", "SHOUT\n");
    check("echo ${PATH:+has_path}", "has_path\n");
    check("x=a; y=b; echo \"$x$y\" ${x}${y} \"${x}_$y\"", "ab ab a_b\n");
}
