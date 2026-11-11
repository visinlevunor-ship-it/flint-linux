#!/usr/bin/env python3
"""End-to-end test: boot the Flint image in QEMU and drive it over the serial console.

Checks login (good and bad), privilege dropping, the shell's job control, flint-box, su,
passwd, flint-pkg and poweroff. Works on Linux and FreeBSD hosts (software emulation is slow: allow minutes).

  KERNEL=/path/to/bzImage tests/qemu/session.py [initramfs.cpio.gz]

Environment: KERNEL (required), QEMU (default qemu-system-x86_64), ACCEL (e.g. -enable-kvm),
SLOW (timeout multiplier, default 1).
"""
import os, select, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
KERNEL = os.environ.get("KERNEL") or sys.exit("set KERNEL to a Linux x86_64 kernel image")
INITRD = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "flint-initramfs.cpio.gz")
QEMU = os.environ.get("QEMU", "qemu-system-x86_64")
SLOW = float(os.environ.get("SLOW", "1"))


class Q:
    def __init__(self):
        cmd = [QEMU, "-m", "256M", "-kernel", KERNEL, "-initrd", INITRD, "-append", "console=ttyS0 panic=-1 quiet", "-nographic", "-no-reboot"]
        cmd += os.environ.get("ACCEL", "").split()
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        os.set_blocking(self.p.stdout.fileno(), False)
        self.buf, self.pos = "", 0

    def pump(self, timeout=0.2):
        r, _, _ = select.select([self.p.stdout], [], [], timeout)
        if r:
            data = os.read(self.p.stdout.fileno(), 65536)
            if data:
                self.buf += data.decode("utf-8", "replace").replace("\r", "")

    def expect(self, text, timeout=60):
        end = time.time() + timeout * SLOW
        while time.time() < end:
            i = self.buf.find(text, self.pos)
            if i >= 0:
                self.pos = i + len(text)
                return
            self.pump()
            if self.p.poll() is not None:
                self.pump()
                if self.buf.find(text, self.pos) < 0:
                    raise AssertionError(f"qemu exited while waiting for {text!r}")
        raise AssertionError(f"timeout waiting for {text!r}")

    def send(self, s):
        time.sleep(0.15)
        self.p.stdin.write(s.encode())
        self.p.stdin.flush()

    def sh(self, cmd, expect, timeout=60):
        self.send(cmd + "\n")
        self.expect(expect, timeout)

    def login(self, user, password, wait_prompt):
        self.send(user + "\n")
        self.expect("Password: ")
        self.send(password + "\n")
        self.expect("Welcome to Flint Linux", 30)
        self.expect(wait_prompt, 30)

    def wait_exit(self, timeout=120):
        end = time.time() + timeout * SLOW
        while time.time() < end:
            if self.p.poll() is not None:
                self.pump()
                return self.p.returncode
            self.pump()
        raise AssertionError("qemu did not exit")


passed = []


def step(name):
    passed.append(name)
    print(f"ok   {name}", flush=True)


q = Q()
try:
    q.expect("Flint Linux 0.", 120); step("boot, flint-init banner")
    q.expect("login: ", 120); step("login prompt appears")

    q.send("root\n"); q.expect("Password: "); q.send("wrongpassword\n")
    q.expect("Login incorrect", 30); q.expect("login: "); step("wrong password for root is refused")
    q.send("ghost\n"); q.expect("Password: "); q.send("whatever\n")
    q.expect("Login incorrect", 30); q.expect("login: "); step("unknown user is refused")

    q.login("root", "flint", "root@flint:~# "); step("root logs in; flint-sh prompt shows user, host and ~")
    q.sh("echo R-$((6*7))", "R-42"); step("shell arithmetic")
    q.sh("echo B-$(/bin/flint-box --list | wc -l)", "B-30"); step("flint-box has 30 applets")
    q.sh("/bin/id", "uid=0(root) gid=0(root) groups=0(root)"); step("flint-box id (root)")
    q.sh('echo "E-$HOME|$USER|$LOGNAME|$SHELL|$TERM"', "E-/root|root|root|/bin/sh|linux"); step("root environment")
    q.sh("/bin/ls -l /dev/ttyS0", "crw--w----"); step("tty handed to the user (mode 620)")
    q.sh("/bin/ls -l /bin/su /bin/passwd /bin/flint-box", "-rwxr-xr-x"); q.expect("-rwsr-xr-x"); q.expect("-rwsr-xr-x")
    step("flint-box is not setuid; passwd and su are")
    q.sh("/bin/mkdir /mnt && /bin/mount -t tmpfs -o size=1m tmpfs /mnt && /bin/mount", "tmpfs on /mnt type tmpfs (rw,relatime,size=1024k"); step("flint-box mount")
    q.sh("echo data42 > /mnt/f; echo M-$(/bin/cat /mnt/f)", "M-data42"); step("redirect and cat on the mount")
    q.sh("/bin/umount /mnt; echo C-$(grep -c ' /mnt ' /proc/self/mounts)", "C-0"); step("flint-box umount")
    q.sh("/bin/ps", "COMMAND"); q.expect("/init"); step("flint-box ps shows /init as PID 1")
    q.sh("cd /tmp && /bin/mkdir -p a/b && echo x > a/b/f && /bin/cp -r a z && /bin/mv z y && /bin/ls y/b && /bin/rm -r a y; echo L-$(/bin/ls /tmp | /bin/wc -l)", "L-0"); step("mkdir/cp/mv/ls/rm chain")
    q.sh("sh -c 'for i in 1 2 3; do printf \"%s.\" $i; done; echo end'", "1.2.3.end"); step("flint-sh runs scripts (-c)")
    q.sh("f() { echo \"fn:$1\"; }; f ok", "fn:ok"); step("shell functions")

    # job control over the serial tty
    q.send("/bin/sleep 100\n"); time.sleep(1); q.send("\x03"); q.expect("root@flint:", 20)
    q.sh("echo J-$?", "J-130"); step("Ctrl-C ends the foreground job (status 130)")
    q.send("/bin/sleep 100\n"); time.sleep(1); q.send("\x1a"); q.expect("Stopped", 20); q.expect("root@flint:", 20)
    q.sh("echo K-$?", "K-148"); step("Ctrl-Z stops the job")
    q.send("fg\n"); q.expect("sleep 100", 20); time.sleep(1); q.send("\x03"); q.expect("root@flint:", 20)
    q.sh("echo FG-$?", "FG-130"); step("fg resumes it; Ctrl-C ends it")

    q.send("exit\n"); q.expect("login: ", 30); step("exit returns to a fresh login prompt (respawn)")

    q.login("flint", "flint", "flint@flint:~$ "); step("flint logs in; prompt ends in $")
    q.sh("/bin/id", "uid=1000(flint) gid=1000(flint) groups=5(tty),100(users),1000(flint)"); step("privileges dropped, groups set")
    q.sh("echo P-$(pwd)", "P-/home/flint"); step("login lands in the home directory")
    q.sh("echo V-$PATH", "V-/usr/local/bin:/usr/bin:/bin"); step("non-root PATH")
    q.sh("/bin/touch ~/f; echo O-$(stat -c %U ~/f)", "O-flint"); step("home is writable and owned by the user")
    q.sh("/bin/cat /etc/shadow", "Permission denied (os error 13)"); step("shadow is unreadable by a normal user")
    q.sh("flint-pkg install hello", "Permission denied"); step("flint-pkg: a normal user cannot install")

    # su
    q.sh("/bin/su root", "Password: "); q.send("wrongpw\n"); q.expect("Authentication failure", 30)
    q.expect("flint@flint:", 20); step("su refuses a wrong password")
    q.sh("/bin/su root", "Password: "); q.send("flint\n"); q.expect("root@flint:", 30)
    q.sh("echo SU-$(/bin/id -u)", "SU-0"); step("su root with the right password gives a root shell")
    q.send("exit\n"); q.expect("flint@flint:", 20); step("exiting su returns to the original shell")
    q.sh("echo SC-$(/bin/su root -c '/bin/id -u')", "Password: "); q.send("flint\n"); q.expect("SC-0", 30); step("su root -c COMMAND")

    # passwd
    q.sh("/bin/passwd", "Current password: "); q.send("notmypassword\n"); q.expect("incorrect password", 30); step("passwd rejects a wrong current password")
    q.sh("/bin/passwd", "Current password: "); q.send("flint\n")
    q.expect("New password: ", 30); q.send("newpw123\n"); q.expect("Retype new password: "); q.send("newpw123\n")
    q.expect("Password updated", 30); step("passwd changes the password")
    q.send("exit\n"); q.expect("login: ", 30)
    q.send("flint\n"); q.expect("Password: "); q.send("flint\n"); q.expect("Login incorrect", 30); q.expect("login: "); step("old password no longer works")
    q.login("flint", "newpw123", "flint@flint:~$ "); step("new password works")
    q.send("exit\n"); q.expect("login: ", 30)

    q.login("root", "flint", "root@flint:~# ")
    q.sh("echo RS-$(/bin/su flint -c '/bin/id -u')", "RS-1000"); step("root can su to a user without a password")
    q.sh("echo LH-$(/bin/su - flint -c 'echo $HOME:$USER')", "LH-/home/flint:flint"); step("su - gives a login environment")

    # flint-pkg with the example repository built into the image
    q.sh("echo PL-$(flint-pkg list | wc -l)", "PL-0"); step("flint-pkg: nothing installed yet")
    q.sh("flint-pkg search hello", "hello 1.0 - Example program installed with flint-pkg"); step("flint-pkg search")
    q.sh("flint-pkg install hello", "will install: greet-data 1.0, hello 1.0"); q.expect("2 package(s) installed", 60)
    step("flint-pkg install pulls in the dependency")
    q.sh("hello", "Hello from a Flint package!"); q.expect("hello, world"); step("the installed program runs")
    q.sh("echo name=flint > /etc/greet.conf; hello", "hello, flint"); step("its config file is honoured")
    q.sh("flint-pkg verify", "greet-data: modified config: /etc/greet.conf"); step("flint-pkg verify notices the edit")
    q.sh("flint-pkg remove greet-data", "required by hello"); step("flint-pkg refuses to remove a needed package")
    q.sh("flint-pkg remove hello && flint-pkg autoremove", "removing greet-data"); q.expect("kept modified config file /etc/greet.conf")
    step("remove + autoremove, keeping the edited config")
    q.sh("echo H-$(command -v hello || echo gone) C-$(cat /etc/greet.conf)", "H-gone C-name=flint"); step("files are gone, the edited config stays")
    q.send("poweroff\n")
    code = q.wait_exit(120)
    assert code == 0, f"qemu exit code {code}"; step("poweroff from the login session shuts the machine down")
    print(f"\nALL {len(passed)} CHECKS PASSED")
except AssertionError as e:
    print(f"\nFAILED after {len(passed)} checks: {e}")
    print("---- last 40 lines of console ----")
    print("\n".join(q.buf.splitlines()[-40:]))
    q.p.kill()
    sys.exit(1)
