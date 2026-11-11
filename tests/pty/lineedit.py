#!/usr/bin/env python3
"""The sh line editor on a real pseudo-terminal: editing keys, history (also across
sessions), Ctrl-R, Tab completion, kill/yank, Alt-., long lines that wrap.

usage: tests/pty/lineedit.py [path/to/flint]     (Linux only)
"""
import fcntl, os, pty, select, struct, sys, tempfile, termios, time

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/debug/flint")
WORK = tempfile.mkdtemp(prefix="flint-lineedit-")
HIST = os.path.join(WORK, "history")

class Session:
    def __init__(self, cols=80):
        pid, fd = pty.fork()
        if pid == 0:
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm", "HOME": WORK, "PS1": "P> ", "HISTFILE": HIST}
            os.chdir(WORK)
            os.execve(BIN, ["sh"], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, cols, 0, 0))
        self.pid, self.fd, self.buf, self.pos = pid, fd, "", 0

    def pump(self, timeout=0.2):
        r, _, _ = select.select([self.fd], [], [], timeout)
        if r:
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return False
            self.buf += data.decode("utf-8", "replace")
            return bool(data)
        return False

    def expect(self, text, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            i = self.buf.find(text, self.pos)
            if i >= 0:
                self.pos = i + len(text)
                return
            self.pump()
        raise AssertionError(f"timeout waiting for {text!r}; got {self.buf[self.pos:][-400:]!r}")

    def send(self, s):
        time.sleep(0.15)
        os.write(self.fd, s.encode() if isinstance(s, str) else s)

    def prompt(self):
        self.expect("P> ")

    def close(self):
        try:
            self.send("exit\r")
            for _ in range(30):
                if os.waitpid(self.pid, os.WNOHANG)[0]:
                    return
                self.pump(0.1)
        except (OSError, ChildProcessError):
            pass

UP, LEFT, HOME, CTRL = "\x1b[A", "\x1b[D", "\x1b[H", lambda c: chr(ord(c) & 0x1F)
passed = 0
def ok(name):
    global passed
    passed += 1
    print(f"ok   {name}", flush=True)

open(os.path.join(WORK, "uniquefilename.txt"), "w").write("file-content-here\n")
os.mkdir(os.path.join(WORK, "somedir"))

s = Session()
try:
    s.prompt()
    s.send("echo hello"); s.send(HOME); s.send(CTRL("f") * 5); s.send("big "); s.send("\r")
    s.expect("big hello"); s.prompt(); ok("cursor movement and insertion in the middle")

    s.send("echo one\r"); s.expect("one"); s.prompt()
    s.send("echo two\r"); s.expect("two"); s.prompt()
    s.send(UP + UP + "\r"); s.expect("\none"); s.prompt(); ok("Up recalls older commands")

    s.send(CTRL("r") + "two" + "\r"); s.expect("\ntwo"); s.prompt(); ok("Ctrl-R finds a command and runs it")

    s.send("cat uniq\t\r"); s.expect("file-content-here"); s.prompt(); ok("Tab completes a file name")
    s.send("cd some\t\r"); s.prompt(); s.send("pwd\r"); s.expect("/somedir"); s.prompt(); ok("Tab completes a directory with a slash")
    s.send("cd ..\r"); s.prompt()

    s.send("ech\t" + "completed\r"); s.expect("\ncompleted"); s.prompt(); ok("Tab completes a command name")

    s.send("garbage words" + CTRL("u") + "echo after-kill" + "\r"); s.expect("\nafter-kill"); s.prompt(); ok("Ctrl-U deletes to the start")
    s.send("echo a bb" + CTRL("w") + "cc\r"); s.expect("\na cc"); s.prompt(); ok("Ctrl-W deletes a word")
    s.send("echo keep" + CTRL("a") + CTRL("k") + CTRL("y") + "\r"); s.expect("\nkeep"); s.prompt(); ok("Ctrl-K kills and Ctrl-Y yanks it back")
    s.send("echo xy" + LEFT + LEFT + "\x1b[3~" + "\r"); s.expect("\ny"); s.prompt(); ok("Delete removes the character under the cursor")
    s.send("echo " + "\x1b." + "\r"); s.expect("\ny"); s.prompt(); ok("Alt-. inserts the last word of the previous command")

    s.send("echo zażółć" + LEFT + LEFT + "X\r"); s.expect("zażóXłć"); s.prompt(); ok("editing UTF-8 text")
    s.send("for i in 1 2\r"); s.expect("> "); s.send("do echo n$i\r"); s.expect("> "); s.send("done\r"); s.expect("n2"); s.prompt(); ok("multi-line commands still work")
    s.send(CTRL("c")); s.prompt(); ok("Ctrl-C gives a fresh prompt")
finally:
    s.close()

hist = open(HIST).read().splitlines()
assert "echo two" in hist and "for i in 1 2; do echo n$i; done" in hist, hist
ok("history file holds the commands (a multi-line one on one line)")

s = Session(cols=30)
try:
    s.prompt()
    s.send(UP + UP + "\r"); s.expect("n2"); s.prompt(); ok("history survives a new session (the last entry is the exit that ended it)")
    long = "echo " + "x" * 70
    s.send(long + HOME + CTRL("f") * 5 + "Y\r"); s.expect("Y" + "x" * 70); s.prompt(); ok("long lines wrap and still edit correctly")
finally:
    s.close()

print(f"\nALL {passed} CHECKS PASSED")
