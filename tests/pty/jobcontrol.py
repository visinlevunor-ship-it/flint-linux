#!/usr/bin/env python3
"""Drive an interactive flint-sh on a real pseudo-terminal: prompt, job control, Ctrl-C/Ctrl-Z.

usage: tests/pty/jobcontrol.py [path/to/flint-sh]     (Linux only; needs coreutils `sleep`)
"""
import os, pty, select, signal, sys, time

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/debug/flint-sh"

class Session:
    def __init__(self):
        pid, fd = pty.fork()
        if pid == 0:
            # the child is a session leader with the pty as its controlling terminal
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm", "HOME": "/tmp", "PS1": "PROMPT> "}
            os.execve(BIN, [BIN], env)
        self.pid, self.fd, self.buf, self.pos = pid, fd, "", 0

    def pump(self, timeout=0.2):
        r, _, _ = select.select([self.fd], [], [], timeout)
        if r:
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return False
            if data:
                self.buf += data.decode("utf-8", "replace").replace("\r", "")
                return True
        return False

    def expect(self, text, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            i = self.buf.find(text, self.pos)
            if i >= 0:
                self.pos = i + len(text)
                return
            self.pump()
        raise AssertionError(f"timeout waiting for {text!r}; got {self.buf[self.pos:]!r}")

    def send(self, s):
        time.sleep(0.1)
        os.write(self.fd, s.encode())

    def line(self, cmd, expect, timeout=10):
        self.send(cmd + "\n")
        self.expect(expect, timeout)

passed = 0
def ok(name):
    global passed
    passed += 1
    print(f"ok   {name}", flush=True)

s = Session()
try:
    s.expect("PROMPT> "); ok("prompt appears (PS1 honoured)")

    s.line("echo hello-$((6*7))", "hello-42"); s.expect("PROMPT> "); ok("commands run")

    s.line("if true; then", "> "); s.line("echo multi", "> "); s.line("fi", "multi"); s.expect("PROMPT> "); ok("multi-line input uses PS2")

    s.line("echo $?", "0\n"); s.expect("PROMPT> "); ok("status of the previous command")

    # Ctrl-C kills the foreground job but not the shell
    s.send("sleep 100\n"); time.sleep(0.5)
    s.send("\x03"); s.expect("PROMPT> ", 5)
    s.line("echo after-ctrl-c-$?", "after-ctrl-c-130"); s.expect("PROMPT> "); ok("Ctrl-C interrupts the foreground job (status 130), shell survives")

    # Ctrl-C at an empty prompt just gives a fresh prompt
    s.send("\x03"); s.expect("PROMPT> ", 5)
    s.line("echo still-here", "still-here"); s.expect("PROMPT> "); ok("Ctrl-C at the prompt gives a fresh prompt")

    # Ctrl-Z stops the job; jobs lists it; fg resumes it; Ctrl-C ends it
    s.send("sleep 100\n"); time.sleep(0.5)
    s.send("\x1a"); s.expect("Stopped", 5); s.expect("PROMPT> ", 5); ok("Ctrl-Z stops the foreground job")
    s.line("echo status-$?", "status-148"); s.expect("PROMPT> "); ok("a stopped job gives status 128+SIGTSTP")
    s.line("jobs", "Stopped"); s.expect("sleep 100"); s.expect("PROMPT> "); ok("jobs lists the stopped job")
    s.send("fg\n"); s.expect("sleep 100", 5); time.sleep(0.5)
    s.send("\x03"); s.expect("PROMPT> ", 5)
    s.line("jobs; echo jobs-done", "jobs-done"); s.expect("PROMPT> "); ok("fg resumes the job; Ctrl-C ends it; the job table is empty")

    # background jobs
    s.line("sleep 0.5 &", "[1]"); s.expect("PROMPT> "); ok("background job is announced")
    s.line("jobs", "Running"); s.expect("PROMPT> "); ok("jobs lists the running job")
    time.sleep(1.0)
    s.line("echo tick", "tick"); s.expect("Done"); s.expect("PROMPT> "); ok("a finished background job is reported before the next prompt")

    # bg resumes a stopped job in the background
    s.send("sleep 1 &\n"); s.expect("PROMPT> ")
    s.send("sleep 100\n"); time.sleep(0.4); s.send("\x1a"); s.expect("Stopped", 5); s.expect("PROMPT> ", 5)
    s.line("bg", "&"); s.expect("PROMPT> ")
    s.line("kill %2; wait %2; echo killed-$?", "killed-143", 10); s.expect("PROMPT> "); ok("bg resumes a job; kill %N and wait %N work")

    # pipelines get one process group; Ctrl-C ends the whole pipeline
    s.send("sleep 100 | sleep 100\n"); time.sleep(0.6)
    s.send("\x03"); s.expect("PROMPT> ", 5)
    s.line("echo pipe-$?", "pipe-130"); s.expect("PROMPT> "); ok("Ctrl-C ends a whole pipeline")

    # the terminal is returned to the shell: reading works afterwards
    s.line("read x; echo got-$x", "")  # no output until we answer
    s.send("typed\n"); s.expect("got-typed"); s.expect("PROMPT> "); ok("builtin read gets terminal input")
    s.line("cat", ""); s.send("to-cat\n"); s.expect("to-cat"); s.send("\x04"); s.expect("PROMPT> "); ok("a foreground program reads the terminal; Ctrl-D ends it")

    # exit
    s.send("exit 3\n")
    _, status = os.waitpid(s.pid, 0)
    assert os.WIFEXITED(status) and os.WEXITSTATUS(status) == 3, status
    ok("exit N exits the shell with that status")
    print(f"\nALL {passed} CHECKS PASSED")
except AssertionError as e:
    print(f"\nFAILED after {passed} checks: {e}")
    print("---- console so far ----\n" + s.buf[-1500:])
    os.kill(s.pid, signal.SIGKILL)
    sys.exit(1)
