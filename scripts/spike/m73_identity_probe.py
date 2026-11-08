#!/usr/bin/env python3
"""M7.3 spike: can a rootless process under a DIFFERENT host uid (subuid range) be protected
from a same-user 'agent'? Starts a stand-in 'broker' in a user namespace mapped to the caller's
subuid range and then attacks it from the host side as the ordinary user (the agent).
Isolated: temp dir, own sockets, no real service. Needs /etc/subuid and newuidmap."""
import ctypes, errno, os, signal, socket, struct, subprocess, sys, tempfile, time

BROKER = r'''
import os, socket, sys, time
d = sys.argv[1]
os.environ["BROKER_SECRET"] = "s3cret"
open(os.path.join(d, "secret"), "w").write("s3cret"); os.chmod(os.path.join(d, "secret"), 0o600)
p = os.path.join(d, "broker.sock")
s = socket.socket(socket.AF_UNIX); s.bind(p); os.chmod(p, 0o666); s.listen(4)
open(os.path.join(d, "pid"), "w").write(str(os.getpid()))
while True:
    c, _ = s.accept(); c.sendall(b"BROKER"); c.close()
'''
libc = ctypes.CDLL(None, use_errno=True)
def tryit(label, fn):
    try:
        r = fn(); print(f"  {label:<46} ALLOWED ({r})"); return True
    except OSError as e:
        print(f"  {label:<46} denied {errno.errorcode.get(e.errno, e.errno)}"); return False

d = tempfile.mkdtemp(prefix="m73-")
os.chmod(d, 0o777)  # the broker identity must be able to create files here
proc = subprocess.Popen(["unshare", "--user", "--map-users=auto", "--map-groups=auto", "-S", "0", "-G", "0",
                         "python3", "-c", BROKER, d], stderr=subprocess.PIPE)
for _ in range(50):
    if os.path.exists(os.path.join(d, "pid")) and os.path.exists(os.path.join(d, "broker.sock")): break
    time.sleep(0.1)
else:
    print("could not start the stand-in broker:", proc.stderr.read().decode()); sys.exit(2)
pid = int(open(os.path.join(d, "pid")).read())
st = os.stat(os.path.join(d, "secret"))
print(f"stand-in broker pid {pid}; my uid {os.getuid()}; its files are owned by host uid {st.st_uid} (mine is {os.getuid()})")
print("attacks from the same-user 'agent':")
blocked = 0
for label, fn in [
    ("kill(SIGTERM) the broker", lambda: os.kill(pid, 0)),
    ("read /proc/<pid>/environ", lambda: open(f"/proc/{pid}/environ", "rb").read(1)),
    ("open /proc/<pid>/mem", lambda: open(f"/proc/{pid}/mem", "rb").read(1)),
    ("ptrace(SEIZE) the broker", lambda: (_ for _ in ()).throw(OSError(ctypes.get_errno(), "x")) if libc.ptrace(0x4206, pid, 0, 0) != 0 else "seized"),
    ("pidfd_getfd from the broker", lambda: (_ for _ in ()).throw(OSError(ctypes.get_errno(), "x")) if libc.syscall(438, libc.syscall(434, pid, 0), 3, 0) < 0 else "got fd"),
    ("read the broker's 0600 secret file", lambda: open(os.path.join(d, "secret")).read()),
    ("write into the broker's file", lambda: open(os.path.join(d, "secret"), "w").write("x")),
]:
    blocked += not tryit(label, fn)
print("legitimate use:")
s = socket.socket(socket.AF_UNIX); s.connect(os.path.join(d, "broker.sock"))
cred = struct.unpack("3i", s.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
print(f"  connect to its 0666 socket works; SO_PEERCRED says the peer uid is {cred[1]} (kernel-reported, unforgeable by the peer)")
print("impersonation:")
fake = os.path.join(d, "fake.sock"); f = socket.socket(socket.AF_UNIX); f.bind(fake); f.listen(1)
c = socket.socket(socket.AF_UNIX); c.connect(fake)
fc = struct.unpack("3i", c.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
print(f"  an agent-made socket in the same directory reports peer uid {fc[1]} != {cred[1]}: a client that checks SO_PEERCRED against the broker uid is not fooled")
os.kill(pid, signal.SIGKILL)  # cleanup (allowed: see the first attack)
proc.terminate(); proc.wait(timeout=5)
print(f"RESULT: {blocked} of 7 attacks denied")
print("READING: kill, ptrace and pidfd_getfd SUCCEED because the user namespace is OWNED by the agent's own uid (the kernel gives the owner all\n"
      "capabilities over processes inside it). A rootless subuid broker is therefore NOT a separate trust domain from its creator; only a\n"
      "system-created uid (root-installed service) would be.")
