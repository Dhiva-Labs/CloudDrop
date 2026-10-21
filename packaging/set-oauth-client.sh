#!/usr/bin/env bash
# Stores the CloudDrop OAuth clients for ready-to-use builds (PPA, snap, Windows,
# local installs): Google's client, and the OneDrive and Dropbox client ids. The file
# is gitignored, so the public repository never contains it, but it is packed into
# the PPA source package so Launchpad builds include the clients.
#
#   ./packaging/set-oauth-client.sh ~/Downloads/client_secret_….json
#   ./packaging/set-oauth-client.sh --onedrive CLIENT_ID --dropbox APP_KEY
#   ./packaging/set-oauth-client.sh --google CLIENT_ID   (asks for the secret, hidden)
#
# Each run updates what it is given and keeps the rest of the file. --google is for
# Google consoles that show a new client secret once instead of offering the JSON.
set -euo pipefail
out="$(dirname "$0")/oauth-client.env"
[ $# -gt 0 ] || { sed -n '7,9s/^#  //p' "$0"; exit 2; }
umask 077
# --google's secret: asked here, from the terminal (the Python below reads its program
# from stdin), and handed over in the environment, never on the command line.
secret=
for a in "$@"; do
    if [ "$a" = --google ]; then
        read -rsp "Google client secret (not shown): " secret < /dev/tty
        echo
    fi
done
GOOGLE_SECRET=$secret python3 - "$out" "$@" <<'PY'
import json, os, re, sys
out, args = sys.argv[1], sys.argv[2:]
env = {}
if os.path.exists(out):
    for line in open(out):
        k, sep, v = line.strip().partition("=")
        if sep:
            env[k] = v
clients = dict(kv.split("=", 1) for kv in env.get("CLOUDDROP_OAUTH_CLIENTS", "").split(",") if "=" in kv)
said = []
while args:
    a = args.pop(0)
    if a == "--google":
        if not args or not re.fullmatch(r"[0-9]+-[a-z0-9]+\.apps\.googleusercontent\.com", args[0].strip()):
            sys.exit("--google needs your client id: digits-letters.apps.googleusercontent.com, from the console's client page")
        cid = args.pop(0).strip()
        secret = os.environ.get("GOOGLE_SECRET", "").strip()
        if not secret:
            sys.exit("no secret given; nothing saved")
        env["CLOUDDROP_CLIENT_ID"], env["CLOUDDROP_CLIENT_SECRET"] = cid, secret
        said.append(f"Google client {cid.split('-')[0]}…")
    elif a in ("--onedrive", "--dropbox"):
        if not args or not args[0].strip() or args[0].startswith("--"):
            sys.exit(f"{a} needs a value")
        clients[a[2:]] = args.pop(0).strip()
        said.append(f"{a[2:]} client id")
    else:
        data = json.load(open(a))
        c = data.get("installed")
        if not c or not c.get("client_id") or not c.get("client_secret"):
            sys.exit(f"{a}: not a Google 'Desktop app' client JSON file")
        env["CLOUDDROP_CLIENT_ID"], env["CLOUDDROP_CLIENT_SECRET"] = c["client_id"], c["client_secret"]
        said.append(f"Google client {c['client_id'].split('-')[0]}… (project {c.get('project_id')})")
if clients:
    env["CLOUDDROP_OAUTH_CLIENTS"] = ",".join(f"{k}={v}" for k, v in sorted(clients.items()))
with open(out, "w") as f:
    f.writelines(f"{k}={v}\n" for k, v in env.items())
print(f"saved {', '.join(said)} to {out}")
have = ["Google" if env.get("CLOUDDROP_CLIENT_ID") else None, *sorted(clients)]
print("the file now has:", ", ".join(h for h in have if h) or "nothing")
PY
