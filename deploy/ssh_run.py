"""Run a shell script on the VPS over SSH. Usage: python ssh_run.py <script.sh|-c "cmd"> [--timeout N]

Reads host/user/password from env VPS_HOST, VPS_USER (default root), VPS_PASS.
"""
import os
import sys

import paramiko


def main():
    args = sys.argv[1:]
    timeout = 600
    if "--timeout" in args:
        i = args.index("--timeout")
        timeout = int(args[i + 1])
        del args[i:i + 2]
    if args[0] == "-c":
        script = args[1]
    else:
        with open(args[0], encoding="utf-8") as f:
            script = f.read()

    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(
        os.environ["VPS_HOST"],
        port=int(os.environ.get("VPS_PORT", "22")),
        username=os.environ.get("VPS_USER", "root"),
        password=os.environ["VPS_PASS"],
        timeout=15,
        banner_timeout=60, auth_timeout=60,
    )
    stdin, stdout, stderr = client.exec_command("bash -s", timeout=timeout, get_pty=False)
    stdin.write(script.replace("\r\n", "\n"))
    stdin.channel.shutdown_write()
    for line in iter(stdout.readline, ""):
        sys.stdout.write(line)
        sys.stdout.flush()
    err = stderr.read().decode(errors="replace")
    if err:
        sys.stdout.write("[stderr]\n" + err)
    code = stdout.channel.recv_exit_status()
    client.close()
    sys.exit(code)


if __name__ == "__main__":
    main()
